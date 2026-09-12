//! Stale capability snapshot tests (RUN-005, DOMAIN.md §6.3).
//!
//! A projection whose `inputs_digest` no longer matches the current inputs, or that is
//! past `expires_at`, cannot authorize dispatch: `authorize` returns the typed
//! `CAPABILITY_INPUTS_UNAVAILABLE` failure and forces recomputation.

use chrono::{DateTime, Utc};
use quansio_capability::error::{CapabilityError, StaleReason};
use quansio_capability::{
    assemble, authorize, Approval, AuthorizationRequest, Decision, EffectClass, Grant, Layer,
    LayerInput, ProjectionInput, ProjectionRequest, ProjectionSubject, ResourceSelector, Tier,
};

mod common;
use common::{at, constraints, effect, fs, grant_with, source_with_platform};

fn subject() -> ProjectionSubject {
    ProjectionSubject::run("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
}

fn read_grant(expires_at: Option<DateTime<Utc>>) -> Grant {
    grant_with(
        "read.internal",
        fs("/root/**"),
        constraints(None, Approval::Always, expires_at, None),
    )
}

fn request<'a>(
    effect_class: &'a EffectClass,
    resource: &'a ResourceSelector,
    now: DateTime<Utc>,
    inputs: &'a [ProjectionInput],
) -> AuthorizationRequest<'a> {
    AuthorizationRequest::new(
        effect_class,
        resource,
        Tier::new(0).expect("tier"),
        Approval::Always,
        inputs,
    )
    .with_now(now)
}

#[test]
fn fresh_projection_authorizes_and_a_changed_digest_forces_recomputation() {
    let now = at("2026-06-01T00:00:00Z");
    let source = source_with_platform(vec![read_grant(None)], &[("read.internal", 0)]);
    let projection = assemble(&source, &ProjectionRequest::at(subject(), now))
        .expect("projection")
        .projection;

    let effect_class = effect("read.internal");
    let resource = fs("/root/a");
    let fresh = request(&effect_class, &resource, now, &projection.inputs);
    let authorization = authorize(&projection, &fresh).expect("fresh projection authorizes");
    assert_eq!(authorization.decision, Decision::Allow);
    assert!(authorization.grant.is_some());

    let mut changed = projection.inputs.clone();
    changed[0].reference = "platform/baseline/v2".to_string();
    let stale = request(&effect_class, &resource, now, &changed);
    let error = authorize(&projection, &stale).expect_err("stale must not authorize");
    assert_eq!(error.code(), "CAPABILITY_INPUTS_UNAVAILABLE");
    match error {
        CapabilityError::StaleProjection(reason) => {
            assert!(matches!(reason.reason, StaleReason::InputsChanged { .. }));
        }
        other => panic!("expected a stale projection, got {other:?}"),
    }
}

#[test]
fn expired_projection_cannot_authorize() {
    let expires_at = at("2026-01-01T00:00:00Z");
    let now = at("2026-06-01T00:00:00Z");
    let source = source_with_platform(vec![read_grant(Some(expires_at))], &[("read.internal", 0)]);
    let projection = assemble(&source, &ProjectionRequest::at(subject(), now))
        .expect("projection")
        .projection;
    assert_eq!(projection.expires_at, Some(expires_at));

    let effect_class = effect("read.internal");
    let resource = fs("/root/a");
    let stale = request(&effect_class, &resource, now, &projection.inputs);
    let error = authorize(&projection, &stale).expect_err("expired must not authorize");
    match error {
        CapabilityError::StaleProjection(reason) => {
            assert!(matches!(reason.reason, StaleReason::Expired { .. }));
        }
        other => panic!("expected a stale projection, got {other:?}"),
    }
}

#[test]
fn current_inputs_match_only_while_the_layer_digests_are_unchanged() {
    let now = at("2026-06-01T00:00:00Z");
    let source = source_with_platform(vec![read_grant(None)], &[("read.internal", 0)]);
    let projection = assemble(&source, &ProjectionRequest::at(subject(), now))
        .expect("projection")
        .projection;

    assert!(projection.inputs_match(&projection.inputs));
    assert!(!projection.is_expired(now));

    let mut changed = projection.inputs.clone();
    changed[3].reference = format!("{}/v2", changed[3].reference);
    assert!(!projection.inputs_match(&changed));
    assert_eq!(changed[3].layer, Layer::UserRole);
}

#[test]
fn layer_expiry_narrows_the_projection_expiry() {
    let layer_expiry = at("2026-03-01T00:00:00Z");
    let now = at("2026-02-01T00:00:00Z");
    let source = source_with_platform(vec![read_grant(None)], &[("read.internal", 0)]).with(
        Layer::ActiveSkills,
        LayerInput::empty("skill/none").with_expires_at(layer_expiry),
    );
    let projection = assemble(&source, &ProjectionRequest::at(subject(), now))
        .expect("projection")
        .projection;
    assert_eq!(projection.expires_at, Some(layer_expiry));
}
