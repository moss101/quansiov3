//! Authorization decisions against a projection (DOMAIN.md §6.3, §7.3).
//!
//! Every path out of `authorize` names its reason: no matching grant, every match expired, the
//! requested tier above every match's ceiling, `never`, `ask` without a receipt, `ask` with a
//! receipt, `always`, and `always` on a tier-4 effect, which still requires a receipt. When
//! several grants match, the most permissive outcome among them decides.

use chrono::Duration;
use quansio_capability::{
    assemble, authorize, Approval, AuthorizationReason, AuthorizationRequest, CapabilityProjection,
    Decision, Grant, ProjectionRequest, ProjectionSubject, Tier,
};

mod common;
use common::{at, constraints, effect, fs, grant_with, source_with_platform};

fn projection(grants: Vec<Grant>) -> CapabilityProjection {
    let source = source_with_platform(grants, &[("fs.write.workspace", 1)]);
    assemble(
        &source,
        &ProjectionRequest::at(
            ProjectionSubject::run("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"),
            now(),
        ),
    )
    .expect("assembles")
    .projection
}

fn now() -> chrono::DateTime<chrono::Utc> {
    at("2026-09-27T12:00:00Z")
}

fn write(approval: Approval, max_tier: Option<u8>) -> Grant {
    grant_with(
        "fs.write.workspace",
        fs("/root/**"),
        constraints(max_tier, approval, None, None),
    )
}

fn decide(
    projection: &CapabilityProjection,
    tier: u8,
    receipt: bool,
) -> (Decision, AuthorizationReason) {
    let effect_class = effect("fs.write.workspace");
    let resource = fs("/root/notes.md");
    let request = AuthorizationRequest::new(
        &effect_class,
        &resource,
        Tier::new(tier).expect("tier"),
        if receipt {
            Approval::Always
        } else {
            Approval::Ask
        },
        &projection.inputs,
    )
    .with_now(now());
    let authorization = authorize(projection, &request).expect("fresh projection");
    (authorization.decision, authorization.reason)
}

#[test]
fn each_approval_decides_as_documented() {
    let ask = projection(vec![write(Approval::Ask, None)]);
    assert_eq!(
        decide(&ask, 1, false),
        (Decision::Ask, AuthorizationReason::ApprovalRequired)
    );
    assert_eq!(
        decide(&ask, 1, true),
        (Decision::Allow, AuthorizationReason::MatchedGrant)
    );

    let always = projection(vec![write(Approval::Always, None)]);
    assert_eq!(
        decide(&always, 1, false),
        (Decision::Allow, AuthorizationReason::ApprovalSatisfied)
    );

    let never = projection(vec![write(Approval::Never, None)]);
    assert_eq!(
        decide(&never, 1, true),
        (Decision::Deny, AuthorizationReason::ApprovalNever)
    );
}

#[test]
fn a_tier_four_effect_needs_a_receipt_even_under_always() {
    let always = projection(vec![write(Approval::Always, None)]);
    assert_eq!(
        decide(&always, 4, false),
        (Decision::Ask, AuthorizationReason::TierFourRequiresApproval)
    );
    assert_eq!(
        decide(&always, 4, true).0,
        Decision::Allow,
        "a receipt satisfies it"
    );
}

#[test]
fn a_tier_above_every_ceiling_is_denied() {
    let capped = projection(vec![write(Approval::Always, Some(1))]);
    assert_eq!(
        decide(&capped, 1, false).0,
        Decision::Allow,
        "at the ceiling"
    );
    assert_eq!(
        decide(&capped, 2, true),
        (Decision::Deny, AuthorizationReason::TierAboveMaxTier)
    );
}

#[test]
fn a_resource_outside_every_grant_is_denied() {
    let projection = projection(vec![write(Approval::Always, None)]);
    let effect_class = effect("fs.write.workspace");
    let outside = fs("/elsewhere/notes.md");
    let request = AuthorizationRequest::new(
        &effect_class,
        &outside,
        Tier::new(1).expect("tier"),
        Approval::Always,
        &projection.inputs,
    )
    .with_now(now());
    let authorization = authorize(&projection, &request).expect("fresh");
    assert_eq!(authorization.decision, Decision::Deny);
    assert_eq!(authorization.reason, AuthorizationReason::NoMatchingGrant);
    assert!(authorization.grant.is_none());
}

#[test]
fn an_expired_grant_never_authorizes_even_if_the_projection_did_not_expire() {
    // Assembly keeps the projection's expiry at the earliest grant expiry, so staleness
    // normally catches this first; authorize still checks each grant's own expiry.
    let mut projection = projection(vec![write(Approval::Always, None)]);
    projection.grants[0].constraints.expires_at = Some(now() - Duration::seconds(1));
    projection.expires_at = None;
    let (decision, reason) = decide(&projection, 1, true);
    assert_eq!(
        (decision, reason),
        (Decision::Deny, AuthorizationReason::GrantExpired)
    );
}

#[test]
fn the_most_permissive_matching_grant_decides() {
    let both = projection(vec![
        write(Approval::Never, None),
        grant_with(
            "fs.write.workspace",
            fs("/root/notes.md"),
            constraints(None, Approval::Always, None, None),
        ),
    ]);
    assert_eq!(
        decide(&both, 1, false),
        (Decision::Allow, AuthorizationReason::ApprovalSatisfied)
    );
}

#[test]
fn decisions_and_reasons_have_their_wire_names() {
    assert_eq!(Decision::Allow.to_string(), "allow");
    assert_eq!(Decision::Ask.to_string(), "ask");
    assert_eq!(Decision::Deny.to_string(), "deny");
    for (reason, wire) in [
        (AuthorizationReason::MatchedGrant, "matched_grant"),
        (AuthorizationReason::NoMatchingGrant, "no_matching_grant"),
        (AuthorizationReason::TierAboveMaxTier, "tier_above_max_tier"),
        (AuthorizationReason::GrantExpired, "grant_expired"),
        (AuthorizationReason::ApprovalNever, "approval_never"),
        (AuthorizationReason::ApprovalRequired, "approval_required"),
        (AuthorizationReason::ApprovalSatisfied, "approval_satisfied"),
        (
            AuthorizationReason::TierFourRequiresApproval,
            "tier_four_requires_approval",
        ),
    ] {
        assert_eq!(reason.to_string(), wire);
    }
}
