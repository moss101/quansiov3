//! Policy filtering and user rules during assembly (DOMAIN.md §6.2, §7.3).
//!
//! Policy is the only source of permission: a `deny` drops a grant, an `ask` or a rule-level
//! tier ceiling narrows it, and only an explicit `allow` lets a user rule move `ask` to
//! `always`. A user rule never adds authority: `never` drops, `ask` narrows, and `always` is
//! refused (and recorded) at tier 4, where policy does not allow it, or where the grant is
//! already `never`. An expired user rule governs nothing.

use chrono::Duration;
use quansio_capability::error::CapabilityError;
use quansio_capability::{
    assemble, Approval, Assembly, Grant, InputUnavailableReason, Layer, LayerInput, PolicyDecision,
    PolicyDocument, PolicyRule, ProjectionRequest, ProjectionSubject, Tier, UserRule,
    UserRuleDecision, WideningReason,
};

mod common;
use common::{at, constraints, effect, fs, grant, grant_with, source_with_platform, StaticSource};

const POLICY: &str = "pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

fn subject() -> ProjectionSubject {
    ProjectionSubject::run("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
}

fn rule(
    effect_class: &str,
    glob: &str,
    decision: PolicyDecision,
    tier_max: Option<u8>,
) -> PolicyRule {
    PolicyRule {
        effect_class: effect(effect_class),
        resource: fs(glob),
        decision,
        tier_max: tier_max.map(|tier| Tier::new(tier).expect("tier")),
        trust_max: Vec::new(),
        data_classes: Vec::new(),
    }
}

fn user_rule(effect_class: &str, glob: &str, decision: UserRuleDecision) -> UserRule {
    UserRule {
        id: "rule_01J8Z3K6F1N8VQ2X5W9Y0CCCCC".to_string(),
        effect_class: effect(effect_class),
        resource: fs(glob),
        decision,
        expires_at: None,
    }
}

/// Platform grants `write` (tier `tier`) under `/root/**`, a tenant policy of `rules` and the
/// user rules `user`.
fn source(tier: u8, rules: Vec<PolicyRule>, user: Vec<UserRule>) -> StaticSource {
    source_with_platform(
        vec![
            grant("fs.write.workspace", fs("/root/**")),
            grant("read.internal", fs("/root/**")),
        ],
        &[("fs.write.workspace", tier), ("read.internal", 0)],
    )
    .with(
        Layer::TenantPolicy,
        LayerInput::policy(PolicyDocument::new(POLICY, rules)),
    )
    .with(
        Layer::UserRules,
        LayerInput::user_rules("user_rules/usr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", user),
    )
}

fn assembled(source: &StaticSource) -> Assembly {
    assemble(source, &ProjectionRequest::new(subject())).expect("assembles")
}

fn write_grant(assembly: &Assembly) -> Option<&Grant> {
    assembly
        .projection
        .grants
        .iter()
        .find(|grant| grant.effect_class == effect("fs.write.workspace"))
}

fn allow_write() -> Vec<PolicyRule> {
    vec![rule(
        "fs.write.workspace",
        "/root/**",
        PolicyDecision::Allow,
        None,
    )]
}

#[test]
fn a_policy_deny_drops_the_grant_it_matches_and_nothing_else() {
    let assembly = assembled(&source(
        1,
        vec![rule(
            "fs.write.workspace",
            "/root/**",
            PolicyDecision::Deny,
            None,
        )],
        Vec::new(),
    ));
    assert!(
        write_grant(&assembly).is_none(),
        "denied: {:?}",
        assembly.projection.grants
    );
    assert_eq!(assembly.projection.grants.len(), 1, "read.internal stays");
}

#[test]
fn a_deny_wins_over_an_allow_on_the_same_grant() {
    let assembly = assembled(&source(
        1,
        vec![
            rule(
                "fs.write.workspace",
                "/root/**",
                PolicyDecision::Allow,
                None,
            ),
            rule(
                "fs.write.workspace",
                "/root/secret/**",
                PolicyDecision::Deny,
                None,
            ),
        ],
        Vec::new(),
    ));
    assert!(write_grant(&assembly).is_none());
}

#[test]
fn a_policy_ask_and_a_tier_ceiling_narrow_the_grant() {
    let source = source_with_platform(
        vec![grant_with(
            "fs.write.workspace",
            fs("/root/**"),
            constraints(Some(3), Approval::Always, None, None),
        )],
        &[("fs.write.workspace", 1)],
    )
    .with(
        Layer::TenantPolicy,
        LayerInput::policy(PolicyDocument::new(
            POLICY,
            vec![
                rule(
                    "fs.write.workspace",
                    "/root/**",
                    PolicyDecision::Ask,
                    Some(2),
                ),
                rule(
                    "fs.write.workspace",
                    "/root/a/**",
                    PolicyDecision::Allow,
                    Some(1),
                ),
            ],
        )),
    );
    let assembly = assembled(&source);
    let write = write_grant(&assembly).expect("kept");
    assert_eq!(
        write.constraints.approval,
        Approval::Ask,
        "ask narrows always"
    );
    assert_eq!(
        write.constraints.max_tier,
        Tier::new(1).ok(),
        "the lowest ceiling wins"
    );
}

#[test]
fn a_tier_ceiling_applies_to_a_grant_with_no_ceiling_of_its_own() {
    let assembly = assembled(&source(
        1,
        vec![rule(
            "fs.write.workspace",
            "/root/**",
            PolicyDecision::Allow,
            Some(2),
        )],
        Vec::new(),
    ));
    assert_eq!(
        write_grant(&assembly).expect("kept").constraints.max_tier,
        Tier::new(2).ok()
    );
}

#[test]
fn always_moves_ask_to_always_only_where_policy_allows() {
    let assembly = assembled(&source(
        2,
        allow_write(),
        vec![user_rule(
            "fs.write.workspace",
            "/root/**",
            UserRuleDecision::Always,
        )],
    ));
    assert_eq!(
        write_grant(&assembly).expect("kept").constraints.approval,
        Approval::Always
    );
    assert!(assembly.rejections.is_empty());
}

#[test]
fn always_without_a_policy_allow_is_refused_and_recorded() {
    let assembly = assembled(&source(
        2,
        vec![rule(
            "fs.write.workspace",
            "/root/**",
            PolicyDecision::Ask,
            None,
        )],
        vec![user_rule(
            "fs.write.workspace",
            "/root/**",
            UserRuleDecision::Always,
        )],
    ));
    assert_eq!(
        write_grant(&assembly).expect("kept").constraints.approval,
        Approval::Ask
    );
    assert_eq!(assembly.rejections.len(), 1);
    let rejection = &assembly.rejections[0];
    assert_eq!(rejection.layer, Layer::UserRules);
    assert_eq!(
        rejection.reason,
        WideningReason::ApprovalWideningNotPermitted
    );
    assert_eq!(rejection.grant.constraints.approval, Approval::Always);
}

#[test]
fn always_is_never_accepted_at_tier_four() {
    let assembly = assembled(&source(
        4,
        allow_write(),
        vec![user_rule(
            "fs.write.workspace",
            "/root/**",
            UserRuleDecision::Always,
        )],
    ));
    assert_eq!(
        write_grant(&assembly).expect("kept").constraints.approval,
        Approval::Ask
    );
    assert_eq!(assembly.rejections.len(), 1);
    assert_eq!(
        assembly.rejections[0].reason,
        WideningReason::TierFourAlwaysRejected
    );
}

#[test]
fn always_cannot_lift_a_grant_that_is_never() {
    let source = source_with_platform(
        vec![grant_with(
            "fs.write.workspace",
            fs("/root/**"),
            constraints(None, Approval::Never, None, None),
        )],
        &[("fs.write.workspace", 1)],
    )
    .with(
        Layer::TenantPolicy,
        LayerInput::policy(PolicyDocument::new(POLICY, allow_write())),
    )
    .with(
        Layer::UserRules,
        LayerInput::user_rules(
            "user_rules/usr",
            vec![user_rule(
                "fs.write.workspace",
                "/root/**",
                UserRuleDecision::Always,
            )],
        ),
    );
    let assembly = assembled(&source);
    assert_eq!(
        write_grant(&assembly).expect("kept").constraints.approval,
        Approval::Never
    );
    assert_eq!(
        assembly.rejections[0].reason,
        WideningReason::ApprovalWideningNotPermitted
    );
}

#[test]
fn a_user_never_drops_the_grant_even_beside_an_always() {
    let assembly = assembled(&source(
        1,
        allow_write(),
        vec![
            user_rule("fs.write.workspace", "/root/**", UserRuleDecision::Always),
            user_rule("fs.write.workspace", "/root/**", UserRuleDecision::Never),
        ],
    ));
    assert!(write_grant(&assembly).is_none());
    assert_eq!(assembly.projection.grants.len(), 1, "read.internal stays");
}

#[test]
fn a_user_ask_narrows_an_always_from_the_platform() {
    let source = source_with_platform(
        vec![grant_with(
            "fs.write.workspace",
            fs("/root/**"),
            constraints(None, Approval::Always, None, None),
        )],
        &[("fs.write.workspace", 1)],
    )
    .with(
        Layer::TenantPolicy,
        LayerInput::policy(PolicyDocument::new(POLICY, allow_write())),
    )
    .with(
        Layer::UserRules,
        LayerInput::user_rules(
            "user_rules/usr",
            vec![user_rule(
                "fs.write.workspace",
                "/root/**",
                UserRuleDecision::Ask,
            )],
        ),
    );
    assert_eq!(
        write_grant(&assembled(&source))
            .expect("kept")
            .constraints
            .approval,
        Approval::Ask
    );
}

#[test]
fn an_expired_user_rule_governs_nothing() {
    let now = at("2026-09-27T12:00:00Z");
    let mut expired = user_rule("fs.write.workspace", "/root/**", UserRuleDecision::Never);
    expired.expires_at = Some(now - Duration::seconds(1));
    let assembly = assemble(
        &source(1, allow_write(), vec![expired]),
        &ProjectionRequest::at(subject(), now),
    )
    .expect("assembles");
    assert!(
        write_grant(&assembly).is_some(),
        "an expired never no longer drops the grant"
    );
}

#[test]
fn a_user_rule_for_another_region_leaves_the_grant_alone() {
    let assembly = assembled(&source(
        1,
        allow_write(),
        vec![user_rule(
            "fs.write.workspace",
            "/elsewhere/**",
            UserRuleDecision::Never,
        )],
    ));
    assert_eq!(
        write_grant(&assembly).expect("kept").constraints.approval,
        Approval::Ask
    );
}

#[test]
fn always_on_an_effect_with_no_catalogue_tier_fails_closed() {
    // Policy allows it (a rule matches, so policy never asks for the tier); the user rule's
    // `always` must know the tier to refuse tier 4, and an unknown tier is never guessed.
    let source = source_with_platform(vec![grant("fs.write.workspace", fs("/root/**"))], &[])
        .with(
            Layer::TenantPolicy,
            LayerInput::policy(PolicyDocument::new(POLICY, allow_write())),
        )
        .with(
            Layer::UserRules,
            LayerInput::user_rules(
                "user_rules/usr",
                vec![user_rule(
                    "fs.write.workspace",
                    "/root/**",
                    UserRuleDecision::Always,
                )],
            ),
        );
    match assemble(&source, &ProjectionRequest::new(subject())) {
        Err(CapabilityError::InputsUnavailable(failure)) => {
            assert_eq!(failure.layer, Layer::UserRules);
            assert!(failure.projection.grants.is_empty());
            assert!(matches!(
                failure.reason,
                InputUnavailableReason::UnknownTier { .. }
            ));
        }
        other => panic!("expected CAPABILITY_INPUTS_UNAVAILABLE, got {other:?}"),
    }
}
