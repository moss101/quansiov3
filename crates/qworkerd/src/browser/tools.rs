//! The model-facing browser tool surface (ADP-001, DOMAIN.md §7.5, §12).
//!
//! [`BrowserDriver::dispatch`] is the one door a model's `browser.*` call comes through. It resolves the
//! declaration from the Tool Registry — so the schema a call is checked against is the one the runtime
//! planned the call with — and does four things the driver methods do not:
//!
//! * **strict arguments in.** An unknown field, a wrong type or a missing target is a typed
//!   `BROWSER_INVALID_REQUEST` before anything is looked at. That is also how the tools stay *only*
//!   what they declare: there is no tool for accepting a certificate or granting a permission, and no
//!   argument that could smuggle one in, so agent input has no way to ask for either.
//! * **schema-validated results out.** A result that does not match the declaration's `output_schema`
//!   is not returned; it is an error, because a result the schema does not describe is a result nobody
//!   reviewed.
//! * **everything page-derived is `UNTRUSTED_EXTERNAL`.** Snapshot text and element names, console
//!   lines, dialog messages and download names are page content. The reply carries the label, the
//!   declaration's `source_trust` says the same, and the runtime's context path renders it as data.
//! * **an unknown outcome is its own status.** A reply is `ok`, `error` or `in_doubt`, and `in_doubt`
//!   maps to `OUTCOME_UNKNOWN` on the Effect Ledger: the host reports it as unknown, the runtime
//!   reconciles it, and nothing retries it.

use serde_json::{json, Value};

use quansio_tools::{first_violation, ToolHost, ToolRegistry};

use super::driver::{ActionReport, BrowserCallContext, BrowserDriver};
use super::fault::{BrowserFault, FaultCode};
use super::fence::Target;
use super::secrets::PaymentField;
use super::Classified;

/// How a dispatched call ended, in the vocabulary the host reports to the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyStatus {
    /// The tool ran and its output is here.
    Ok,
    /// The tool refused or failed with a known outcome; `error` says which.
    Error,
    /// The outcome could not be established. Settles as `OUTCOME_UNKNOWN`; never retried.
    InDoubt,
}

impl ReplyStatus {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::InDoubt => "in_doubt",
        }
    }
}

/// The result of one `browser.*` call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolReply {
    /// The tool.
    pub tool: String,
    /// How it ended.
    pub status: ReplyStatus,
    /// The output: schema-validated for `ok`, the in-doubt description for `in_doubt`.
    pub output: Value,
    /// The typed error, for `error`: `{code, message, retryable, requires_new_observation, details?}`.
    pub error: Option<Value>,
    /// Evidence bytes (a screenshot), when the call captured any.
    pub evidence: Option<Vec<u8>>,
    /// What the action settles on the ledger, when it has an external consequence.
    pub classified: Option<Classified>,
    /// Whether this answered a replay of an action that already ran.
    pub replayed: bool,
}

/// The label every browser result carries (DOMAIN.md §12).
pub const RESULT_TRUST: &str = "UNTRUSTED_EXTERNAL";

impl ToolReply {
    /// What the Effect Ledger records for this reply: settled, failed or unknown.
    #[must_use]
    pub const fn ledger_outcome(&self) -> &'static str {
        match self.status {
            ReplyStatus::Ok => "SETTLED_SUCCESS",
            ReplyStatus::Error => "SETTLED_FAILED",
            ReplyStatus::InDoubt => "OUTCOME_UNKNOWN",
        }
    }

    /// The wire form returned to the runtime.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut reply = json!({
            "tool": self.tool,
            "status": self.status.as_str(),
            "trust": RESULT_TRUST,
            "output": self.output,
            "ledger_outcome": self.ledger_outcome(),
            "replayed": self.replayed,
        });
        if let Some(error) = &self.error {
            reply["error"] = error.clone();
        }
        reply
    }

    fn failure(tool: &str, fault: &BrowserFault) -> Self {
        Self {
            tool: tool.to_string(),
            status: ReplyStatus::Error,
            output: Value::Null,
            error: Some(fault.to_json()),
            evidence: None,
            classified: None,
            replayed: false,
        }
    }
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, BrowserFault> {
    text(args, key).ok_or_else(|| {
        BrowserFault::new(
            FaultCode::InvalidRequest,
            format!("{key} is required for this tool"),
        )
    })
}

impl BrowserDriver {
    /// Run one `browser.*` tool call: validate, execute, validate the result, label it.
    pub async fn dispatch(&self, tool: &str, args: &Value, ctx: &BrowserCallContext) -> ToolReply {
        let registry = ToolRegistry::builtin();
        let declaration = match registry.get(tool) {
            Some(declaration) if declaration.host == ToolHost::Browser => declaration,
            _ => {
                return ToolReply::failure(
                    tool,
                    &BrowserFault::new(
                        FaultCode::InvalidRequest,
                        format!("{tool:?} is not a browser tool this driver serves"),
                    ),
                )
            }
        };
        if let Some(violation) = first_violation(&declaration.input_schema, args) {
            return ToolReply::failure(
                tool,
                &BrowserFault::new(
                    FaultCode::InvalidRequest,
                    format!(
                        "the arguments do not match {tool}: {} {}",
                        violation.path, violation.detail
                    ),
                ),
            );
        }
        if text(args, "session_id") != Some(self.session_id().as_str()) {
            return ToolReply::failure(
                tool,
                &BrowserFault::new(
                    FaultCode::TargetNotFound,
                    "the call names a session this driver does not serve",
                )
                .with_details(json!({ "reason": "session_unknown" })),
            );
        }
        let result = self.route(tool, args, ctx).await;
        match result {
            Err(fault) => ToolReply::failure(tool, &fault),
            Ok(report) => finish(tool, &declaration.output_schema, report),
        }
    }

    async fn route(
        &self,
        tool: &str,
        args: &Value,
        ctx: &BrowserCallContext,
    ) -> Result<ActionReport, BrowserFault> {
        let tab_id = text(args, "tab_id");
        match tool {
            "browser.navigate" => {
                self.navigate(
                    ctx,
                    tab_id,
                    required(args, "url")?,
                    text(args, "expected_document_revision"),
                )
                .await
            }
            "browser.observe" => {
                self.observe(
                    tab_id,
                    args.get("include_screenshot").and_then(Value::as_bool) == Some(true),
                )
                .await
            }
            "browser.screenshot" => self.screenshot(tab_id).await,
            "browser.extract" => {
                self.extract(
                    tab_id,
                    required(args, "selector")?,
                    text(args, "format").unwrap_or("text"),
                )
                .await
            }
            "browser.click" => {
                self.click(
                    ctx,
                    required(args, "tab_id")?,
                    required(args, "expected_document_revision")?,
                    &Target::from_args(args)?,
                )
                .await
            }
            "browser.type" => {
                self.type_text(
                    ctx,
                    required(args, "tab_id")?,
                    required(args, "expected_document_revision")?,
                    &Target::from_args(args)?,
                    required(args, "text")?,
                    args.get("clear").and_then(Value::as_bool) == Some(true),
                )
                .await
            }
            "browser.fill_payment_field" => {
                let field = text(args, "field")
                    .and_then(PaymentField::parse)
                    .ok_or_else(|| {
                        BrowserFault::new(
                            FaultCode::InvalidRequest,
                            "field must be one of the payment fields the tool names",
                        )
                    })?;
                self.fill_payment_field(
                    ctx,
                    required(args, "tab_id")?,
                    required(args, "expected_document_revision")?,
                    &Target::from_args(args)?,
                    field,
                )
                .await
            }
            "browser.import_session" => {
                self.import_session(
                    ctx,
                    required(args, "import_id")?,
                    required(args, "digest")?,
                    args.get("cookie_count")
                        .and_then(Value::as_u64)
                        .and_then(|count| usize::try_from(count).ok())
                        .ok_or_else(|| {
                            BrowserFault::new(
                                FaultCode::InvalidRequest,
                                "cookie_count is required for this tool",
                            )
                        })?,
                )
                .await
            }
            "browser.press_key" => {
                self.press_key(
                    ctx,
                    required(args, "tab_id")?,
                    required(args, "expected_document_revision")?,
                    required(args, "key")?,
                )
                .await
            }
            "browser.handle_dialog" => {
                self.handle_dialog(
                    ctx,
                    required(args, "tab_id")?,
                    required(args, "expected_document_revision")?,
                    required(args, "dialog_id")?,
                    args.get("accept").and_then(Value::as_bool).ok_or_else(|| {
                        BrowserFault::new(FaultCode::InvalidRequest, "accept is required")
                    })?,
                    text(args, "prompt_text"),
                )
                .await
            }
            other => Err(BrowserFault::new(
                FaultCode::InvalidRequest,
                format!("{other:?} is not a browser tool this driver serves"),
            )),
        }
    }
}

fn finish(tool: &str, output_schema: &Value, report: ActionReport) -> ToolReply {
    if let Some(reason) = &report.in_doubt {
        return ToolReply {
            tool: tool.to_string(),
            status: ReplyStatus::InDoubt,
            output: report.output,
            error: Some(BrowserFault::new(FaultCode::ActionInDoubt, reason.clone()).to_json()),
            evidence: None,
            classified: report.classified,
            replayed: report.replayed,
        };
    }
    // A reconciled replay is not a tool result: it says the doubt was closed, and there is nothing in
    // the declaration's output to hold it to.
    let reconciled = report.output.get("reconciled").and_then(Value::as_bool) == Some(true);
    if !reconciled {
        if let Some(violation) = first_violation(output_schema, &report.output) {
            return ToolReply::failure(
                tool,
                &BrowserFault::new(
                    FaultCode::PageFailure,
                    format!(
                        "the result did not match {tool}'s declared output ({} {}); it was not returned",
                        violation.path, violation.detail
                    ),
                ),
            );
        }
    }
    ToolReply {
        tool: tool.to_string(),
        status: ReplyStatus::Ok,
        output: report.output,
        error: None,
        evidence: report.evidence,
        classified: report.classified,
        replayed: report.replayed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        ToolRegistry::builtin()
            .get("browser.click")
            .expect("browser.click is declared")
            .output_schema
            .clone()
    }

    #[test]
    fn a_result_the_declaration_does_not_describe_is_not_returned() {
        let schema = schema();
        // What the click declares: `clicked` is required and nothing undeclared may appear.
        let good = finish(
            "browser.click",
            &schema,
            ActionReport::of(json!({ "clicked": true })),
        );
        assert_eq!(good.status, ReplyStatus::Ok);
        assert_eq!(good.ledger_outcome(), "SETTLED_SUCCESS");
        assert_eq!(good.to_json()["trust"], RESULT_TRUST);

        for (label, output) in [
            (
                "an undeclared field",
                json!({ "clicked": true, "cookie": "abc" }),
            ),
            ("a missing required field", json!({ "completed": true })),
            ("a wrong type", json!({ "clicked": "yes" })),
        ] {
            let reply = finish("browser.click", &schema, ActionReport::of(output));
            assert_eq!(reply.status, ReplyStatus::Error, "{label}");
            assert_eq!(
                reply.output,
                Value::Null,
                "{label}: the result was returned anyway"
            );
            assert_eq!(
                reply.error.as_ref().expect("error")["code"],
                "BROWSER_PAGE_FAILURE",
                "{label}"
            );
        }
    }

    #[test]
    fn an_in_doubt_report_is_an_unknown_outcome_with_the_flags_that_forbid_a_retry() {
        let mut report = ActionReport::of(json!({ "in_doubt": true }));
        report.in_doubt = Some("input was sent and the browser then failed".to_string());
        let reply = finish("browser.click", &schema(), report);
        assert_eq!(reply.status, ReplyStatus::InDoubt);
        assert_eq!(reply.ledger_outcome(), "OUTCOME_UNKNOWN");
        let error = reply.error.expect("error");
        assert_eq!(error["code"], "BROWSER_ACTION_IN_DOUBT");
        assert_eq!(error["retryable"], false);
        assert_eq!(error["requires_new_observation"], true);
    }

    #[test]
    fn a_reconciled_replay_is_not_held_to_the_tools_output() {
        let reply = finish(
            "browser.click",
            &schema(),
            ActionReport::of(
                json!({ "reconciled": true, "action_id": "act_1", "outcome": "it had happened" }),
            ),
        );
        assert_eq!(reply.status, ReplyStatus::Ok);
    }
}
