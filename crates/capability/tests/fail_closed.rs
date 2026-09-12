//! Fail-closed tests (RUN-005, DOMAIN.md §6.3).
//!
//! A missing or unresolvable input — an unreachable layer source, an unparseable grant,
//! a policy with no rule for a tier ≥ 1 effect class, an unknown catalogue tier — yields
//! an empty projection and the typed `CAPABILITY_INPUTS_UNAVAILABLE` failure. Never a
//! partial projection that quietly grants more.

use quansio_capability::error::{CapabilityError, InputsUnavailable};
use quansio_capability::{
    assemble, Approval, Grant, InputUnavailableReason, Layer, LayerInput, PolicyDocument,
    ProjectionRequest, ProjectionSubject,
};
use serde_json::json;

mod common;
use common::{constraints, effect, fs, grant, grant_with, source_with_platform};

fn subject() -> ProjectionSubject {
    ProjectionSubject::run("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
}

fn expect_unavailable(error: CapabilityError) -> InputsUnavailable {
    match error {
        CapabilityError::InputsUnavailable(failure) => *failure,
        other => panic!("expected CAPABILITY_INPUTS_UNAVAILABLE, got {other:?}"),
    }
}

#[test]
fn unresolvable_layer_yields_empty_projection_and_typed_error() {
    let source = source_with_platform(
        vec![grant("read.internal", fs("/root/**"))],
        &[("read.internal", 0)],
    )
    .unavailable(Layer::WorkspacePolicy);

    let error = assemble(&source, &ProjectionRequest::new(subject())).expect_err("fail closed");
    assert_eq!(error.code(), "CAPABILITY_INPUTS_UNAVAILABLE");
    let failure = expect_unavailable(error);
    assert_eq!(failure.layer, Layer::WorkspacePolicy);
    assert!(
        failure.projection.grants.is_empty(),
        "a failed projection must grant nothing"
    );
    assert!(failure
        .projection
        .inputs
        .iter()
        .all(|input| input.layer.ordinal() < Layer::WorkspacePolicy.ordinal()));
}

#[test]
fn unparseable_grant_is_rejected() {
    let malformed = json!({
        "effect_class": "Message.Send",
        "resource": {"kind": "fs", "selector": "/root"},
        "constraints": {"approval": "ask"}
    });
    let error = Grant::from_json(&malformed).expect_err("must not parse");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");

    let missing_resource = json!({"effect_class": "message.send"});
    assert!(Grant::from_json(&missing_resource).is_err());

    let unknown_kind = json!({
        "effect_class": "message.send",
        "resource": {"kind": "gopher", "selector": "/root"}
    });
    assert!(Grant::from_json(&unknown_kind).is_err());
}

#[test]
fn unparseable_layer_payload_fails_closed() {
    let source = source_with_platform(
        vec![grant("read.internal", fs("/root/**"))],
        &[("read.internal", 0)],
    )
    .failing(
        Layer::TenantPolicy,
        InputUnavailableReason::Unparseable {
            detail: "policy document is not canonical JSON".to_string(),
        },
    );

    let error = assemble(&source, &ProjectionRequest::new(subject())).expect_err("fail closed");
    let failure = expect_unavailable(error);
    assert_eq!(failure.layer, Layer::TenantPolicy);
    assert!(failure.projection.grants.is_empty());
}

#[test]
fn policy_gap_at_tier_one_or_above_fails_closed() {
    let platform = vec![
        grant_with(
            "fs.write.workspace",
            fs("/root/a"),
            constraints(Some(1), Approval::Ask, None, None),
        ),
        grant("read.internal", fs("/root/**")),
    ];
    let source = source_with_platform(platform, &[("fs.write.workspace", 1), ("read.internal", 0)])
        .with(
            Layer::TenantPolicy,
            LayerInput::policy(PolicyDocument::new(
                "pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
                Vec::new(),
            )),
        );

    let error = assemble(&source, &ProjectionRequest::new(subject())).expect_err("fail closed");
    assert_eq!(error.code(), "CAPABILITY_INPUTS_UNAVAILABLE");
    let failure = expect_unavailable(error);
    assert_eq!(failure.layer, Layer::TenantPolicy);
    assert!(
        failure.projection.grants.is_empty(),
        "no partial projection"
    );
    match failure.reason {
        InputUnavailableReason::PolicyGap { effect_class, tier } => {
            assert_eq!(effect_class, effect("fs.write.workspace"));
            assert_eq!(tier.get(), 1);
        }
        other => panic!("expected a policy gap, got {other:?}"),
    }
}

#[test]
fn tier_zero_without_a_policy_rule_keeps_the_default_allow() {
    let source = source_with_platform(
        vec![grant("read.internal", fs("/root/**"))],
        &[("read.internal", 0)],
    )
    .with(
        Layer::TenantPolicy,
        LayerInput::policy(PolicyDocument::new(
            "pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
            Vec::new(),
        )),
    );

    let assembly = assemble(&source, &ProjectionRequest::new(subject())).expect("tier 0 allows");
    assert_eq!(assembly.projection.grants.len(), 1);
}

#[test]
fn unknown_catalogue_tier_fails_closed() {
    let source = source_with_platform(vec![grant("read.internal", fs("/root/**"))], &[]).with(
        Layer::TenantPolicy,
        LayerInput::policy(PolicyDocument::new(
            "pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
            Vec::new(),
        )),
    );

    let error = assemble(&source, &ProjectionRequest::new(subject())).expect_err("fail closed");
    let failure = expect_unavailable(error);
    assert!(matches!(
        failure.reason,
        InputUnavailableReason::UnknownTier { .. }
    ));
    assert!(failure.projection.grants.is_empty());
}
