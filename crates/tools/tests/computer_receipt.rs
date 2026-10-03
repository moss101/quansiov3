//! The action receipt in the Tool Registry schema (ADP-002, DOMAIN.md §7.5 and §8.6.1 A).
//!
//! Asserted from the shipped `config/tools.yaml`: every `computer.*` Tool carries the one canonical,
//! required `receipt` in its output; a declaration that drops or loosens it is refused at load; and a
//! result with an incoherent receipt (a verified effect for an action that was never dispatched) fails
//! output validation, while `effect_verified: null` — unknown — is a valid value of its own.

use quansio_tools::receipt::{check_coherence, receipt_schema, validate_result};
use quansio_tools::{ToolError, ToolRegistry};
use serde_json::{json, Value};

const TOOLS_YAML: &str = include_str!("../../../config/tools.yaml");

const COMPUTER_TOOLS: [&str; 5] = [
    "computer.read",
    "computer.click",
    "computer.type",
    "computer.clipboard",
    "computer.system_key",
];

fn receipt(overrides: Value) -> Value {
    let mut base = json!({
        "method": "hid_event",
        "execution_method": "foreground",
        "dispatched": true,
        "effect_verified": null,
        "outcome": "unverified",
        "invariant_violated": false,
        "invariants": null,
    });
    for (key, value) in overrides.as_object().expect("overrides").clone() {
        base[key] = value;
    }
    base
}

fn click_result(receipt: Value) -> Value {
    json!({ "clicked": true, "receipt": receipt })
}

fn click() -> &'static quansio_tools::ToolDeclaration {
    ToolRegistry::builtin().get("computer.click").expect("computer.click")
}

#[test]
fn every_computer_tool_carries_the_canonical_required_receipt() {
    let registry = ToolRegistry::builtin();
    for name in COMPUTER_TOOLS {
        let declaration = registry.get(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(
            declaration.output_schema.pointer("/properties/receipt"),
            Some(&receipt_schema()),
            "{name}"
        );
        let required = declaration.output_schema["required"].as_array().expect("required");
        assert!(required.iter().any(|field| field == "receipt"), "{name}");
    }
    // Only computer tools carry one: a browser Tool's results are not receipts.
    assert!(registry
        .get("browser.click")
        .expect("browser.click")
        .output_schema
        .pointer("/properties/receipt")
        .is_none());
    assert!(registry.check_receipts().is_ok());
}

#[test]
fn a_computer_declaration_that_drops_or_loosens_the_receipt_is_refused() {
    let broken = |from: &str, to: &str| -> ToolRegistry {
        assert!(TOOLS_YAML.contains(from), "{from}");
        ToolRegistry::from_yaml(&TOOLS_YAML.replacen(from, to, 1)).expect("still a valid registry")
    };
    let refused = [
        // The receipt is no longer required of computer.click.
        broken("required: [clicked, receipt]", "required: [clicked]"),
        // effect_verified loses its null, so an unknown verdict could not be said.
        broken(
            "effect_verified: { type: [boolean, \"null\"] }",
            "effect_verified: { type: boolean }",
        ),
        // The set of outcomes is widened.
        broken(
            "enum: [applied, observed, unverified, not_applied, invariant_violated, refused]",
            "enum: [applied, observed, unverified, not_applied, invariant_violated, refused, probably]",
        ),
        // A field of the receipt is no longer required.
        broken(
            "required: [method, execution_method, dispatched, effect_verified, outcome, invariant_violated, invariants]",
            "required: [method, execution_method, dispatched, outcome, invariant_violated, invariants]",
        ),
    ];
    for registry in refused {
        let error = registry.check_receipts().expect_err("must be refused");
        assert!(matches!(error, ToolError::MalformedDeclaration { .. }), "{error:?}");
    }
    // A computer Tool with no receipt at all.
    let without = TOOLS_YAML.replacen("        receipt: *computer_receipt\n      required: [clicked, receipt]", "      required: [clicked]", 1);
    assert_ne!(without, TOOLS_YAML);
    let registry = ToolRegistry::from_yaml(&without).expect("registry");
    assert!(registry.check_receipts().is_err());
}

#[test]
fn unknown_verification_is_a_valid_result_and_a_made_up_verdict_is_not() {
    let declaration = click();
    // true, false and null are the three verdicts.
    let applied = receipt(json!({ "effect_verified": true, "outcome": "applied" }));
    let unverified = receipt(json!({}));
    let not_applied = receipt(json!({ "effect_verified": false, "outcome": "not_applied" }));
    for ok in [applied, unverified, not_applied] {
        validate_result(declaration, &click_result(ok.clone())).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    // Anything else is not.
    for bad in [json!("unknown"), json!("true"), json!(0), json!({})] {
        let result = click_result(receipt(json!({ "effect_verified": bad })));
        assert!(
            matches!(validate_result(declaration, &result), Err(ToolError::InvalidResult { .. })),
            "{result}"
        );
    }
    // A missing verdict is not read as unknown.
    let mut missing = receipt(json!({}));
    missing.as_object_mut().expect("object").remove("effect_verified");
    assert!(validate_result(declaration, &click_result(missing)).is_err());
    // A result with no receipt at all.
    assert!(validate_result(declaration, &json!({ "clicked": true })).is_err());
    // An unknown field in the receipt.
    let mut extra = receipt(json!({}));
    extra["verified"] = json!(true);
    assert!(validate_result(declaration, &click_result(extra)).is_err());
}

#[test]
fn a_receipt_that_contradicts_itself_fails_output_validation() {
    let declaration = click();
    let contradictions = [
        // Verified, but nothing was dispatched.
        receipt(json!({ "dispatched": false, "effect_verified": true, "outcome": "applied" })),
        receipt(json!({ "dispatched": false })),
        // Verified true with a word that says unknown or absent.
        receipt(json!({ "effect_verified": true })),
        receipt(json!({ "effect_verified": true, "outcome": "not_applied" })),
        // Unknown with a word that says known.
        receipt(json!({ "outcome": "applied" })),
        receipt(json!({ "outcome": "not_applied" })),
        // Refused although dispatched.
        receipt(json!({ "effect_verified": false, "outcome": "refused" })),
        // A violated invariant on a foreground action, or without the outcome that says so.
        receipt(json!({ "invariant_violated": true, "effect_verified": false, "outcome": "invariant_violated" })),
        receipt(json!({ "execution_method": "background", "invariant_violated": true })),
        // Background and verified, but never measured.
        receipt(json!({ "execution_method": "background", "effect_verified": true, "outcome": "applied" })),
    ];
    for bad in contradictions {
        assert!(check_coherence(&bad).is_err(), "{bad}");
        assert!(
            matches!(
                validate_result(declaration, &click_result(bad.clone())),
                Err(ToolError::InvalidResult { .. })
            ),
            "{bad}"
        );
    }
    // The coherent cases pass the same check.
    let measured = json!({
        "physical_input": false,
        "moved_system_cursor": false,
        "frontmost_before": { "bundle_id": "com.apple.TextEdit", "pid": 1 },
        "frontmost_after": { "bundle_id": "com.apple.TextEdit", "pid": 1 },
        "frontmost_changed": false,
    });
    for good in [
        receipt(json!({})),
        receipt(json!({ "outcome": "observed" })),
        receipt(json!({ "dispatched": false, "effect_verified": false, "outcome": "refused" })),
        receipt(json!({ "execution_method": "background", "effect_verified": true, "outcome": "applied", "invariants": measured })),
        receipt(json!({ "execution_method": "background", "invariant_violated": true, "effect_verified": false, "outcome": "invariant_violated", "invariants": measured })),
    ] {
        assert!(check_coherence(&good).is_ok(), "{good}");
    }
}
