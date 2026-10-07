//! `browser.fill_payment_field`: the one-time payment-input path (ADP-009, DOMAIN.md §13.9 C).
//!
//! The agent never writes a card number, a security code or an address. It names **one field** from a fixed enum
//! ([`PaymentField`]) and the element that is that field on the page it observed; the person has supplied the value out of
//! band, and the runtime inserts it. The order is the design, and every stage that can fail names itself in the fault's
//! `details.stage`:
//!
//! 1. `match` — the live element's `autocomplete` token must be one that identifies the named field. A page that does not label
//!    a field gets no card: the runtime does not guess from a name or a placeholder where payment is concerned.
//! 2. `take` — the runtime takes the person's value, **once**: the value is deleted as it is handed over, so a failure after
//!    this point still spends it (the person supplies it again; a card number is not left waiting for a retry).
//! 3. `origin` — the top frame's and the field's frame's live origin must both be the origin the person supplied the value for.
//! 4. `mask` — a card number, security code or expiry is switched to the browser's masked rendering before anything is
//!    typed, so a frame of the live view or a screenshot taken while it sits in the field shows dots.
//! 5. `insert` — one in-page call that re-checks both origins and focus and inserts, in the same task.
//! 6. `verify` — the page reads the field back and answers only whether it holds what was supplied.
//! 7. the value is dropped and its bytes overwritten, on every path.
//!
//! The journal digest, the ledger arguments, every result and every log hold the placeholder form (`{{payment:<field>}}`),
//! never a value; a result says which field, whether it was masked and verified, and nothing else about the text.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde_json::{json, Value};

use super::{
    digest_of, target_json, ActionKey, BrowserCallContext, BrowserDriver, BrowserFault, Classified,
    Dispatched, FaultCode, Step, Target, ATOMIC_INSERT_FN,
};
use crate::browser::driver::ActionReport;
use crate::browser::origin::origin_allowed;
use crate::browser::secrets::{PaymentField, PaymentRequest};

/// Switch the field to the browser's masked rendering and say whether it took.
const MASK_FN: &str = r#"function () {
  this.style.setProperty("-webkit-text-security", "disc", "important");
  const shown = getComputedStyle(this).getPropertyValue("-webkit-text-security");
  return { ok: this.type === "password" || shown === "disc" };
}"#;

/// Read the field back and answer whether it holds what was supplied; the value is not returned. A page may format what it is
/// given (a card number in groups of four, an expiry as `MM / YY`), so a masked numeric field is compared without spaces, dashes
/// and slashes, and any other field is compared trimmed.
const VERIFY_FN: &str = r#"function (expected, loose) {
  const norm = (text) => (loose ? text.replace(/[\s\-\/]/g, "") : text.trim());
  return { ok: norm(this.value) === norm(expected), length: this.value.length };
}"#;

/// Empty a field a failed verification left wrong. No event is dispatched: a page that rewrote what it was given has an input
/// handler, and running it again on an empty field is the page arguing with the cleanup.
const CLEAR_FN: &str = r#"function () {
  this.value = "";
}"#;

fn stage_fault(
    code: FaultCode,
    stage: &str,
    message: impl Into<String>,
    extra: Value,
) -> BrowserFault {
    let mut details = json!({ "stage": stage });
    if let (Some(base), Some(more)) = (details.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            base.insert(key.clone(), value.clone());
        }
    }
    BrowserFault::new(code, message).with_details(details)
}

impl BrowserDriver {
    /// Insert the person's one-time value into the field the agent named.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] with `details.stage` naming the stage that failed (see the module).
    #[allow(clippy::too_many_lines)]
    pub async fn fill_payment_field(
        &self,
        ctx: &BrowserCallContext,
        tab_id: &str,
        expected_revision: &str,
        target: &Target,
        field: PaymentField,
    ) -> Result<ActionReport, BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);
        // The digest is over the arguments in placeholder form: a payment value never reaches it.
        let digest = digest_of(
            "browser.fill_payment_field",
            &json!({
                "tab_id": tab_id,
                "expected_document_revision": expected_revision,
                "target": target_json(target),
                "field": field.as_str(),
                "value": format!("{{{{payment:{}}}}}", field.as_str()),
            }),
        );
        if let Some(report) = self.replay_gate(&key, &digest)? {
            return Ok(report);
        }
        self.require_control()?;
        let Some(port) = self.inner.secrets.clone() else {
            return Err(stage_fault(
                FaultCode::SecretUnavailable,
                "take",
                "no payment input is connected to this session",
                json!({}),
            ));
        };
        let classified = Some(Classified {
            effect_class: Some("credential.access"),
            tier: Some(4),
            reason: "inserting a supplied payment value materializes a credential".to_string(),
        });
        self.check_tier(ctx, &classified)?;
        let tab = self.tab(Some(tab_id))?;
        let attention = tab.attention.subscribe();
        self.fence_pending(&tab, false)?;
        let resolved = self.resolve_target(&tab, target, expected_revision).await?;
        tab.conn
            .call(
                "DOM.focus",
                json!({ "backendNodeId": resolved.backend_node_id }),
            )
            .await
            .map_err(|_| {
                BrowserFault::new(FaultCode::InvalidRequest, "the element cannot take focus")
            })?;
        let node = self.live_node(&tab.conn, resolved.backend_node_id).await?;
        if let Some(expected) = &resolved.expected {
            if let Some(changed) = expected.difference(&node.fingerprint()) {
                return Err(BrowserFault::new(
                    FaultCode::StalePage,
                    "the element changed while it was being focused; observe the page again",
                )
                .with_details(json!({ "reason": "fingerprint_changed", "field": changed })));
            }
        }
        let editable = node.descriptor.get("editable").and_then(Value::as_bool) == Some(true);
        let focused = node.descriptor.get("focused").and_then(Value::as_bool) == Some(true);
        if !editable || !focused {
            return Err(stage_fault(
                FaultCode::InvalidRequest,
                "match",
                "the target is not a text field that can receive input",
                json!({}),
            ));
        }

        // 1. match: the page must say this is that field.
        let declared = node
            .descriptor
            .get("autocomplete")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let found: Vec<&str> = declared.split_whitespace().take(8).collect();
        if !field.tokens().iter().any(|token| found.contains(token)) {
            return Err(stage_fault(
                FaultCode::InvalidRequest,
                "match",
                format!(
                    "the field is not labelled as {}: its autocomplete does not say so, and a card field is never guessed from a name",
                    field.as_str()
                ),
                json!({
                    "field": field.as_str(),
                    "expected_tokens": field.tokens(),
                    "found_tokens": found.iter().map(|t| t.chars().take(32).collect::<String>()).collect::<Vec<_>>(),
                }),
            ));
        }
        self.check_revision(&tab, expected_revision)?;

        // 2. take: the person's value, once. From here a failure spends it.
        let payment = port
            .take_payment(PaymentRequest {
                field,
                call_id: ctx.tool_call_id.clone(),
                action_id: ctx.action_id.clone(),
            })
            .await
            .map_err(|fault| {
                stage_fault(
                    fault.code,
                    "take",
                    fault.message.clone(),
                    json!({ "field": field.as_str() }),
                )
            })?;

        // 3. origin: both live origins are the one the value was supplied for.
        let frame_origin = node
            .descriptor
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("null")
            .to_string();
        self.check_revision(&tab, expected_revision)?;
        let top_origin = self.top_origin(&tab).await?;
        for (which, origin) in [("top", &top_origin), ("target_frame", &frame_origin)] {
            if !origin_allowed(origin, std::slice::from_ref(&payment.allowed_origin)) {
                return Err(stage_fault(
                    FaultCode::SecretOriginMismatch,
                    "origin",
                    format!("the value was not supplied for the {which} origin {origin}"),
                    json!({ "which": which, "origin": origin, "field": field.as_str() }),
                ));
            }
        }

        // 4. mask: before anything is typed.
        let mut masked = false;
        if field.masked() {
            let verdict = tab
                .conn
                .call(
                    "Runtime.callFunctionOn",
                    json!({
                        "objectId": node.object_id,
                        "functionDeclaration": MASK_FN,
                        "returnByValue": true,
                    }),
                )
                .await?;
            masked = verdict.pointer("/result/value/ok").and_then(Value::as_bool) == Some(true);
            if !masked {
                return Err(stage_fault(
                    FaultCode::PageFailure,
                    "mask",
                    "the field could not be masked, so nothing was typed",
                    json!({ "field": field.as_str() }),
                ));
            }
        }

        // 5. insert, atomically.
        self.inner.journal.start(&key, &digest)?;
        let sent = Arc::new(AtomicBool::new(false));
        let within = self.inner.config.action_timeout;
        let steps = vec![Step {
            method: "Runtime.callFunctionOn",
            params: json!({
                "objectId": node.object_id,
                "functionDeclaration": ATOMIC_INSERT_FN,
                "arguments": [
                    { "value": frame_origin },
                    { "value": top_origin },
                    { "value": true },
                    { "value": payment.value.expose() },
                ],
                "returnByValue": true,
            }),
            marks_sent: true,
            within,
        }];
        let handle = self.spawn_steps(&tab, &sent, steps);
        let dispatched = self.await_dispatch(&tab, handle, attention, &sent).await;
        if let Dispatched::Done(result) = &dispatched {
            let verdict = result.pointer("/result/value");
            if verdict.and_then(|v| v.get("ok")).and_then(Value::as_bool) != Some(true) {
                let why = verdict
                    .and_then(|v| v.get("why"))
                    .and_then(Value::as_str)
                    .unwrap_or("insert_refused");
                let fault = match why {
                    "frame_origin" | "top_origin" => stage_fault(
                        FaultCode::SecretOriginMismatch,
                        "insert",
                        format!(
                            "the {why} changed before the value was inserted; nothing was typed"
                        ),
                        json!({ "which": why }),
                    ),
                    "focus" => stage_fault(
                        FaultCode::StalePage,
                        "insert",
                        "focus left the field before the value was inserted; nothing was typed",
                        json!({ "reason": "focus_lost" }),
                    ),
                    _ => stage_fault(
                        FaultCode::PageFailure,
                        "insert",
                        "the page refused the insertion; nothing was typed",
                        json!({}),
                    ),
                };
                tab.state().invalidate_observations();
                return Err(self.finish_fault(&key, fault));
            }

            // 6. verify: the page answers whether the field holds what was supplied, and nothing else.
            let check = tab
                .conn
                .call(
                    "Runtime.callFunctionOn",
                    json!({
                        "objectId": node.object_id,
                        "functionDeclaration": VERIFY_FN,
                        "arguments": [
                            { "value": payment.value.expose() },
                            { "value": field.masked() },
                        ],
                        "returnByValue": true,
                    }),
                )
                .await?;
            if check.pointer("/result/value/ok").and_then(Value::as_bool) != Some(true) {
                let _ = tab
                    .conn
                    .call(
                        "Runtime.callFunctionOn",
                        json!({
                            "objectId": node.object_id,
                            "functionDeclaration": CLEAR_FN,
                            "returnByValue": true,
                        }),
                    )
                    .await;
                tab.state().invalidate_observations();
                return Err(self.finish_fault(
                    &key,
                    stage_fault(
                        FaultCode::PageFailure,
                        "verify",
                        "the field does not hold what was supplied; it was emptied",
                        json!({ "field": field.as_str() }),
                    ),
                ));
            }
        }
        // 7. delete: the value goes out of scope here and its bytes are overwritten.
        drop(payment);

        let output = json!({
            "filled": matches!(dispatched, Dispatched::Done(_)),
            "field": field.as_str(),
            "stage": "done",
            "masked": masked,
            "verified": matches!(dispatched, Dispatched::Done(_)),
        });
        self.conclude(
            &tab,
            &key,
            classified,
            dispatched,
            output,
            resolved.warnings,
        )
        .await
    }
}
