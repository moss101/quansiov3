//! The action receipt in the Tool contract (ADP-002, DOMAIN.md §7.5 and §8.6.1 A).
//!
//! Every `computer.*` Tool's `output_schema` must carry the canonical `receipt` object, required, so the
//! typed Tool result always says what the action did: `effect_verified` is `true`, `false` or `null`, and
//! `null` (unknown) is a value of its own. The registry refuses a declaration that omits the receipt or
//! loosens its schema, and a result is checked for the cross-field coherence a JSON Schema cannot
//! express (an effect cannot be verified when nothing was dispatched).
//!
//! The runtime half of the same rules, which re-derives a bridge's receipt, is
//! `quansio_machine::computer_use::receipt`; the two are held to one wire shape by a test in that crate.

use serde_json::{json, Value};

use crate::declaration::ToolDeclaration;
use crate::error::ToolError;
use crate::schema::first_violation;

/// Tools whose results carry a receipt.
pub const RECEIPT_TOOL_PREFIX: &str = "computer.";

const METHODS: [&str; 5] = [
    "accessibility_tree",
    "accessibility_action",
    "process_event",
    "hid_event",
    "pasteboard",
];
const OUTCOMES: [&str; 6] = [
    "applied",
    "observed",
    "unverified",
    "not_applied",
    "invariant_violated",
    "refused",
];

/// Whether a Tool's result carries a receipt.
#[must_use]
pub fn carries_receipt(tool: &str) -> bool {
    tool.starts_with(RECEIPT_TOOL_PREFIX)
}

fn app_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "bundle_id": { "type": "string" },
            "pid": { "type": "integer" },
        },
        "required": ["bundle_id", "pid"],
    })
}

/// The canonical receipt schema every `computer.*` output must contain, verbatim.
#[must_use]
pub fn receipt_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "method": { "type": "string", "enum": METHODS },
            "execution_method": { "type": "string", "enum": ["background", "foreground"] },
            "dispatched": { "type": "boolean" },
            "effect_verified": { "type": ["boolean", "null"] },
            "outcome": { "type": "string", "enum": OUTCOMES },
            "invariant_violated": { "type": "boolean" },
            "invariants": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "properties": {
                    "physical_input": { "type": "boolean" },
                    "moved_system_cursor": { "type": "boolean" },
                    "frontmost_before": app_schema(),
                    "frontmost_after": app_schema(),
                    "frontmost_changed": { "type": "boolean" },
                },
                "required": [
                    "physical_input",
                    "moved_system_cursor",
                    "frontmost_before",
                    "frontmost_after",
                    "frontmost_changed",
                ],
            },
        },
        "required": [
            "method",
            "execution_method",
            "dispatched",
            "effect_verified",
            "outcome",
            "invariant_violated",
            "invariants",
        ],
    })
}

/// Prove a `computer.*` declaration carries the canonical receipt, required, in its output.
///
/// # Errors
/// Returns [`ToolError::MalformedDeclaration`] when a receipt-carrying Tool's output schema has no
/// `receipt`, has a different one, or does not require it.
pub fn check_declaration(declaration: &ToolDeclaration) -> Result<(), ToolError> {
    if !carries_receipt(&declaration.name) {
        return Ok(());
    }
    let malformed = |detail: &str| ToolError::MalformedDeclaration {
        tool: declaration.name.clone(),
        detail: detail.to_string(),
    };
    let schema = &declaration.output_schema;
    let Some(receipt) = schema.pointer("/properties/receipt") else {
        return Err(malformed(
            "a computer Tool's output_schema must declare a receipt (DOMAIN.md §8.6.1 A)",
        ));
    };
    if receipt != &receipt_schema() {
        return Err(malformed(
            "the receipt in the output_schema is not the canonical receipt schema",
        ));
    }
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|names| names.iter().any(|name| name == "receipt"));
    if !required {
        return Err(malformed("the receipt must be a required output field"));
    }
    Ok(())
}

/// The cross-field rules a receipt must satisfy (DOMAIN.md §8.6.1 A), over the wire form.
///
/// # Errors
/// Returns what contradicts what.
pub fn check_coherence(receipt: &Value) -> Result<(), String> {
    let field = |name: &str| receipt.get(name);
    let dispatched = field("dispatched")
        .and_then(Value::as_bool)
        .ok_or("dispatched must be a boolean")?;
    let violated = field("invariant_violated")
        .and_then(Value::as_bool)
        .ok_or("invariant_violated must be a boolean")?;
    let outcome = field("outcome")
        .and_then(Value::as_str)
        .ok_or("outcome must be a string")?;
    let background = field("execution_method").and_then(Value::as_str) == Some("background");
    // `effect_verified` must be present as true, false or null: its absence says nothing.
    let verified = match field("effect_verified") {
        Some(Value::Bool(flag)) => Some(*flag),
        Some(Value::Null) => None,
        _ => return Err("effect_verified must be true, false or null".to_string()),
    };
    if !dispatched {
        return if outcome == "refused" && verified == Some(false) && !violated {
            Ok(())
        } else {
            Err(
                "a receipt that dispatched nothing must be refused with effect_verified=false"
                    .to_string(),
            )
        };
    }
    if outcome == "refused" {
        return Err("a refused receipt cannot say it dispatched".to_string());
    }
    if violated {
        return if background && verified == Some(false) && outcome == "invariant_violated" {
            Ok(())
        } else {
            Err("a violated invariant is a background action with effect_verified=false and outcome invariant_violated"
                .to_string())
        };
    }
    match (verified, outcome) {
        (Some(true), "applied")
        | (Some(false), "not_applied")
        | (None, "observed" | "unverified") => {}
        _ => return Err("effect_verified and outcome disagree".to_string()),
    }
    if verified == Some(true) && background {
        let measured = field("invariants").is_some_and(|value| !value.is_null());
        if !measured {
            return Err("a background action cannot be verified without a measurement".to_string());
        }
    }
    Ok(())
}

/// Validate a `computer.*` Tool result: its output schema, then the receipt's coherence.
///
/// # Errors
/// Returns [`ToolError::InvalidResult`] for the first violation.
pub fn validate_result(declaration: &ToolDeclaration, result: &Value) -> Result<(), ToolError> {
    let invalid = |path: String, detail: String| ToolError::InvalidResult {
        tool: declaration.name.clone(),
        path,
        detail,
    };
    if let Some(violation) = first_violation(&declaration.output_schema, result) {
        return Err(invalid(violation.path, violation.detail));
    }
    if carries_receipt(&declaration.name) {
        let receipt = result
            .get("receipt")
            .ok_or_else(|| invalid(".receipt".to_string(), "is a required field".to_string()))?;
        check_coherence(receipt).map_err(|detail| invalid(".receipt".to_string(), detail))?;
    }
    Ok(())
}
