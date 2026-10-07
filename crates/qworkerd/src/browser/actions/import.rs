//! `browser.import_session`: apply the cookies a person staged to the managed browser (ADP-009, DOMAIN.md §13.9 B).
//!
//! Host-only: no model is offered this tool. A person stages a bundle (`ImportBrowserSession`), the runtime asks for the approval
//! (effect class `browser.session.import`, tier 4, always asked, so the call needs an approved ceiling of 4), and then this runs.
//! The arguments the approval was given are `{import_id, digest, cookie_count, domains}` and never a value; the order below is the
//! contract and each stage that fails names itself in the fault's `details.stage`:
//!
//! 1. `tier` — the call carries an approved ceiling that covers tier 4.
//! 2. `take` — the runtime hands over the staged bundle **once** and overwrites it as it does; it refuses a bundle whose digest is
//!    not the one approved, and a bundle that was already used, expired or staged for another workspace.
//! 3. `verify` — the count of cookies is the count that was approved.
//! 4. `apply` — `Storage.setCookies` on the session's browser context, a batch at a time, falling back to one cookie at a time
//!    when a batch is refused.
//! 5. `check` — the browser is asked which of the bundle's `(domain, path, name)` it now holds; the answer reports that count as
//!    `applied` (a browser may accept a call and drop a cookie it dislikes), and the rest as `rejected`. Values are never read back.
//! 6. the values are zeroed on every path.
//!
//! The journal digest, the ledger arguments, every result and every log hold the bundle's digest and counts, never a cookie value.

use serde_json::{json, Value};

use super::{digest_of, ActionKey, BrowserCallContext, BrowserDriver, Classified};
use crate::browser::driver::ActionReport;
use crate::browser::fault::{BrowserFault, FaultCode};
use crate::browser::secrets::ImportRequest;
use quansio_machine::secrets::imports::{ImportCookie, SameSite};

/// Cookies sent to the browser in one `Storage.setCookies` call.
const BATCH: usize = 200;

fn stage_fault(code: FaultCode, stage: &str, message: impl Into<String>) -> BrowserFault {
    BrowserFault::new(code, message).with_details(json!({ "stage": stage }))
}

/// The CDP form of a cookie. `domain` keeps its leading dot, which is what makes a domain cookie of a host cookie.
fn cdp_cookie(cookie: &ImportCookie) -> Value {
    let mut value = json!({
        "name": cookie.name,
        "value": cookie.value,
        "domain": cookie.domain,
        "path": cookie.path,
        "secure": cookie.secure,
        "httpOnly": cookie.http_only,
    });
    match cookie.same_site {
        SameSite::None => value["sameSite"] = json!("None"),
        SameSite::Lax => value["sameSite"] = json!("Lax"),
        SameSite::Strict => value["sameSite"] = json!("Strict"),
        SameSite::Unspecified => {}
    }
    if let Some(expires) = cookie.expires {
        value["expires"] = json!(expires);
    }
    value
}

impl BrowserDriver {
    /// Apply the approved, staged cookie bundle.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] with `details.stage` naming the stage that failed (see the module).
    pub async fn import_session(
        &self,
        ctx: &BrowserCallContext,
        import_id: &str,
        digest: &str,
        cookie_count: usize,
    ) -> Result<ActionReport, BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);
        let call_digest = digest_of(
            "browser.import_session",
            &json!({ "import_id": import_id, "digest": digest, "cookie_count": cookie_count }),
        );
        if let Some(report) = self.replay_gate(&key, &call_digest)? {
            return Ok(report);
        }
        let Some(port) = self.inner.secrets.clone() else {
            return Err(stage_fault(
                FaultCode::SecretUnavailable,
                "take",
                "no browser-data import is connected to this session",
            ));
        };
        // 1. tier: this settles as browser.session.import, tier 4, and the runtime must have approved a ceiling that covers it.
        let classified = Some(Classified {
            effect_class: Some("browser.session.import"),
            tier: Some(4),
            reason: "applying another browser's cookies gives this browser that person's signed-in sessions".to_string(),
        });
        self.check_tier(ctx, &classified)
            .map_err(|fault| stage_fault(fault.code, "tier", fault.message))?;
        if ctx.approved_tier.is_none() {
            return Err(stage_fault(
                FaultCode::EffectTierExceeded,
                "tier",
                "an import runs only under an approved tier; none was stated",
            ));
        }
        let tab = self.tab(None)?;

        // 2. take: once. From here a failure spends the bundle.
        let bundle = port
            .take_import(ImportRequest {
                import_id: import_id.to_string(),
                digest: digest.to_string(),
                call_id: ctx.tool_call_id.clone(),
                action_id: ctx.action_id.clone(),
            })
            .await
            .map_err(|fault| stage_fault(fault.code, "take", fault.message.clone()))?;

        // 3. verify: the bundle is the size that was approved.
        if bundle.cookies.len() != cookie_count {
            return Err(self.finish_fault(
                &key,
                stage_fault(
                    FaultCode::InvalidRequest,
                    "verify",
                    "the staged bundle does not hold the number of cookies that was approved; nothing was applied",
                ),
            ));
        }

        // 4. apply. A browser accepts a call and may still drop a cookie it dislikes (SameSite=None without Secure, a name prefix
        // it does not honour), so the count of cookies *held* afterwards is what is reported, never the count sent.
        self.inner.journal.start(&key, &call_digest)?;
        for chunk in bundle.cookies.chunks(BATCH) {
            let cookies: Vec<Value> = chunk.iter().map(cdp_cookie).collect();
            match tab
                .conn
                .call("Storage.setCookies", json!({ "cookies": cookies }))
                .await
            {
                Ok(_) => {}
                Err(super::CdpError::Closed | super::CdpError::Transport(_)) => {
                    return self.finish_in_doubt(
                        &key,
                        "the browser failed while cookies were being applied; some may have been",
                    );
                }
                Err(_) => {
                    // A batch the browser refuses whole: try each cookie alone, so one bad cookie does not cost the others.
                    for cookie in chunk {
                        match tab
                            .conn
                            .call(
                                "Storage.setCookies",
                                json!({ "cookies": [cdp_cookie(cookie)] }),
                            )
                            .await
                        {
                            Ok(_) | Err(super::CdpError::Protocol { .. }) => {}
                            Err(_) => {
                                return self.finish_in_doubt(
                                    &key,
                                    "the browser failed while cookies were being applied; some may have been",
                                );
                            }
                        }
                    }
                }
            }
        }

        // 5. check: which of the bundle's cookies does the browser now hold? Names and places only.
        let held = tab.conn.call("Storage.getCookies", json!({})).await?;
        let present: std::collections::BTreeSet<(String, String, String)> = held
            .get("cookies")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|cookie| {
                        Some((
                            cookie.get("domain")?.as_str()?.to_string(),
                            cookie.get("path")?.as_str()?.to_string(),
                            cookie.get("name")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let verified = bundle
            .cookies
            .iter()
            .filter(|cookie| {
                present.contains(&(
                    cookie.domain.clone(),
                    cookie.path.clone(),
                    cookie.name.clone(),
                ))
            })
            .count();

        // 6. the bundle goes out of scope here and its values are overwritten.
        drop(bundle);
        let mut report = ActionReport::of(json!({
            "imported": verified == cookie_count,
            "cookie_count": cookie_count,
            "applied": verified,
            "rejected": cookie_count - verified,
            "digest": digest,
        }));
        report.classified = classified;
        self.finish_ok(&key, report)
    }
}
