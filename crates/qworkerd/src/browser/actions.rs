//! The mutating actions of the hardened driver (ADP-001).
//!
//! Every action here follows one order, and the order is the design:
//!
//! 1. **parse and validate** the request — a malformed placeholder, a missing target, a destination that
//!    is not `http`/`https`/`about:blank` — before anything is looked up;
//! 2. take the session's **action queue**, so two calls never interleave their input;
//! 3. consult the **journal**: a completed key returns its stored result, a key in doubt returns the
//!    in-doubt result, a key reused with different arguments is a conflict. None of these touches the
//!    page, which is why they come before the page's state is consulted — a redelivered click must not
//!    fail as *stale* merely because the first delivery worked;
//! 4. **fence**: agent input while a user holds the session, a dialog or certificate error pending;
//! 5. **verify the target** against the live page (revision, snapshot or capture, fingerprint, hit
//!    test), then **classify** what the action would settle and check it against the approved tier;
//! 6. write the journal's **start** line, and only then dispatch the first input event;
//! 7. **conclude**: invalidate what the model last observed, wait for the page to settle, and record the
//!    outcome — or, when the connection fails after an event was sent, record that the outcome is *in
//!    doubt* and say so. An in-doubt action is never retried by anything in this crate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use quansio_core::Digest;
use quansio_machine::control::browser::{Decision, DecisionKind, DecisionReceipt};
use quansio_tools::canonical_json;
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::cdp::CdpError;
use super::driver::{
    classify_node, ActionReport, BrowserCallContext, BrowserDriver, Resolved, Tab,
};
use super::fault::{BrowserFault, FaultCode};
use super::fence::Target;
use super::journal::{ActionKey, Begin};
use super::origin::{is_external_handoff, origin_allowed, origin_of, validate_navigation};
use super::secrets::{
    has_placeholders, only_placeholders, parse_placeholders, sensitivity_of, SecretRequest, Segment,
};
use super::tab::{CertificateStatus, PendingDialog};
use super::{classify_input, classify_navigation, Classified};

mod import;
mod payment;

/// One CDP call of an action.
struct Step {
    method: &'static str,
    params: Value,
    /// Set the action's `sent` flag before this call is made: from here on, a failure means the page
    /// may have received the input.
    marks_sent: bool,
    within: Duration,
}

/// How a dispatched action ended.
enum Dispatched {
    Done(Value),
    Interrupted(Interrupt),
    Failed { sent: bool, error: CdpError },
}

/// What the page raised while an action was in flight.
enum Interrupt {
    Dialog(PendingDialog),
    Certificate(super::tab::CertificateRecord),
}

/// The classified effect classes a stored result can name again.
fn static_class(class: &str) -> Option<&'static str> {
    [
        "payment.execute",
        "browser.session.import",
        "record.delete",
        "credential.access",
        "message.send",
        "content.upload",
        "record.create",
        "network.egress.new_destination",
        "read.external",
    ]
    .into_iter()
    .find(|known| *known == class)
}

fn classified_json(classified: &Option<Classified>) -> Value {
    match classified {
        Some(classified) => json!({
            "effect_class": classified.effect_class,
            "tier": classified.tier,
            "reason": classified.reason,
        }),
        None => Value::Null,
    }
}

fn digest_of(tool: &str, args: &Value) -> String {
    Digest::of_canonical_json(&canonical_json(&json!({ "tool": tool, "args": args })))
        .as_str()
        .to_string()
}

fn target_json(target: &Target) -> Value {
    match target {
        Target::Ref {
            snapshot_id,
            reference,
        } => json!({ "snapshot_id": snapshot_id, "ref": reference }),
        Target::Point { capture_id, x, y } => json!({ "capture_id": capture_id, "x": x, "y": y }),
    }
}

fn in_doubt_output(key: &ActionKey, reason: &str) -> Value {
    json!({
        "in_doubt": true,
        "action_id": key.action_id,
        "reason": reason,
        "retryable": false,
        "requires_new_observation": true,
        "guidance": "the action may or may not have happened; observe the page again and decide from what it shows. Do not repeat it unless the observation shows it did not happen.",
    })
}

impl BrowserDriver {
    // ------------------------------------------------------------------ journal plumbing

    /// Step 3: answer a replay, an in-doubt key or a reconciled key without touching the page.
    fn replay_gate(
        &self,
        key: &ActionKey,
        digest: &str,
    ) -> Result<Option<ActionReport>, BrowserFault> {
        Ok(match self.inner.journal.check(key, digest)? {
            Begin::Fresh => None,
            Begin::Replay(stored) => {
                if stored.get("ok").and_then(Value::as_bool) == Some(false) {
                    let fault = stored.get("fault").cloned().unwrap_or(Value::Null);
                    let code = fault
                        .get("code")
                        .and_then(Value::as_str)
                        .and_then(FaultCode::parse)
                        .unwrap_or(FaultCode::PageFailure);
                    return Err(BrowserFault::new(
                        code,
                        fault
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("the earlier attempt of this action failed")
                            .to_string(),
                    )
                    .with_details(fault.get("details").cloned().unwrap_or(Value::Null)));
                }
                let mut report =
                    ActionReport::of(stored.get("output").cloned().unwrap_or(Value::Null));
                report.classified = stored
                    .pointer("/classified/effect_class")
                    .and_then(Value::as_str)
                    .and_then(static_class)
                    .map(|class| Classified {
                        effect_class: Some(class),
                        tier: stored
                            .pointer("/classified/tier")
                            .and_then(Value::as_u64)
                            .and_then(|tier| u8::try_from(tier).ok()),
                        reason: stored
                            .pointer("/classified/reason")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    });
                report.replayed = true;
                Some(report)
            }
            Begin::InDoubt { reason } => {
                let mut report = ActionReport::of(in_doubt_output(key, &reason));
                report.in_doubt = Some(reason);
                report.replayed = true;
                Some(report)
            }
            Begin::Reconciled { outcome } => {
                let mut report = ActionReport::of(json!({
                    "reconciled": true,
                    "action_id": key.action_id,
                    "outcome": outcome,
                }));
                report.replayed = true;
                Some(report)
            }
        })
    }

    fn finish_ok(
        &self,
        key: &ActionKey,
        mut report: ActionReport,
    ) -> Result<ActionReport, BrowserFault> {
        self.inner.journal.complete(
            key,
            &json!({
                "ok": true,
                "output": report.output,
                "classified": classified_json(&report.classified),
            }),
        )?;
        report.replayed = false;
        Ok(report)
    }

    fn finish_fault(&self, key: &ActionKey, fault: BrowserFault) -> BrowserFault {
        let _ = self.inner.journal.complete(
            key,
            &json!({ "ok": false, "fault": {
                "code": fault.code.as_str(),
                "message": fault.message,
                "details": fault.details,
            } }),
        );
        fault
    }

    fn finish_in_doubt(&self, key: &ActionKey, reason: &str) -> Result<ActionReport, BrowserFault> {
        self.inner.journal.mark_in_doubt(key, reason)?;
        let mut report = ActionReport::of(in_doubt_output(key, reason));
        report.in_doubt = Some(reason.to_string());
        Ok(report)
    }

    /// Close an in-doubt action after the runtime reconciled it from a fresh observation.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the action is not in doubt.
    pub fn reconcile(&self, ctx: &BrowserCallContext, outcome: &str) -> Result<(), BrowserFault> {
        self.inner
            .journal
            .reconcile(&ActionKey::new(&ctx.tool_call_id, &ctx.action_id), outcome)
    }

    /// Step 5's second half: the classified consequence against what the runtime approved.
    fn check_tier(
        &self,
        ctx: &BrowserCallContext,
        classified: &Option<Classified>,
    ) -> Result<(), BrowserFault> {
        if let (
            Some(ceiling),
            Some(Classified {
                tier: Some(tier),
                effect_class: Some(class),
                ..
            }),
        ) = (ctx.approved_tier, classified)
        {
            if *tier > ceiling {
                return Err(BrowserFault::new(
                    FaultCode::EffectTierExceeded,
                    format!(
                        "the action would settle {class} (tier {tier}), above the approved tier {ceiling}"
                    ),
                )
                .with_details(json!({ "effect_class": class, "tier": tier, "approved_tier": ceiling })));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ dispatch plumbing

    fn spawn_steps(
        &self,
        tab: &Arc<Tab>,
        sent: &Arc<AtomicBool>,
        steps: Vec<Step>,
    ) -> JoinHandle<Result<Value, CdpError>> {
        let tab = Arc::clone(tab);
        let sent = Arc::clone(sent);
        tokio::spawn(async move {
            let mut last = Value::Null;
            for step in steps {
                if step.marks_sent {
                    sent.store(true, Ordering::SeqCst);
                }
                last = tab
                    .conn
                    .call_within(step.method, step.params, step.within)
                    .await?;
            }
            Ok(last)
        })
    }

    async fn await_dispatch(
        &self,
        tab: &Arc<Tab>,
        mut handle: JoinHandle<Result<Value, CdpError>>,
        mut attention: watch::Receiver<u64>,
        sent: &Arc<AtomicBool>,
    ) -> Dispatched {
        loop {
            tokio::select! {
                joined = &mut handle => {
                    return match joined {
                        Ok(Ok(value)) => Dispatched::Done(value),
                        Ok(Err(error)) => Dispatched::Failed { sent: sent.load(Ordering::SeqCst), error },
                        Err(join) => Dispatched::Failed {
                            sent: sent.load(Ordering::SeqCst),
                            error: CdpError::Transport(join.to_string()),
                        },
                    };
                }
                _ = attention.changed() => {
                    let state = tab.state();
                    if let Some(dialog) = &state.dialog {
                        return Dispatched::Interrupted(Interrupt::Dialog(dialog.clone()));
                    }
                    if let Some(certificate) = state.pending_certificates().first() {
                        return Dispatched::Interrupted(Interrupt::Certificate((*certificate).clone()));
                    }
                    if state.closed {
                        return Dispatched::Failed {
                            sent: sent.load(Ordering::SeqCst),
                            error: CdpError::Closed,
                        };
                    }
                }
            }
        }
    }

    /// Wait for the page to finish what the action started. Bounded: a page that never settles is
    /// reported as still loading rather than waited on.
    async fn wait_settled(&self, tab: &Tab) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let deadline = tokio::time::Instant::now() + self.inner.config.settle_timeout;
        loop {
            {
                let state = tab.state();
                if !state.is_loading() || state.dialog.is_some() {
                    break;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Step 7: record how a dispatched action ended.
    async fn conclude(
        &self,
        tab: &Arc<Tab>,
        key: &ActionKey,
        classified: Option<Classified>,
        dispatched: Dispatched,
        mut output: Value,
        warnings: Vec<String>,
    ) -> Result<ActionReport, BrowserFault> {
        match dispatched {
            Dispatched::Done(_) => {
                tab.state().invalidate_observations();
                self.wait_settled(tab).await;
                let (revision, loading, url) = {
                    let state = tab.state();
                    (
                        state.revision.clone(),
                        state.is_loading(),
                        state.url.clone(),
                    )
                };
                output["completed"] = json!(true);
                output["tab_id"] = json!(tab.id);
                output["document_revision"] = json!(revision);
                output["url"] = json!(url);
                output["loading"] = json!(loading);
                output["input_origin"] = json!("agent");
                if !warnings.is_empty() {
                    output["warnings"] = json!(warnings);
                }
                let mut report = ActionReport::of(output);
                report.classified = classified;
                self.finish_ok(key, report)
            }
            Dispatched::Interrupted(interrupt) => {
                tab.state().invalidate_observations();
                output["completed"] = json!(true);
                output["tab_id"] = json!(tab.id);
                output["input_origin"] = json!("agent");
                match interrupt {
                    Interrupt::Dialog(dialog) => output["dialog_pending"] = dialog.to_json(),
                    Interrupt::Certificate(certificate) => {
                        output["certificate_decision_required"] = certificate.to_json();
                    }
                }
                if !warnings.is_empty() {
                    output["warnings"] = json!(warnings);
                }
                let mut report = ActionReport::of(output);
                report.classified = classified;
                self.finish_ok(key, report)
            }
            Dispatched::Failed { sent: true, error } => {
                tab.state().invalidate_observations();
                let reason = format!(
                    "input was sent to the page and the browser then failed ({error}); the page may have received it"
                );
                self.finish_in_doubt(key, &reason)
            }
            Dispatched::Failed { sent: false, error } => Err(self.finish_fault(
                key,
                BrowserFault::new(
                    FaultCode::PageFailure,
                    format!("the browser failed before any input was sent: {error}"),
                ),
            )),
        }
    }

    async fn top_origin(&self, tab: &Tab) -> Result<String, BrowserFault> {
        let result = tab
            .conn
            .call(
                "Runtime.evaluate",
                json!({ "expression": "location.origin", "returnByValue": true }),
            )
            .await?;
        Ok(result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or("null")
            .to_string())
    }

    // ------------------------------------------------------------------ navigate

    /// Navigate a tab, or open a new one.
    ///
    /// The destination must be `http`, `https` or `about:blank` with no credentials in it. A page that
    /// answers with a certificate error does not load: the action reports the error, which only a user
    /// can decide.
    ///
    /// # Errors
    /// Returns [`BrowserFault`]; see the module's order of checks.
    pub async fn navigate(
        &self,
        ctx: &BrowserCallContext,
        tab_id: Option<&str>,
        url: &str,
        expected_revision: Option<&str>,
    ) -> Result<ActionReport, BrowserFault> {
        validate_navigation(url)?;
        if tab_id.is_some() && expected_revision.is_none() {
            return Err(BrowserFault::new(
                FaultCode::InvalidRequest,
                "navigating an existing tab needs expected_document_revision",
            ));
        }
        let _turn = self.inner.queue.lock().await;
        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);
        let digest = digest_of(
            "browser.navigate",
            &json!({ "tab_id": tab_id, "url": url, "expected_document_revision": expected_revision }),
        );
        if let Some(report) = self.replay_gate(&key, &digest)? {
            return Ok(report);
        }
        self.require_control()?;
        let classified = origin_of(url).map(|origin| {
            let (scheme, host) = origin.split_once("://").unwrap_or(("http", ""));
            classify_navigation(scheme, host)
        });
        self.check_tier(ctx, &classified)?;
        let tab = match tab_id {
            Some(id) => self.tab(Some(id))?,
            None => {
                let id = self.open_tab().await?;
                self.tab(Some(&id))?
            }
        };
        let attention = tab.attention.subscribe();
        self.fence_pending(&tab, false)?;
        if let Some(expected) = expected_revision {
            self.check_revision(&tab, expected)?;
        }
        self.inner.journal.start(&key, &digest)?;
        let sent = Arc::new(AtomicBool::new(false));
        let handle = self.spawn_steps(
            &tab,
            &sent,
            vec![Step {
                method: "Page.navigate",
                params: json!({ "url": url }),
                marks_sent: true,
                within: self.inner.config.navigation_timeout,
            }],
        );
        let dispatched = self.await_dispatch(&tab, handle, attention, &sent).await;
        if let Dispatched::Done(result) = &dispatched {
            if let Some(error_text) = result.get("errorText").and_then(Value::as_str) {
                return Err(self.finish_fault(
                    &key,
                    BrowserFault::new(
                        FaultCode::PageFailure,
                        format!("the navigation failed: {error_text}"),
                    )
                    .with_details(json!({ "error_text": error_text })),
                ));
            }
        }
        // A navigation stopped by a certificate error did not load anything, and the model must know
        // that: it is a refusal with the pending decision attached, not a completed navigation.
        if let Dispatched::Interrupted(Interrupt::Certificate(certificate)) = &dispatched {
            tab.state().invalidate_observations();
            return Err(self.finish_fault(
                &key,
                BrowserFault::new(
                    FaultCode::CertificateDecisionRequired,
                    "the destination's certificate was not accepted; only a user can decide whether to continue",
                )
                .with_details(json!({ "certificate": certificate.to_json() })),
            ));
        }
        let output = json!({ "navigated": matches!(dispatched, Dispatched::Done(_)) });
        self.conclude(&tab, &key, classified, dispatched, output, Vec::new())
            .await
    }

    // ------------------------------------------------------------------ click

    /// Click the target the model observed.
    ///
    /// # Errors
    /// Returns [`BrowserFault`]: stale revision, snapshot, capture or fingerprint; a pending dialog or
    /// certificate error; a consequence above the approved tier; a link that would hand off to another
    /// application, which agent input cannot do.
    pub async fn click(
        &self,
        ctx: &BrowserCallContext,
        tab_id: &str,
        expected_revision: &str,
        target: &Target,
    ) -> Result<ActionReport, BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);
        let digest = digest_of(
            "browser.click",
            &json!({
                "tab_id": tab_id,
                "expected_document_revision": expected_revision,
                "target": target_json(target),
            }),
        );
        if let Some(report) = self.replay_gate(&key, &digest)? {
            return Ok(report);
        }
        self.require_control()?;
        let tab = self.tab(Some(tab_id))?;
        let attention = tab.attention.subscribe();
        self.fence_pending(&tab, false)?;
        let resolved = self.resolve_target(&tab, target, expected_revision).await?;
        let classified = Some(classify_node(&resolved.node)).filter(Classified::is_consequential);
        self.check_tier(ctx, &classified)?;
        self.refuse_handoff(&resolved)?;
        self.inner.journal.start(&key, &digest)?;
        let sent = Arc::new(AtomicBool::new(false));
        let within = self.inner.config.action_timeout;
        let (x, y) = (resolved.x, resolved.y);
        let handle = self.spawn_steps(
            &tab,
            &sent,
            vec![
                Step {
                    method: "Input.dispatchMouseEvent",
                    params: json!({ "type": "mouseMoved", "x": x, "y": y }),
                    marks_sent: false,
                    within,
                },
                Step {
                    method: "Input.dispatchMouseEvent",
                    params: json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 }),
                    marks_sent: true,
                    within,
                },
                Step {
                    method: "Input.dispatchMouseEvent",
                    params: json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1 }),
                    marks_sent: true,
                    within,
                },
            ],
        );
        let dispatched = self.await_dispatch(&tab, handle, attention, &sent).await;
        let output = json!({
            "clicked": true,
            "hit": resolved.hit,
            "classified": classified_json(&classified),
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

    /// Agent input is not a user gesture, so it cannot hand a link off to another application.
    fn refuse_handoff(&self, resolved: &Resolved) -> Result<(), BrowserFault> {
        if let Some(href) = resolved.node.descriptor.get("href").and_then(Value::as_str) {
            if is_external_handoff(href) {
                let scheme = href.split(':').next().unwrap_or_default();
                return Err(BrowserFault::new(
                    FaultCode::GestureRequired,
                    format!(
                        "the link hands off to another application ({scheme}:); that needs a genuine user gesture and agent input is not one"
                    ),
                )
                .with_details(json!({ "scheme": scheme })));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ type

    /// Type text into a field, resolving `{{secret:<name>.<field>}}` placeholders at insertion.
    ///
    /// Plain text into a password, one-time-code or card field is refused. A placeholder resolves only
    /// through the runtime's secret port, and only after the top frame's *and* the target frame's live
    /// origin are checked against the origins the secret was issued for — once on the way in, and again
    /// inside the page in the same task as the insertion.
    ///
    /// # Errors
    /// Returns [`BrowserFault`]; see the module's order of checks, plus
    /// [`FaultCode::SensitiveInputDenied`], [`FaultCode::SecretOriginMismatch`] and
    /// [`FaultCode::SecretUnavailable`].
    #[allow(clippy::too_many_lines)]
    pub async fn type_text(
        &self,
        ctx: &BrowserCallContext,
        tab_id: &str,
        expected_revision: &str,
        target: &Target,
        text: &str,
        clear: bool,
    ) -> Result<ActionReport, BrowserFault> {
        let segments = parse_placeholders(text)?;
        let uses_secrets = has_placeholders(&segments);
        let _turn = self.inner.queue.lock().await;
        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);
        // The digest is over the arguments in placeholder form: a secret value never reaches it.
        let digest = digest_of(
            "browser.type",
            &json!({
                "tab_id": tab_id,
                "expected_document_revision": expected_revision,
                "target": target_json(target),
                "text": text,
                "clear": clear,
            }),
        );
        if let Some(report) = self.replay_gate(&key, &digest)? {
            return Ok(report);
        }
        self.require_control()?;
        if uses_secrets && self.inner.secrets.is_none() {
            return Err(BrowserFault::new(
                FaultCode::SecretUnavailable,
                "no secret broker is connected to this session",
            ));
        }
        let classified = if uses_secrets {
            Some(Classified {
                effect_class: Some("credential.access"),
                tier: Some(4),
                reason: "typing a secret materializes a credential".to_string(),
            })
        } else {
            let none = classify_input();
            none.is_consequential().then_some(none)
        };
        self.check_tier(ctx, &classified)?;
        let tab = self.tab(Some(tab_id))?;
        let attention = tab.attention.subscribe();
        self.fence_pending(&tab, false)?;
        let resolved = self.resolve_target(&tab, target, expected_revision).await?;

        // Focus, then look at what actually has focus: the target may have been a label, a wrapper or a
        // different element than the point suggested, and sensitivity is a property of the receiver.
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
            if let Some(field) = expected.difference(&node.fingerprint()) {
                return Err(BrowserFault::new(
                    FaultCode::StalePage,
                    "the element changed while it was being focused; observe the page again",
                )
                .with_details(json!({ "reason": "fingerprint_changed", "field": field })));
            }
        }
        let editable = node.descriptor.get("editable").and_then(Value::as_bool) == Some(true);
        let focused = node.descriptor.get("focused").and_then(Value::as_bool) == Some(true);
        if !editable || !focused {
            return Err(BrowserFault::new(
                FaultCode::InvalidRequest,
                "the target is not a text field that can receive input",
            ));
        }
        let sensitive = sensitivity_of(&node.descriptor);
        if let Some(kind) = sensitive {
            if !(uses_secrets && only_placeholders(&segments)) {
                return Err(BrowserFault::new(
                    FaultCode::SensitiveInputDenied,
                    format!(
                        "this is a {} field; it accepts only a {{{{secret:<name>.<field>}}}} placeholder, never plain text",
                        kind.as_str()
                    ),
                )
                .with_details(json!({ "kind": kind.as_str() })));
            }
        }
        self.check_revision(&tab, expected_revision)?;

        // Resolve the placeholders first: resolving materializes a credential, which can take time, and
        // the page may move while it does. The live origins are read *after* it, as late as the
        // protocol allows, and compared with what each secret was issued for.
        let mut resolved_secrets = Vec::new();
        for segment in &segments {
            if let Segment::Secret { name, field } = segment {
                let port = self.inner.secrets.as_ref().ok_or_else(|| {
                    BrowserFault::new(FaultCode::SecretUnavailable, "no secret broker")
                })?;
                let resolved_secret = port
                    .resolve(SecretRequest {
                        name: name.clone(),
                        field: field.clone(),
                        call_id: ctx.tool_call_id.clone(),
                        action_id: ctx.action_id.clone(),
                    })
                    .await?;
                resolved_secrets.push((name.clone(), field.clone(), resolved_secret));
            }
        }
        let frame_origin = node
            .descriptor
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("null")
            .to_string();
        let top_origin = if uses_secrets {
            // The page may have navigated while the secret was being resolved: the revision says so.
            self.check_revision(&tab, expected_revision)?;
            self.top_origin(&tab).await?
        } else {
            String::new()
        };
        let mut final_text = String::new();
        let mut used = Vec::new();
        let mut next_secret = resolved_secrets.iter();
        for segment in &segments {
            match segment {
                Segment::Literal(literal) => final_text.push_str(literal),
                Segment::Secret { .. } => {
                    let Some((name, field, resolved_secret)) = next_secret.next() else {
                        continue;
                    };
                    for (which, origin) in [("top", &top_origin), ("target_frame", &frame_origin)] {
                        if !origin_allowed(origin, &resolved_secret.allowed_origins) {
                            return Err(BrowserFault::new(
                                FaultCode::SecretOriginMismatch,
                                format!(
                                    "secret {name}.{field} was not issued for the {which} origin {origin}"
                                ),
                            )
                            .with_details(json!({
                                "secret": format!("{name}.{field}"),
                                "which": which,
                                "origin": origin,
                            })));
                        }
                    }
                    final_text.push_str(resolved_secret.value.expose());
                    used.push(json!({ "name": name, "field": field }));
                }
            }
        }

        self.inner.journal.start(&key, &digest)?;
        let sent = Arc::new(AtomicBool::new(false));
        let within = self.inner.config.action_timeout;
        let steps = if uses_secrets {
            vec![Step {
                method: "Runtime.callFunctionOn",
                params: json!({
                    "objectId": node.object_id,
                    "functionDeclaration": ATOMIC_INSERT_FN,
                    "arguments": [
                        { "value": frame_origin },
                        { "value": top_origin },
                        { "value": clear },
                        { "value": final_text },
                    ],
                    "returnByValue": true,
                }),
                marks_sent: true,
                within,
            }]
        } else {
            let mut steps = Vec::new();
            if clear {
                steps.push(Step {
                    method: "Runtime.callFunctionOn",
                    params: json!({
                        "objectId": node.object_id,
                        "functionDeclaration": "function () { if (this.select) { this.select(); } else { document.execCommand('selectAll'); } }",
                        "returnByValue": true,
                    }),
                    marks_sent: true,
                    within,
                });
            }
            steps.push(Step {
                method: "Input.insertText",
                params: json!({ "text": final_text }),
                marks_sent: true,
                within,
            });
            steps
        };
        let handle = self.spawn_steps(&tab, &sent, steps);
        let dispatched = self.await_dispatch(&tab, handle, attention, &sent).await;
        if uses_secrets {
            if let Dispatched::Done(result) = &dispatched {
                let verdict = result.pointer("/result/value");
                if verdict.and_then(|v| v.get("ok")).and_then(Value::as_bool) != Some(true) {
                    let why = verdict
                        .and_then(|v| v.get("why"))
                        .and_then(Value::as_str)
                        .unwrap_or("insert_refused");
                    let fault = match why {
                        "frame_origin" | "top_origin" => BrowserFault::new(
                            FaultCode::SecretOriginMismatch,
                            format!("the {why} changed before the secret was inserted; nothing was typed"),
                        )
                        .with_details(json!({ "which": why })),
                        "focus" => BrowserFault::new(
                            FaultCode::StalePage,
                            "focus left the field before the secret was inserted; nothing was typed",
                        )
                        .with_details(json!({ "reason": "focus_lost" })),
                        _ => BrowserFault::new(
                            FaultCode::PageFailure,
                            "the page refused the insertion; nothing was typed",
                        ),
                    };
                    tab.state().invalidate_observations();
                    return Err(self.finish_fault(&key, fault));
                }
            }
        }
        let mut output = json!({
            "typed": matches!(dispatched, Dispatched::Done(_)),
            "used_secrets": used,
        });
        if let Some(kind) = sensitive {
            output["sensitive_field"] = json!(kind.as_str());
        }
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

    // ------------------------------------------------------------------ press_key

    /// Press a key. A printable key is refused while focus is in a sensitive field, or in a frame whose
    /// focus the driver cannot see into.
    ///
    /// # Errors
    /// Returns [`BrowserFault`]; see the module's order of checks, plus
    /// [`FaultCode::SensitiveInputDenied`].
    pub async fn press_key(
        &self,
        ctx: &BrowserCallContext,
        tab_id: &str,
        expected_revision: &str,
        key_name: &str,
    ) -> Result<ActionReport, BrowserFault> {
        let spec = KeySpec::parse(key_name)?;
        let _turn = self.inner.queue.lock().await;
        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);
        let digest = digest_of(
            "browser.press_key",
            &json!({
                "tab_id": tab_id,
                "expected_document_revision": expected_revision,
                "key": key_name,
            }),
        );
        if let Some(report) = self.replay_gate(&key, &digest)? {
            return Ok(report);
        }
        self.require_control()?;
        let tab = self.tab(Some(tab_id))?;
        let attention = tab.attention.subscribe();
        self.fence_pending(&tab, false)?;
        self.check_revision(&tab, expected_revision)?;
        if spec.printable {
            let focus = tab
                .conn
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": ACTIVE_ELEMENT_JS, "returnByValue": true }),
                )
                .await?;
            let descriptor = focus
                .pointer("/result/value")
                .cloned()
                .unwrap_or(Value::Null);
            let cross_origin =
                descriptor.get("cross_origin").and_then(Value::as_bool) == Some(true);
            if cross_origin || sensitivity_of(&descriptor).is_some() {
                let kind = sensitivity_of(&descriptor).map(super::secrets::SensitiveKind::as_str);
                return Err(BrowserFault::new(
                    FaultCode::SensitiveInputDenied,
                    "a printable key is refused while focus is in a sensitive field or a frame the driver cannot read; use a secret placeholder with browser.type",
                )
                .with_details(json!({ "kind": kind, "cross_origin_focus": cross_origin })));
            }
        }
        self.inner.journal.start(&key, &digest)?;
        let sent = Arc::new(AtomicBool::new(false));
        let within = self.inner.config.action_timeout;
        let mut down = json!({
            "type": if spec.text.is_some() && spec.modifiers & 0b111 == 0 { "keyDown" } else { "rawKeyDown" },
            "key": spec.key,
            "code": spec.code,
            "windowsVirtualKeyCode": spec.virtual_key,
            "modifiers": spec.modifiers,
        });
        if let Some(text) = &spec.text {
            down["text"] = json!(text);
        }
        let up = json!({
            "type": "keyUp",
            "key": spec.key,
            "code": spec.code,
            "windowsVirtualKeyCode": spec.virtual_key,
            "modifiers": spec.modifiers,
        });
        let handle = self.spawn_steps(
            &tab,
            &sent,
            vec![
                Step {
                    method: "Input.dispatchKeyEvent",
                    params: down,
                    marks_sent: true,
                    within,
                },
                Step {
                    method: "Input.dispatchKeyEvent",
                    params: up,
                    marks_sent: true,
                    within,
                },
            ],
        );
        let dispatched = self.await_dispatch(&tab, handle, attention, &sent).await;
        self.conclude(
            &tab,
            &key,
            None,
            dispatched,
            json!({ "pressed": key_name }),
            Vec::new(),
        )
        .await
    }

    // ------------------------------------------------------------------ dialogs

    /// Answer the pending dialog. It must be quoted by its exact id and the revision it was raised
    /// under; anything else is [`FaultCode::StaleDialog`].
    ///
    /// # Errors
    /// Returns [`BrowserFault`].
    pub async fn handle_dialog(
        &self,
        ctx: &BrowserCallContext,
        tab_id: &str,
        expected_revision: &str,
        dialog_id: &str,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<ActionReport, BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        let key = ActionKey::new(&ctx.tool_call_id, &ctx.action_id);
        let digest = digest_of(
            "browser.handle_dialog",
            &json!({
                "tab_id": tab_id,
                "expected_document_revision": expected_revision,
                "dialog_id": dialog_id,
                "accept": accept,
                "prompt_text": prompt_text,
            }),
        );
        if let Some(report) = self.replay_gate(&key, &digest)? {
            return Ok(report);
        }
        self.require_control()?;
        let tab = self.tab(Some(tab_id))?;
        {
            let state = tab.state();
            let pending = state.dialog.as_ref();
            let stale_dialog = |reason: &str| {
                BrowserFault::new(
                    FaultCode::StaleDialog,
                    "that is not the dialog that is pending; read the page's dialog state again",
                )
                .with_details(json!({
                    "reason": reason,
                    "pending": pending.map(PendingDialog::to_json),
                }))
            };
            match pending {
                None => return Err(stale_dialog("no_dialog_pending")),
                Some(dialog) if dialog.id != dialog_id => return Err(stale_dialog("dialog_id")),
                Some(dialog) if dialog.revision != expected_revision => {
                    return Err(stale_dialog("revision"))
                }
                Some(_) => {}
            }
        }
        self.fence_pending(&tab, true)?;
        self.inner.journal.start(&key, &digest)?;
        let sent = Arc::new(AtomicBool::new(false));
        let mut params = json!({ "accept": accept });
        if let Some(text) = prompt_text {
            params["promptText"] = json!(text);
        }
        let handle = self.spawn_steps(
            &tab,
            &sent,
            vec![Step {
                method: "Page.handleJavaScriptDialog",
                params,
                marks_sent: true,
                within: self.inner.config.action_timeout,
            }],
        );
        let joined = handle.await;
        let dispatched = match joined {
            Ok(Ok(value)) => Dispatched::Done(value),
            Ok(Err(error)) => Dispatched::Failed { sent: true, error },
            Err(join) => Dispatched::Failed {
                sent: true,
                error: CdpError::Transport(join.to_string()),
            },
        };
        // The browser reports the dialog closed as an event; wait for it so the next action is not
        // refused for a dialog that is already gone.
        for _ in 0..80 {
            if tab.state().dialog.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let output = json!({ "handled": true, "dialog_id": dialog_id, "accepted": accept });
        self.conclude(&tab, &key, None, dispatched, output, Vec::new())
            .await
    }

    // ------------------------------------------------------------------ user-only decisions

    /// Act on a user's decision about a certificate error.
    ///
    /// The only input is a [`DecisionReceipt`], which the control plane mints for a person and for no one
    /// else; the agent's tool surface has no way to name one. Continuing is not remembered by the driver:
    /// the browser keeps the exception for that certificate, and the record keeps the certificate's hash
    /// once the browser reveals it.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the receipt is not for a certificate on this tab, or nothing is
    /// pending.
    pub async fn resolve_certificate(
        &self,
        tab_id: &str,
        receipt: &DecisionReceipt,
    ) -> Result<ActionReport, BrowserFault> {
        let invalid = |why: &str| BrowserFault::new(FaultCode::InvalidRequest, why.to_string());
        if receipt.kind() != DecisionKind::Certificate
            || receipt.session_id() != self.session_id()
            || receipt.tab_id() != tab_id
        {
            return Err(invalid(
                "the receipt is not a certificate decision for this tab",
            ));
        }
        let tab = self.tab(Some(tab_id))?;
        let (event_id, origin) = {
            let state = tab.state();
            let record = state
                .certificate(receipt.subject())
                .filter(|record| record.status == CertificateStatus::Pending)
                .ok_or_else(|| invalid("no such certificate error is waiting for a decision"))?;
            (record.event_id, record.origin.clone())
        };
        let action = match receipt.decision() {
            Decision::Allow => "continue",
            Decision::Deny => "cancel",
        };
        {
            let mut state = tab.state();
            if let Some(record) = state
                .certificates
                .iter_mut()
                .find(|record| record.id == receipt.subject())
            {
                record.status = match receipt.decision() {
                    Decision::Allow => CertificateStatus::Continued,
                    Decision::Deny => CertificateStatus::Cancelled,
                };
                record.decided_by = Some(receipt.decided_by().to_string());
            }
        }
        tab.conn
            .call(
                "Security.handleCertificateError",
                json!({ "eventId": event_id, "action": action }),
            )
            .await?;
        tab.ping();
        // The navigation the error interrupted proceeds (or is cancelled); let it settle.
        self.wait_settled(&tab).await;
        Ok(ActionReport::of(json!({
            "resolved": true,
            "certificate_id": receipt.subject(),
            "origin": origin,
            "decision": receipt.decision().as_str(),
            "decided_by": receipt.decided_by(),
        })))
    }

    /// Act on a user's decision about a media permission.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the receipt is not a permission decision for this origin and
    /// permission, or the permission is not one that can be granted.
    pub async fn apply_permission_decision(
        &self,
        tab_id: &str,
        receipt: &DecisionReceipt,
    ) -> Result<ActionReport, BrowserFault> {
        let invalid = |why: &str| BrowserFault::new(FaultCode::InvalidRequest, why.to_string());
        if receipt.kind() != DecisionKind::Permission
            || receipt.session_id() != self.session_id()
            || receipt.tab_id() != tab_id
        {
            return Err(invalid(
                "the receipt is not a permission decision for this tab",
            ));
        }
        let (origin, permission) = receipt
            .subject()
            .split_once('|')
            .ok_or_else(|| invalid("the receipt's subject is not <origin>|<permission>"))?;
        if !quansio_machine::control::browser::GRANTABLE_PERMISSIONS.contains(&permission) {
            return Err(invalid("that permission can never be granted"));
        }
        let tab = self.tab(Some(tab_id))?;
        let setting = match receipt.decision() {
            Decision::Allow => "granted",
            Decision::Deny => "denied",
        };
        tab.conn
            .call(
                "Browser.setPermission",
                json!({
                    "permission": { "name": permission },
                    "setting": setting,
                    "origin": origin,
                }),
            )
            .await?;
        Ok(ActionReport::of(json!({
            "applied": true,
            "origin": origin,
            "permission": permission,
            "setting": setting,
            "decided_by": receipt.decided_by(),
        })))
    }

    // ------------------------------------------------------------------ user input (takeover)

    /// A click the user made while they hold the session. It is the one kind of input that counts as a
    /// genuine gesture: popups and application handoffs it causes are allowed.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] unless a user holds the session.
    pub async fn user_click(&self, tab_id: &str, x: f64, y: f64) -> Result<(), BrowserFault> {
        self.user_press(tab_id, x, y, "left", 1, &[]).await
    }

    /// A click the user made: a button, the press's click count (1 to 3) and modifier keys held.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] for a point, button, count or modifier a person may not send, or unless a user holds the
    /// session.
    pub async fn user_press(
        &self,
        tab_id: &str,
        x: f64,
        y: f64,
        button: &str,
        click_count: u8,
        modifiers: &[String],
    ) -> Result<(), BrowserFault> {
        use quansio_machine::gateway::wire::{
            user_modifiers_allowed, user_point_allowed, USER_BUTTONS,
        };
        let invalid =
            |detail: &str| BrowserFault::new(FaultCode::InvalidRequest, detail.to_string());
        if !user_point_allowed(x) || !user_point_allowed(y) {
            return Err(invalid("a click is at a point from 0 to 20000"));
        }
        if !USER_BUTTONS.contains(&button) {
            return Err(invalid("a click is with the left, middle or right button"));
        }
        if !(1..=3).contains(&click_count) {
            return Err(invalid("a click is a single, double or triple click"));
        }
        if !user_modifiers_allowed(modifiers) {
            return Err(invalid(
                "modifiers are Alt, Control, Meta or Shift, each once",
            ));
        }
        let mask = modifier_mask(modifiers);
        let _turn = self.inner.queue.lock().await;
        self.require_user_control()?;
        let tab = self.tab(Some(tab_id))?;
        tab.state().last_user_gesture = Some(std::time::Instant::now());
        let mut events = vec![json!({ "type": "mouseMoved", "x": x, "y": y, "modifiers": mask })];
        // One press and release. The count is the press's own: the second click of a double click arrives as a click of
        // count 2, after the first (count 1) was delivered, exactly as the browser itself sends them.
        for kind in ["mousePressed", "mouseReleased"] {
            events.push(json!({
                "type": kind, "x": x, "y": y, "button": button, "clickCount": click_count, "modifiers": mask,
            }));
        }
        for event in events {
            tab.conn
                .call_within(
                    "Input.dispatchMouseEvent",
                    event,
                    self.inner.config.action_timeout,
                )
                .await?;
        }
        tab.state().invalidate_observations();
        // A popup or a handoff the click caused is judged by the follow-up tasks; give them a moment.
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    /// A drag the user made: the left button down at one point, moved in steps, and up at another.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] for a point or modifier a person may not send, or unless a user holds the session.
    pub async fn user_drag(
        &self,
        tab_id: &str,
        from: (f64, f64),
        to: (f64, f64),
        modifiers: &[String],
    ) -> Result<(), BrowserFault> {
        use quansio_machine::gateway::wire::{user_modifiers_allowed, user_point_allowed};
        let invalid =
            |detail: &str| BrowserFault::new(FaultCode::InvalidRequest, detail.to_string());
        if ![from.0, from.1, to.0, to.1]
            .into_iter()
            .all(user_point_allowed)
        {
            return Err(invalid("a drag is between points from 0 to 20000"));
        }
        if !user_modifiers_allowed(modifiers) {
            return Err(invalid(
                "modifiers are Alt, Control, Meta or Shift, each once",
            ));
        }
        let mask = modifier_mask(modifiers);
        let _turn = self.inner.queue.lock().await;
        self.require_user_control()?;
        let tab = self.tab(Some(tab_id))?;
        tab.state().last_user_gesture = Some(std::time::Instant::now());
        const STEPS: u32 = 8;
        let mut events = vec![
            json!({ "type": "mouseMoved", "x": from.0, "y": from.1, "modifiers": mask }),
            json!({ "type": "mousePressed", "x": from.0, "y": from.1, "button": "left", "buttons": 1, "clickCount": 1, "modifiers": mask }),
        ];
        for step in 1..=STEPS {
            let along = f64::from(step) / f64::from(STEPS);
            events.push(json!({
                "type": "mouseMoved",
                "x": from.0 + (to.0 - from.0) * along,
                "y": from.1 + (to.1 - from.1) * along,
                "button": "left", "buttons": 1, "modifiers": mask,
            }));
        }
        events.push(json!({ "type": "mouseReleased", "x": to.0, "y": to.1, "button": "left", "buttons": 0, "clickCount": 1, "modifiers": mask }));
        for event in events {
            tab.conn
                .call_within(
                    "Input.dispatchMouseEvent",
                    event,
                    self.inner.config.action_timeout,
                )
                .await?;
        }
        tab.state().invalidate_observations();
        Ok(())
    }

    /// Text the user typed while they hold the session.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] unless a user holds the session.
    pub async fn user_text(&self, tab_id: &str, text: &str) -> Result<(), BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        self.require_user_control()?;
        let tab = self.tab(Some(tab_id))?;
        tab.state().last_user_gesture = Some(std::time::Instant::now());
        tab.conn
            .call_within(
                "Input.insertText",
                json!({ "text": text }),
                self.inner.config.action_timeout,
            )
            .await?;
        tab.state().invalidate_observations();
        Ok(())
    }

    /// A key the user pressed while they hold the session, perhaps with modifiers: a named key of
    /// [`quansio_machine::gateway::wire::USER_KEYS`], or a letter or digit held with `Alt`, `Control` or `Meta`
    /// ([`quansio_machine::gateway::wire::user_key_allowed`]). Other printable characters are text.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] for any other key, or unless a user holds the session.
    pub async fn user_key(&self, tab_id: &str, key_name: &str) -> Result<(), BrowserFault> {
        if !quansio_machine::gateway::wire::user_key_allowed(key_name) {
            return Err(BrowserFault::new(
                FaultCode::InvalidRequest,
                format!("{key_name:?} is not a key a person can send"),
            ));
        }
        let spec = KeySpec::parse(key_name)?;
        let _turn = self.inner.queue.lock().await;
        self.require_user_control()?;
        let tab = self.tab(Some(tab_id))?;
        tab.state().last_user_gesture = Some(std::time::Instant::now());
        let mut down = json!({
            "type": if spec.text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": spec.key, "code": spec.code, "windowsVirtualKeyCode": spec.virtual_key, "modifiers": spec.modifiers,
        });
        // A chord is a shortcut, not typed text: the character is sent as text only when nothing but Shift is held.
        if let Some(text) = spec.text.as_ref().filter(|_| spec.modifiers & 0b111 == 0) {
            down["text"] = json!(text);
        }
        let up = json!({
            "type": "keyUp",
            "key": spec.key, "code": spec.code, "windowsVirtualKeyCode": spec.virtual_key, "modifiers": spec.modifiers,
        });
        for event in [down, up] {
            tab.conn
                .call_within(
                    "Input.dispatchKeyEvent",
                    event,
                    self.inner.config.action_timeout,
                )
                .await?;
        }
        tab.state().invalidate_observations();
        Ok(())
    }

    /// A scroll the user made while they hold the session.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] for a point or delta that is not finite or is out of bounds, or unless a user holds the session.
    pub async fn user_scroll(
        &self,
        tab_id: &str,
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<(), BrowserFault> {
        let limit = quansio_machine::gateway::wire::MAX_USER_SCROLL;
        let point = |value: f64| value.is_finite() && (0.0..=20_000.0).contains(&value);
        let delta = |value: f64| value.is_finite() && value.abs() <= limit;
        if !point(x) || !point(y) || !delta(delta_x) || !delta(delta_y) {
            return Err(BrowserFault::new(
                FaultCode::InvalidRequest,
                "a scroll is at a point from 0 to 20000 and by at most 10000 pixels",
            ));
        }
        let _turn = self.inner.queue.lock().await;
        self.require_user_control()?;
        let tab = self.tab(Some(tab_id))?;
        tab.state().last_user_gesture = Some(std::time::Instant::now());
        tab.conn
            .call_within(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x, "deltaY": delta_y }),
                self.inner.config.action_timeout,
            )
            .await?;
        tab.state().invalidate_observations();
        Ok(())
    }

    fn require_user_control(&self) -> Result<(), BrowserFault> {
        let session = self.session();
        if matches!(session.control_holder, super::ControlHolder::User) {
            Ok(())
        } else {
            Err(BrowserFault::new(
                FaultCode::InvalidRequest,
                "user input is accepted only while the user holds the session",
            ))
        }
    }

    /// Whether the user's gesture is recent enough to count (test and surface support).
    #[must_use]
    pub fn user_gesture_recent(&self, tab_id: &str) -> bool {
        self.tab(Some(tab_id)).is_ok_and(|tab| {
            tab.state()
                .user_gesture_within(self.inner.config.gesture_window)
        })
    }

    /// Where an in-flight session's tabs are, for the surface (`url`, `revision`).
    #[must_use]
    pub fn tab_summary(&self, tab_id: &str) -> Option<(String, String)> {
        let tab = self.tab(Some(tab_id)).ok()?;
        let state = tab.state();
        Some((state.url.clone(), state.revision.clone()))
    }
}

/// The insertion, checked and done in one task of the target frame's own event loop: the frame's origin
/// and its top-level ancestor's origin are compared with what the runtime resolved the secret for, the
/// element must still hold focus, and only then is the text inserted.
const ATOMIC_INSERT_FN: &str = r#"function (expectedFrame, expectedTop, clear, text) {
  const doc = this.ownerDocument;
  const win = doc.defaultView;
  if (win.location.origin !== expectedFrame) { return { ok: false, why: "frame_origin" }; }
  const ancestors = win.location.ancestorOrigins ? Array.from(win.location.ancestorOrigins) : [];
  const top = ancestors.length ? ancestors[ancestors.length - 1] : win.location.origin;
  if (top !== expectedTop) { return { ok: false, why: "top_origin" }; }
  if (doc.activeElement !== this) { return { ok: false, why: "focus" }; }
  if (clear) { if (this.select) { this.select(); } else { doc.execCommand("selectAll"); } }
  const done = doc.execCommand("insertText", false, text);
  return { ok: done === true, why: done === true ? "" : "insert_refused" };
}"#;

/// What has focus, descending through same-origin frames; a frame it cannot read is reported as such.
const ACTIVE_ELEMENT_JS: &str = r#"(() => {
  let doc = document;
  let el = doc.activeElement;
  while (el && (el.tagName === "IFRAME" || el.tagName === "FRAME")) {
    let inner = null;
    try { inner = el.contentDocument; } catch (e) { inner = null; }
    if (!inner) { return { cross_origin: true, tag: el.tagName.toLowerCase() }; }
    doc = inner;
    el = doc.activeElement;
  }
  if (!el) { return { cross_origin: false, tag: "" }; }
  const attr = (n) => (el.getAttribute ? el.getAttribute(n) : null) || "";
  return {
    cross_origin: false,
    tag: (el.tagName || "").toLowerCase(),
    type: (el.type || "").toLowerCase(),
    autocomplete: attr("autocomplete").toLowerCase(),
    name: attr("name"),
    id: el.id || "",
    placeholder: attr("placeholder"),
    aria_label: attr("aria-label"),
    editable: !!(el.isContentEditable || /^(input|textarea|select)$/i.test(el.tagName || ""))
  };
})()"#;

/// The CDP modifier bitmask for the named modifiers (`Alt` 1, `Control` 2, `Meta` 4, `Shift` 8).
fn modifier_mask(modifiers: &[String]) -> u32 {
    modifiers.iter().fold(0, |mask, modifier| {
        mask | match modifier.as_str() {
            "Alt" => 1,
            "Control" => 2,
            "Meta" => 4,
            "Shift" => 8,
            _ => 0,
        }
    })
}

/// A key a model may press, resolved to the CDP fields.
struct KeySpec {
    key: String,
    code: String,
    virtual_key: u32,
    text: Option<String>,
    modifiers: u32,
    printable: bool,
}

impl KeySpec {
    fn parse(spec: &str) -> Result<Self, BrowserFault> {
        let invalid = || {
            BrowserFault::new(
                FaultCode::InvalidRequest,
                format!("{spec:?} is not a key browser.press_key knows"),
            )
        };
        let mut parts: Vec<&str> = spec.split('+').collect();
        let base = parts
            .pop()
            .filter(|base| !base.is_empty())
            .ok_or_else(invalid)?;
        let mut modifiers = 0u32;
        for modifier in parts {
            modifiers |= match modifier {
                "Alt" => 1,
                "Control" | "Ctrl" => 2,
                "Meta" | "Command" => 4,
                "Shift" => 8,
                _ => return Err(invalid()),
            };
        }
        let named = |key: &str, code: &str, virtual_key: u32| {
            Ok(Self {
                key: key.to_string(),
                code: code.to_string(),
                virtual_key,
                text: None,
                modifiers,
                printable: false,
            })
        };
        match base {
            "Enter" => Ok(Self {
                text: Some("\r".to_string()),
                ..named("Enter", "Enter", 13)?
            }),
            "Tab" => named("Tab", "Tab", 9),
            "Escape" => named("Escape", "Escape", 27),
            "Backspace" => named("Backspace", "Backspace", 8),
            "Delete" => named("Delete", "Delete", 46),
            "ArrowUp" => named("ArrowUp", "ArrowUp", 38),
            "ArrowDown" => named("ArrowDown", "ArrowDown", 40),
            "ArrowLeft" => named("ArrowLeft", "ArrowLeft", 37),
            "ArrowRight" => named("ArrowRight", "ArrowRight", 39),
            "Home" => named("Home", "Home", 36),
            "End" => named("End", "End", 35),
            "PageUp" => named("PageUp", "PageUp", 33),
            "PageDown" => named("PageDown", "PageDown", 34),
            "Space" => Ok(Self {
                key: " ".to_string(),
                code: "Space".to_string(),
                virtual_key: 32,
                text: Some(" ".to_string()),
                modifiers,
                printable: true,
            }),
            single if single.chars().count() == 1 => {
                let character = single.chars().next().ok_or_else(invalid)?;
                let upper = character.to_ascii_uppercase();
                Ok(Self {
                    key: single.to_string(),
                    code: if character.is_ascii_alphabetic() {
                        format!("Key{upper}")
                    } else if character.is_ascii_digit() {
                        format!("Digit{character}")
                    } else {
                        String::new()
                    },
                    virtual_key: u32::from(upper),
                    text: Some(single.to_string()),
                    modifiers,
                    printable: true,
                })
            }
            _ => Err(invalid()),
        }
    }
}
