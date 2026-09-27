//! The grant value types (DOMAIN.md §6.1): effect classes, tiers, approvals, budget
//! references and policy documents parse strictly and combine most-restrictively.

use quansio_capability::{Approval, BudgetRef, EffectClass, PolicyDecision, PolicyDocument, Tier};
use serde_json::json;

mod common;
use common::{fs, grant};

#[test]
fn effect_classes_are_dotted_lowercase_words() {
    for valid in ["read.internal", "fs.write.workspace", "message.send"] {
        assert_eq!(EffectClass::parse(valid).expect(valid).as_str(), valid);
        assert_eq!(valid.parse::<EffectClass>().expect(valid).as_str(), valid);
    }
    for invalid in [
        "",
        ".read",
        "read.",
        "read..internal",
        "Read.Internal",
        "read internal",
    ] {
        let error = EffectClass::parse(invalid).expect_err(invalid);
        assert_eq!(error.code(), "VALIDATION_SCHEMA", "{invalid}");
    }
}

#[test]
fn tiers_run_zero_to_four_and_an_absent_tier_is_unbounded() {
    assert!(Tier::new(5).is_err());
    let two = Tier::new(2).expect("tier");
    assert_eq!(two.get(), 2);
    assert_eq!(two.to_string(), "2");
    assert_eq!(Tier::unbounded().get(), 4);
    assert_eq!(Tier::rank(Some(two)), 2);
    assert_eq!(Tier::rank(None), 4);
}

#[test]
fn approvals_combine_to_the_most_restrictive_and_default_to_ask() {
    assert_eq!(Approval::default(), Approval::Ask);
    assert_eq!(
        Approval::Always.most_restrictive(Approval::Ask),
        Approval::Ask
    );
    assert_eq!(
        Approval::Ask.most_restrictive(Approval::Never),
        Approval::Never
    );
    assert_eq!(
        Approval::Always.most_restrictive(Approval::Always),
        Approval::Always
    );
    assert_eq!(Approval::Never.to_string(), "never");
    assert_eq!(Approval::Always.to_string(), "always");
    assert_eq!(Approval::Ask.to_string(), "ask");
}

#[test]
fn budget_references_keep_the_smaller_allowance_deterministically() {
    let small = BudgetRef {
        reference: "bdg_b".to_string(),
        cap_units: Some(10),
    };
    let large = BudgetRef {
        reference: "bdg_a".to_string(),
        cap_units: Some(100),
    };
    let unresolved = BudgetRef::new("bdg_c");
    assert_eq!(BudgetRef::most_restrictive(&small, &large), &small);
    assert_eq!(
        BudgetRef::most_restrictive(&large, &small),
        &small,
        "commutative"
    );
    assert_eq!(
        BudgetRef::most_restrictive(&unresolved, &large),
        &large,
        "resolved beats unbounded"
    );
    assert_eq!(BudgetRef::most_restrictive(&large, &unresolved), &large);
    let tie = BudgetRef {
        reference: "bdg_z".to_string(),
        cap_units: Some(10),
    };
    assert_eq!(
        BudgetRef::most_restrictive(&tie, &small).reference,
        "bdg_b",
        "ties break by id"
    );
    assert_eq!(BudgetRef::most_restrictive(&small, &tie).reference, "bdg_b");
    assert_eq!(small.to_string(), "bdg_b");
}

#[test]
fn budget_references_read_both_wire_forms_and_write_the_reference() {
    let bare: BudgetRef = serde_json::from_value(json!("bdg_a")).expect("reference form");
    assert_eq!(bare, BudgetRef::new("bdg_a"));
    let detailed: BudgetRef = serde_json::from_value(json!({"reference": "bdg_a", "cap_units": 7}))
        .expect("detailed form");
    assert_eq!(detailed.cap_units, Some(7));
    assert_eq!(
        serde_json::to_value(&detailed).expect("serialize"),
        json!("bdg_a")
    );
    assert!(serde_json::from_value::<BudgetRef>(json!(7)).is_err());
}

#[test]
fn a_policy_document_parses_strictly_and_only_an_allow_permits() {
    let document = PolicyDocument::from_json(&json!({
        "reference": "pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        "rules": [
            {"effect_class": "fs.write.workspace", "resource": {"kind": "fs", "selector": "/root/**"}, "decision": "allow"},
            {"effect_class": "message.send", "resource": {"kind": "fs", "selector": "/root/**"}, "decision": "ask"}
        ]
    }))
    .expect("canonical policy");
    assert_eq!(document.rules[0].decision, PolicyDecision::Allow);
    assert!(document.explicitly_allows(&grant("fs.write.workspace", fs("/root/a"))));
    assert!(
        !document.explicitly_allows(&grant("message.send", fs("/root/a"))),
        "ask is not allow"
    );
    assert!(!document.explicitly_allows(&grant("fs.write.workspace", fs("/other"))));

    let error =
        PolicyDocument::from_json(&json!({"reference": "pol_x", "rules": [{"decision": "maybe"}]}))
            .expect_err("not canonical");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
}
