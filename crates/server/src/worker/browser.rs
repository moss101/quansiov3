//! `browser.*` calls on a worker's browser (EXEC-009 composition over ADP-001).
//!
//! The model cannot know a browser session's id, and it must not be able to name one: a session belongs to the
//! run that opened it. So the host binds every call to **its own run's session** on the worker's target —
//! opening it on the run's first call — and overwrites whatever `session_id` the model wrote. A call can only
//! ever act on the session of the run it belongs to.
//!
//! Two decisions stay here because only the server knows them: whether the agent still holds the session (a
//! person's takeover fences agent input before anything is sent), and the tier the runtime approved for the call,
//! which the driver compares to what a click would settle. What the driver says comes back as one of three
//! things — a result, a typed error with its own `retryable`, or *in doubt* — and is settled as exactly that.

use std::sync::Arc;

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_machine::control::browser::{BrowserSession, BrowserSessionStore, SessionStatus};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::runtime::turn_loop::{HostFailure, HostOutcome};

/// The browser tools a worker serves. `browser.fill_payment_field` and `browser.import_session` need a person's
/// out-of-band value and a signed receipt the worker does not receive yet, so they are refused, not guessed at.
pub const BROWSER_TOOLS: [&str; 8] = [
    "browser.navigate",
    "browser.observe",
    "browser.click",
    "browser.type",
    "browser.press_key",
    "browser.handle_dialog",
    "browser.extract",
    "browser.screenshot",
];

/// Opens and finds the session of a run.
#[derive(Debug, Clone, Default)]
pub struct BrowserSessions {
    /// Two parallel calls of one run must not both open a session.
    open: Arc<Mutex<()>>,
}

impl BrowserSessions {
    /// The run's session on `target_id`, opened if the run has none yet.
    ///
    /// # Errors
    /// Returns the reason a session could not be found or opened.
    pub async fn for_run(
        &self,
        pool: &PgPool,
        tenant_id: &str,
        target_id: &str,
        run_id: &str,
    ) -> Result<BrowserSession, String> {
        let _only_one = self.open.lock().await;
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        crate::control::schema::set_tenant_context(&mut tx, tenant_id)
            .await
            .map_err(|e| e.to_string())?;
        let existing = BrowserSessionStore::list_for_target(&mut tx, tenant_id, target_id)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|session| {
                session.run_id.as_deref() == Some(run_id) && session.status != SessionStatus::Closed
            });
        let session = match existing {
            Some(session) => session,
            None => {
                let id = CanonicalId::generate(Prefix::BrowserSession, &mut UlidGenerator::new())
                    .to_string();
                BrowserSessionStore::open(&mut tx, tenant_id, &id, target_id, Some(run_id), None)
                    .await
                    .map_err(|e| e.to_string())?
            }
        };
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(session)
    }
}

/// The tier the runtime approves for a tool: the consequence tier of the effect class it declares.
#[must_use]
pub fn approved_tier(tool: &str) -> Option<u8> {
    let declaration = quansio_tools::ToolRegistry::builtin().get(tool)?;
    // Every browser tool declares its class; a class derived from the arguments states no ceiling here.
    let quansio_tools::EffectClassRule::Static(class) = &declaration.effect_class else {
        return None;
    };
    crate::effects::EffectTaxonomy::builtin()
        .get(class.as_str())
        .map(|entry| entry.tier.get())
}

/// Refuse a call while the agent does not hold the session.
///
/// # Errors
/// Returns the refusal when a person holds the session (a takeover) or it is paused or closed.
pub fn require_agent_control(session: &BrowserSession) -> Result<(), HostFailure> {
    if session.agent_may_drive() {
        Ok(())
    } else {
        Err(HostFailure::Refused {
            detail: format!(
                "BROWSER_INPUT_FENCED: browser session {} is {} and held by {}; the agent's input is fenced until it is handed back",
                session.id,
                session.status.as_str(),
                session.control_holder.as_str()
            ),
        })
    }
}

/// Settle the worker's reply document as exactly what the driver said it was.
///
/// A reply that is not a document the driver could have produced is an unknown outcome: the call may have
/// reached the page, and nothing here can establish that it did not.
#[must_use]
pub fn settle(output: &str) -> HostOutcome {
    let unknown = |detail: String| HostOutcome::Unknown { detail };
    let Ok(document) = serde_json::from_str::<Value>(output) else {
        return unknown("the worker's browser reply was not a document".to_string());
    };
    match document.get("status").and_then(Value::as_str) {
        Some("ok") => HostOutcome::Success {
            output: document.get("output").cloned().unwrap_or(Value::Null),
            evidence_ids: Vec::new(),
            remote_ref: None,
        },
        Some("error") => {
            let error = document
                .get("error")
                .cloned()
                .unwrap_or_else(|| json!({ "code": "TOOL_FAILED" }));
            HostOutcome::Failure {
                retryable: error
                    .get("retryable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                error,
                evidence_ids: Vec::new(),
            }
        }
        Some("in_doubt") => unknown(document.get("output").map_or_else(
            || "the browser could not establish the outcome".to_string(),
            Value::to_string,
        )),
        _ => unknown("the worker's browser reply carried no status".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driver_reply_settles_as_exactly_what_it_said_it_was() {
        let ok =
            settle(r#"{"status":"ok","output":{"url":"https://example.com/"},"replayed":false}"#);
        assert!(
            matches!(ok, HostOutcome::Success { ref output, .. } if output["url"] == "https://example.com/"),
            "{ok:?}"
        );

        let error = settle(
            r#"{"status":"error","error":{"code":"BROWSER_STALE_PAGE","retryable":true,"requires_new_observation":true}}"#,
        );
        assert!(
            matches!(error, HostOutcome::Failure { retryable: true, ref error, .. } if error["code"] == "BROWSER_STALE_PAGE"),
            "{error:?}"
        );
        let fatal = settle(
            r#"{"status":"error","error":{"code":"BROWSER_NAVIGATION_DENIED","retryable":false}}"#,
        );
        assert!(matches!(
            fatal,
            HostOutcome::Failure {
                retryable: false,
                ..
            }
        ));
    }

    #[test]
    fn an_outcome_the_driver_could_not_establish_is_unknown_and_so_is_anything_that_is_not_its_reply(
    ) {
        assert!(matches!(
            settle(
                r#"{"status":"in_doubt","output":{"reason":"the connection was lost after input"}}"#
            ),
            HostOutcome::Unknown { .. }
        ));
        // Garbage, a missing status and a status nobody defined are never read as success.
        for reply in [
            "not json",
            "{}",
            r#"{"status":"done"}"#,
            r#"{"output":{"ok":true}}"#,
            "",
        ] {
            assert!(
                matches!(settle(reply), HostOutcome::Unknown { .. }),
                "{reply:?} must be unknown"
            );
        }
    }

    #[test]
    fn only_a_static_effect_class_states_a_ceiling() {
        assert_eq!(approved_tier("browser.click"), Some(3));
        assert_eq!(approved_tier("no.such.tool"), None);
    }
}
