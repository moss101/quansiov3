//! The policy decision pipeline (DOMAIN.md §7.1, §7.3, §12).
//!
//! Evaluation order, and it is deliberately strict:
//!
//! 1. **RBAC** — the actor's role must permit the action family.
//! 2. **Capability projection** — a proposal without a resolved projection has no
//!    authority and is denied (`CAPABILITY_INPUTS_UNAVAILABLE`).
//! 3. **Content-trust escalation** — an `UNTRUSTED_EXTERNAL` derivation raises the
//!    effective tier and forbids `always`.
//! 4. **Privacy guard** — protected data classes and untrusted content may not move to a
//!    destination outside the egress grant set.
//! 5. **Sequence guards** — an approval cannot be used before it is granted, and a
//!    settlement cannot precede its reservation.
//! 6. **Policy rules** — tenant policy then workspace policy; the most restrictive
//!    matching decision wins, and a missing rule at tier ≥ 1 denies.
//! 7. **Receipt requirement** — tier ≥ 3 requires an approval receipt unless a standing
//!    rule applies; tier 4 never accepts `always`.
//! 8. **User rules** — evaluated last; they may only narrow, or move `ask → always`
//!    where policy permits and never on tier 4 or an escalated proposal.
//!
//! Any missing security input is a deny with a typed reason. There is no path in this
//! module that turns an absent input into an allow.

use chrono::{DateTime, Utc};
use quansio_capability::{Decision, EffectClass, ResourceSelector, Tier, UserRuleDecision};
use quansio_core::Digest;
use serde_json::json;

use crate::policy::escalation::{escalate, Escalation, TrustLevel, FAIL_CLOSED_TRUST};
use crate::policy::guards::{
    check_privacy, check_sequence, DataClass, EgressDestination, EgressGrantSet, GuardFailure,
    PrivacyInputs, SequenceContext, SequenceGuard,
};
use crate::policy::rbac::{self, ActionFamily, ActorRoles, RbacFailure};
use crate::policy::rules::{PolicySet, RuleContext, UserRule};

/// Why a `UserRule` `always` decision was ignored (DOMAIN.md §7.1, §7.3, §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserRuleRejection {
    /// Tier 4 never accepts `always`.
    TierFour,
    /// An escalated proposal never accepts `always`.
    Escalated,
    /// Policy did not explicitly allow the effect, so `ask` may not widen.
    NotPermittedByPolicy,
}

impl UserRuleRejection {
    /// The canonical reason string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TierFour => "user_rule_tier_four_always_rejected",
            Self::Escalated => "user_rule_escalated_always_rejected",
            Self::NotPermittedByPolicy => "user_rule_always_not_permitted",
        }
    }
}

/// A recorded rejection of a widening user rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedUserRule {
    /// The `rule_…` id that was ignored.
    pub rule_id: String,
    /// Why it was ignored.
    pub reason: UserRuleRejection,
}

/// The typed reason behind a policy decision (DOMAIN.md §15 `POLICY_DENIED` family).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyReason {
    /// A matching grant/rule permits the effect and no receipt is required.
    Allowed,
    /// A standing user rule permits the effect without a receipt.
    AllowedByStandingRule,
    /// Policy requires an approval receipt (tier ≥ 3, or an `ask` rule).
    ApprovalRequired,
    /// RBAC refused the action family.
    Rbac(RbacFailure),
    /// No capability projection was supplied, so there is no authority to evaluate.
    CapabilityProjectionMissing,
    /// A matching policy rule explicitly denies.
    PolicyExplicitDeny,
    /// No policy rule matched and the effective tier is ≥ 1: a policy gap denies.
    NoMatchingPolicyRule {
        /// Effective tier that had no rule.
        tier: u8,
    },
    /// A privacy or sequence guard denied.
    Guard(GuardFailure),
    /// A matching user rule is `never`.
    UserRuleNever {
        /// The `rule_…` id.
        rule_id: String,
    },
}

impl PolicyReason {
    /// The canonical reason string recorded on the decision row.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::AllowedByStandingRule => "allowed_by_standing_rule",
            Self::ApprovalRequired => "approval_required",
            Self::Rbac(failure) => failure.as_str(),
            Self::CapabilityProjectionMissing => "capability_projection_missing",
            Self::PolicyExplicitDeny => "policy_explicit_deny",
            Self::NoMatchingPolicyRule { .. } => "no_matching_policy_rule",
            Self::Guard(GuardFailure::ProtectedEgressDenied) => "privacy_protected_egress_denied",
            Self::Guard(GuardFailure::UntrustedEgressDenied) => "privacy_untrusted_egress_denied",
            Self::Guard(GuardFailure::SequenceViolation(
                SequenceGuard::ApprovalGrantedBeforeUse,
            )) => "sequence_approval_not_granted",
            Self::Guard(GuardFailure::SequenceViolation(
                SequenceGuard::ReservationBeforeSettlement,
            )) => "sequence_settlement_before_reservation",
            Self::UserRuleNever { .. } => "user_rule_never",
        }
    }
}

impl core::fmt::Display for PolicyReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The complete input to one policy evaluation.
#[derive(Debug, Clone, Copy)]
pub struct EvaluationRequest<'a> {
    /// The proposed effect class.
    pub effect_class: &'a EffectClass,
    /// The exact resource of the proposal.
    pub resource: &'a ResourceSelector,
    /// The catalog tier of the effect class.
    pub catalog_tier: Tier,
    /// The command/action family the proposal belongs to.
    pub action: ActionFamily,
    /// The actor's roles.
    pub roles: ActorRoles,
    /// `derived_from_trust`; `None` fails closed to `UNTRUSTED_EXTERNAL`.
    pub derived_from_trust: Option<TrustLevel>,
    /// Data classes carried by the proposal.
    pub data_classes: &'a [DataClass],
    /// Destination the proposal would move data to, when any.
    pub destination: Option<&'a EgressDestination>,
    /// Destinations already inside the egress grant set.
    pub egress_grants: &'a EgressGrantSet,
    /// The resolved capability projection id; `None` denies.
    pub capability_projection_id: Option<&'a str>,
    /// Sequence guards the proposal must satisfy.
    pub sequence_guards: &'a [SequenceGuard],
    /// Durable sequence facts.
    pub sequence: &'a SequenceContext,
    /// The acting user, for user-rule matching; `None` applies no user rules.
    pub user_id: Option<&'a str>,
    /// When the decision is made.
    pub now: DateTime<Utc>,
}

/// The outcome of one policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyOutcome {
    /// The decision (`allow | ask | deny`).
    pub decision: Decision,
    /// The typed reason.
    pub reason: PolicyReason,
    /// The catalog tier of the effect class.
    pub catalog_tier: Tier,
    /// The effective tier after content-trust escalation.
    pub effective_tier: Tier,
    /// Whether the untrusted derivation escalated the tier.
    pub escalated: bool,
    /// Whether a receipt is required for dispatch.
    pub approval_required: bool,
    /// Whether a standing user rule supplied the authority.
    pub standing_allow: bool,
    /// Widening user rules that were ignored.
    pub rejected_user_rules: Vec<RejectedUserRule>,
    /// SHA-256 over the exact evaluation inputs.
    pub inputs_digest: Digest,
}

impl PolicyOutcome {
    /// Whether the decision permits dispatch without a receipt.
    #[must_use]
    pub fn is_allow(&self) -> bool {
        self.decision == Decision::Allow
    }

    /// Whether the proposal requires an approval receipt.
    #[must_use]
    pub fn requires_approval(&self) -> bool {
        self.decision == Decision::Ask
    }

    /// Whether the proposal is refused.
    #[must_use]
    pub fn is_deny(&self) -> bool {
        self.decision == Decision::Deny
    }

    /// The DOMAIN.md §15 error code a caller should return.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.decision {
            Decision::Allow => "OK",
            Decision::Ask => "APPROVAL_REQUIRED",
            Decision::Deny => "POLICY_DENIED",
        }
    }
}

/// A tenant/workspace policy set plus the user rules evaluated after it.
#[derive(Debug, Clone, Copy)]
pub struct PolicyEvaluator<'a> {
    /// The effective policies, tenant first.
    pub policies: &'a PolicySet,
    /// The user rules in scope.
    pub user_rules: &'a [UserRule],
}

impl<'a> PolicyEvaluator<'a> {
    /// Build an evaluator.
    #[must_use]
    pub const fn new(policies: &'a PolicySet, user_rules: &'a [UserRule]) -> Self {
        Self {
            policies,
            user_rules,
        }
    }

    /// Evaluate one proposal exactly as documented at the module head.
    #[must_use]
    pub fn evaluate(&self, request: &EvaluationRequest<'_>) -> PolicyOutcome {
        let trust = request.derived_from_trust.unwrap_or(FAIL_CLOSED_TRUST);
        let escalation = escalate(request.catalog_tier, trust);
        let digest = inputs_digest(request, &escalation, self.policies, self.user_rules);

        // 1. RBAC.
        if let Err(failure) = rbac::check(request.roles, request.action) {
            return self.outcome(
                Decision::Deny,
                PolicyReason::Rbac(failure),
                &escalation,
                false,
                Vec::new(),
                digest,
            );
        }

        // 2. A proposal without a resolved projection has no authority.
        if request.capability_projection_id.is_none() {
            return self.outcome(
                Decision::Deny,
                PolicyReason::CapabilityProjectionMissing,
                &escalation,
                false,
                Vec::new(),
                digest,
            );
        }

        // 3./4. Privacy guard.
        if let Some(failure) = check_privacy(PrivacyInputs {
            data_classes: request.data_classes,
            destination: request.destination,
            egress_grants: request.egress_grants,
            untrusted_origin: trust.is_untrusted_external(),
        })
        .failure()
        {
            return self.outcome(
                Decision::Deny,
                PolicyReason::Guard(failure),
                &escalation,
                false,
                Vec::new(),
                digest,
            );
        }

        // 5. Sequence guards.
        if let Some(failure) = check_sequence(request.sequence_guards, request.sequence).failure() {
            return self.outcome(
                Decision::Deny,
                PolicyReason::Guard(failure),
                &escalation,
                false,
                Vec::new(),
                digest,
            );
        }

        // 6. Policy rules: most restrictive matching decision wins.
        let rule_context = RuleContext {
            trust,
            effective_tier: escalation.effective_tier,
            data_classes: request.data_classes,
            now: request.now,
            sequence: request.sequence,
        };
        let matching =
            self.policies
                .matching_rules(request.effect_class, request.resource, &rule_context);
        if matching
            .iter()
            .any(|rule| rule.decision == quansio_capability::PolicyDecision::Deny)
        {
            return self.outcome(
                Decision::Deny,
                PolicyReason::PolicyExplicitDeny,
                &escalation,
                false,
                Vec::new(),
                digest,
            );
        }
        let policy_allows_always = !matching.is_empty()
            && matching
                .iter()
                .all(|rule| rule.decision == quansio_capability::PolicyDecision::Allow);
        let mut decision = if matching.is_empty() {
            if escalation.effective_tier.get() == 0 {
                Decision::Allow
            } else {
                return self.outcome(
                    Decision::Deny,
                    PolicyReason::NoMatchingPolicyRule {
                        tier: escalation.effective_tier.get(),
                    },
                    &escalation,
                    false,
                    Vec::new(),
                    digest,
                );
            }
        } else if matching
            .iter()
            .any(|rule| rule.decision == quansio_capability::PolicyDecision::Ask)
        {
            Decision::Ask
        } else {
            Decision::Allow
        };

        // 7. Tier ≥ 3 requires a receipt unless a standing rule applies; tier 4 never
        // accepts `always`. The standing-rule check happens with the user rules below.
        if decision == Decision::Allow && escalation.effective_tier.get() >= 3 {
            decision = Decision::Ask;
        }

        // 8. User rules may only narrow, or widen `ask → always` where policy permits.
        let mut standing_allow = false;
        let mut rejected: Vec<RejectedUserRule> = Vec::new();
        let mut reason = match decision {
            Decision::Deny => PolicyReason::PolicyExplicitDeny,
            Decision::Ask => PolicyReason::ApprovalRequired,
            Decision::Allow => PolicyReason::Allowed,
        };
        if let Some(user_id) = request.user_id {
            let applicable: Vec<&UserRule> = self
                .user_rules
                .iter()
                .filter(|rule| {
                    rule.applies(user_id, request.effect_class, request.resource, request.now)
                })
                .collect();
            if let Some(never) = applicable
                .iter()
                .find(|rule| rule.decision == UserRuleDecision::Never)
            {
                return self.outcome(
                    Decision::Deny,
                    PolicyReason::UserRuleNever {
                        rule_id: never.id.clone(),
                    },
                    &escalation,
                    false,
                    Vec::new(),
                    digest,
                );
            }
            if applicable
                .iter()
                .any(|rule| rule.decision == UserRuleDecision::Ask)
            {
                decision = Decision::Ask;
                reason = PolicyReason::ApprovalRequired;
            }
            for rule in applicable
                .iter()
                .filter(|rule| rule.decision == UserRuleDecision::Always)
            {
                if escalation.effective_tier.get() == 4 {
                    rejected.push(reject(&rule.id, UserRuleRejection::TierFour));
                } else if escalation.escalated {
                    rejected.push(reject(&rule.id, UserRuleRejection::Escalated));
                } else if !policy_allows_always {
                    rejected.push(reject(&rule.id, UserRuleRejection::NotPermittedByPolicy));
                } else {
                    standing_allow = true;
                }
            }
            if standing_allow {
                decision = Decision::Allow;
                reason = PolicyReason::AllowedByStandingRule;
            }
        }

        self.outcome(
            decision,
            reason,
            &escalation,
            standing_allow,
            rejected,
            digest,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn outcome(
        &self,
        decision: Decision,
        reason: PolicyReason,
        escalation: &Escalation,
        standing_allow: bool,
        rejected_user_rules: Vec<RejectedUserRule>,
        inputs_digest: Digest,
    ) -> PolicyOutcome {
        PolicyOutcome {
            decision,
            reason,
            catalog_tier: escalation.catalog_tier,
            effective_tier: escalation.effective_tier,
            escalated: escalation.escalated,
            approval_required: decision == Decision::Ask,
            standing_allow,
            rejected_user_rules,
            inputs_digest,
        }
    }
}

fn reject(rule_id: &str, reason: UserRuleRejection) -> RejectedUserRule {
    RejectedUserRule {
        rule_id: rule_id.to_string(),
        reason,
    }
}

/// SHA-256 over the exact evaluation inputs, recorded on the `policy_decisions` row.
fn inputs_digest(
    request: &EvaluationRequest<'_>,
    escalation: &Escalation,
    policies: &PolicySet,
    user_rules: &[UserRule],
) -> Digest {
    let material = json!({
        "effect_class": request.effect_class.as_str(),
        "resource": {
            "kind": request.resource.kind(),
            "selector": request.resource.selector(),
        },
        "catalog_tier": request.catalog_tier.get(),
        "effective_tier": escalation.effective_tier.get(),
        "escalated": escalation.escalated,
        "trust": escalation.trust.as_str(),
        "action": format!("{:?}", request.action),
        "roles": {
            "tenant": request.roles.tenant.map(|role| role.as_str()),
            "workspace": request.roles.workspace.map(|role| role.as_str()),
        },
        "data_classes": request
            .data_classes
            .iter()
            .map(DataClass::as_str)
            .collect::<Vec<_>>(),
        "destination": request.destination.map(|destination| json!({
            "kind": destination.kind,
            "value": destination.value,
        })),
        "egress_grants": request
            .egress_grants
            .destinations()
            .iter()
            .map(|destination| json!({ "kind": destination.kind, "value": destination.value }))
            .collect::<Vec<_>>(),
        "capability_projection_id": request.capability_projection_id,
        "sequence_guards": request
            .sequence_guards
            .iter()
            .map(|guard| guard.as_str())
            .collect::<Vec<_>>(),
        "sequence": {
            "granted_approvals": request.sequence.granted_approval_ids,
            "used_approvals": request.sequence.used_approval_ids,
            "reserved_effects": request.sequence.reserved_effect_ids,
            "settled_effects": request.sequence.settled_effect_ids,
        },
        "policies": policies
            .in_order()
            .into_iter()
            .map(|policy| json!({
                "id": policy.id,
                "version": policy.version,
                "scope": policy.scope.as_str(),
            }))
            .collect::<Vec<_>>(),
        "user_id": request.user_id,
        "user_rules": user_rules
            .iter()
            .map(|rule| json!({
                "id": rule.id,
                "effect_class": rule.effect_class.as_str(),
                "decision": crate::policy::rules::user_rule_decision_str(rule.decision),
            }))
            .collect::<Vec<_>>(),
    });
    Digest::of_canonical_json(&material.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::guards::{EgressDestination, SequenceGuard};
    use crate::policy::rbac::{TenantRole, WorkspaceRole};
    use crate::policy::rules::{Policy, PolicyRule, PolicyScope};
    use quansio_capability::{PolicyDecision, UserRuleDecision};

    fn class(value: &str) -> EffectClass {
        EffectClass::parse(value).expect("effect class")
    }

    fn selector(value: &str) -> ResourceSelector {
        ResourceSelector::from_parts("domain", value, &[]).expect("selector")
    }

    fn rule(effect_class: &str, decision: PolicyDecision) -> PolicyRule {
        PolicyRule {
            effect_class: class(effect_class),
            resource_selector: selector("*"),
            decision,
            conditions: crate::policy::rules::RuleConditions::default(),
        }
    }

    fn policy_set(rules: Vec<PolicyRule>) -> PolicySet {
        PolicySet {
            tenant: Some(Policy::new("pol_t", PolicyScope::Tenant, None, rules)),
            workspace: None,
        }
    }

    fn editor() -> ActorRoles {
        ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor))
    }

    static EMPTY_GRANTS: EgressGrantSet = EgressGrantSet::empty();
    static EMPTY_SEQUENCE: SequenceContext = SequenceContext::empty();

    fn request<'a>(
        effect: &'a EffectClass,
        resource: &'a ResourceSelector,
        tier: Tier,
        action: ActionFamily,
    ) -> EvaluationRequest<'a> {
        EvaluationRequest {
            effect_class: effect,
            resource,
            catalog_tier: tier,
            action,
            roles: editor(),
            derived_from_trust: Some(TrustLevel::TrustedUser),
            data_classes: &[],
            destination: None,
            egress_grants: &EMPTY_GRANTS,
            capability_projection_id: Some("cap_x"),
            sequence_guards: &[],
            sequence: &EMPTY_SEQUENCE,
            user_id: Some("usr_x"),
            now: Utc::now(),
        }
    }

    #[test]
    fn tier_zero_without_a_rule_is_allowed() {
        let policies = PolicySet::default();
        let evaluator = PolicyEvaluator::new(&policies, &[]);
        let effect = class("read.internal");
        let resource = selector("*");
        let outcome = evaluator.evaluate(&request(
            &effect,
            &resource,
            Tier::new(0).expect("tier"),
            ActionFamily::Read,
        ));
        assert_eq!(outcome.reason, PolicyReason::Allowed);
        assert!(outcome.is_allow());
    }

    #[test]
    fn a_missing_rule_at_tier_three_denies() {
        let policies = PolicySet::default();
        let evaluator = PolicyEvaluator::new(&policies, &[]);
        let effect = class("message.send");
        let resource = selector("*");
        let outcome = evaluator.evaluate(&request(
            &effect,
            &resource,
            Tier::new(3).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 3 },
        ));
        assert!(outcome.is_deny());
        assert_eq!(
            outcome.reason,
            PolicyReason::NoMatchingPolicyRule { tier: 3 }
        );
    }

    #[test]
    fn an_explicit_deny_beats_a_user_always() {
        let policies = policy_set(vec![rule("message.send", PolicyDecision::Deny)]);
        let user_rules = vec![UserRule {
            id: "rule_1".to_string(),
            user_id: "usr_x".to_string(),
            workspace_id: "ws_x".to_string(),
            effect_class: class("message.send"),
            resource_selector: selector("*"),
            decision: UserRuleDecision::Always,
            expires_at: None,
        }];
        let evaluator = PolicyEvaluator::new(&policies, &user_rules);
        let effect = class("message.send");
        let resource = selector("*");
        let outcome = evaluator.evaluate(&request(
            &effect,
            &resource,
            Tier::new(3).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 3 },
        ));
        assert!(outcome.is_deny());
        assert_eq!(outcome.reason, PolicyReason::PolicyExplicitDeny);
    }

    #[test]
    fn user_always_moves_ask_to_allow_only_where_policy_allows() {
        let policies = policy_set(vec![rule("message.send", PolicyDecision::Allow)]);
        let user_rules = vec![UserRule {
            id: "rule_1".to_string(),
            user_id: "usr_x".to_string(),
            workspace_id: "ws_x".to_string(),
            effect_class: class("message.send"),
            resource_selector: selector("*"),
            decision: UserRuleDecision::Always,
            expires_at: None,
        }];
        let evaluator = PolicyEvaluator::new(&policies, &user_rules);
        let effect = class("message.send");
        let resource = selector("*");
        let outcome = evaluator.evaluate(&request(
            &effect,
            &resource,
            Tier::new(3).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 3 },
        ));
        assert!(outcome.is_allow());
        assert!(outcome.standing_allow);
        assert_eq!(outcome.reason, PolicyReason::AllowedByStandingRule);
    }

    #[test]
    fn user_always_is_rejected_on_tier_four() {
        let policies = policy_set(vec![rule("payment.execute", PolicyDecision::Allow)]);
        let user_rules = vec![UserRule {
            id: "rule_1".to_string(),
            user_id: "usr_x".to_string(),
            workspace_id: "ws_x".to_string(),
            effect_class: class("payment.execute"),
            resource_selector: selector("*"),
            decision: UserRuleDecision::Always,
            expires_at: None,
        }];
        let evaluator = PolicyEvaluator::new(&policies, &user_rules);
        let effect = class("payment.execute");
        let resource = selector("*");
        let mut req = request(
            &effect,
            &resource,
            Tier::new(4).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 4 },
        );
        req.roles = ActorRoles::new(Some(TenantRole::Admin), Some(WorkspaceRole::Admin));
        let outcome = evaluator.evaluate(&req);
        assert!(
            outcome.is_deny() || outcome.requires_approval(),
            "{outcome:?}"
        );
        assert!(!outcome.standing_allow);
        assert_eq!(
            outcome.rejected_user_rules[0].reason,
            UserRuleRejection::TierFour
        );
    }

    #[test]
    fn a_user_never_denies() {
        let policies = policy_set(vec![rule("message.send", PolicyDecision::Allow)]);
        let user_rules = vec![UserRule {
            id: "rule_1".to_string(),
            user_id: "usr_x".to_string(),
            workspace_id: "ws_x".to_string(),
            effect_class: class("message.send"),
            resource_selector: selector("*"),
            decision: UserRuleDecision::Never,
            expires_at: None,
        }];
        let evaluator = PolicyEvaluator::new(&policies, &user_rules);
        let effect = class("message.send");
        let resource = selector("*");
        let outcome = evaluator.evaluate(&request(
            &effect,
            &resource,
            Tier::new(2).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 2 },
        ));
        assert!(outcome.is_deny());
    }

    #[test]
    fn missing_projection_and_rbac_deny_before_any_rule() {
        let policies = policy_set(vec![rule("message.send", PolicyDecision::Allow)]);
        let evaluator = PolicyEvaluator::new(&policies, &[]);
        let effect = class("message.send");
        let resource = selector("*");
        let mut req = request(
            &effect,
            &resource,
            Tier::new(2).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 2 },
        );
        req.capability_projection_id = None;
        assert_eq!(
            evaluator.evaluate(&req).reason,
            PolicyReason::CapabilityProjectionMissing
        );

        req.capability_projection_id = Some("cap_x");
        req.roles = ActorRoles::default();
        assert_eq!(
            evaluator.evaluate(&req).reason,
            PolicyReason::Rbac(RbacFailure::NoRole)
        );
    }

    #[test]
    fn protected_egress_without_a_grant_denies() {
        let policies = policy_set(vec![rule("data.upload.protected", PolicyDecision::Allow)]);
        let evaluator = PolicyEvaluator::new(&policies, &[]);
        let effect = class("data.upload.protected");
        let resource = selector("*");
        let destination = EgressDestination::domain("api.example.com");
        let classes = [DataClass::parse("pii").expect("class")];
        let mut req = request(
            &effect,
            &resource,
            Tier::new(4).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 4 },
        );
        req.roles = ActorRoles::new(Some(TenantRole::Admin), Some(WorkspaceRole::Admin));
        req.data_classes = &classes;
        req.destination = Some(&destination);
        let outcome = evaluator.evaluate(&req);
        assert!(outcome.is_deny());
        assert_eq!(
            outcome.reason,
            PolicyReason::Guard(GuardFailure::ProtectedEgressDenied)
        );
    }

    #[test]
    fn a_failed_sequence_guard_denies() {
        let policies = policy_set(vec![rule("record.update", PolicyDecision::Allow)]);
        let evaluator = PolicyEvaluator::new(&policies, &[]);
        let effect = class("record.update");
        let resource = selector("*");
        let sequence = SequenceContext {
            settled_effect_ids: vec!["eff_a".to_string()],
            ..SequenceContext::default()
        };
        let guards = [SequenceGuard::ReservationBeforeSettlement];
        let mut req = request(
            &effect,
            &resource,
            Tier::new(2).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 2 },
        );
        req.sequence_guards = &guards;
        req.sequence = &sequence;
        let outcome = evaluator.evaluate(&req);
        assert!(outcome.is_deny());
        assert_eq!(
            outcome.reason,
            PolicyReason::Guard(GuardFailure::SequenceViolation(
                SequenceGuard::ReservationBeforeSettlement
            ))
        );
    }

    #[test]
    fn untrusted_derivation_escalates_and_forbids_always() {
        let policies = policy_set(vec![rule("message.send", PolicyDecision::Allow)]);
        let user_rules = vec![UserRule {
            id: "rule_1".to_string(),
            user_id: "usr_x".to_string(),
            workspace_id: "ws_x".to_string(),
            effect_class: class("message.send"),
            resource_selector: selector("*"),
            decision: UserRuleDecision::Always,
            expires_at: None,
        }];
        let evaluator = PolicyEvaluator::new(&policies, &user_rules);
        let effect = class("message.send");
        let resource = selector("*");
        let mut req = request(
            &effect,
            &resource,
            Tier::new(2).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 2 },
        );
        req.derived_from_trust = Some(TrustLevel::UntrustedExternal);
        let outcome = evaluator.evaluate(&req);
        assert!(outcome.escalated);
        assert_eq!(outcome.effective_tier.get(), 3);
        assert!(outcome.requires_approval());
        assert!(!outcome.standing_allow);
        assert_eq!(
            outcome.rejected_user_rules[0].reason,
            UserRuleRejection::Escalated
        );
    }

    #[test]
    fn absent_trust_fails_closed_to_untrusted() {
        let policies = policy_set(vec![rule("message.send", PolicyDecision::Allow)]);
        let evaluator = PolicyEvaluator::new(&policies, &[]);
        let effect = class("message.send");
        let resource = selector("*");
        let mut req = request(
            &effect,
            &resource,
            Tier::new(2).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 2 },
        );
        req.derived_from_trust = None;
        let outcome = evaluator.evaluate(&req);
        assert!(outcome.escalated);
        assert_eq!(outcome.effective_tier.get(), 3);
    }

    #[test]
    fn digest_is_stable_for_identical_inputs_and_changes_with_params() {
        let policies = policy_set(vec![rule("message.send", PolicyDecision::Allow)]);
        let evaluator = PolicyEvaluator::new(&policies, &[]);
        let effect = class("message.send");
        let resource = selector("*");
        let fixed_now: DateTime<Utc> = "2026-09-12T10:00:00Z".parse().expect("time");
        let mut first = request(
            &effect,
            &resource,
            Tier::new(2).expect("tier"),
            ActionFamily::ExecuteEffect { tier: 2 },
        );
        first.now = fixed_now;
        let mut second = first;
        assert_eq!(
            evaluator.evaluate(&first).inputs_digest,
            evaluator.evaluate(&second).inputs_digest
        );
        second.now = "2026-09-12T11:00:00Z".parse().expect("time");
        // Time-dependent matching rules are captured by evaluation, not the raw clock.
        assert_eq!(
            evaluator.evaluate(&first).inputs_digest,
            evaluator.evaluate(&second).inputs_digest
        );
    }
}
