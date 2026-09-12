//! `Policy` and `UserRule` documents with typed conditions (DOMAIN.md §7.3).
//!
//! A policy is a tenant- or workspace-scope rule set. A workspace policy can only
//! narrow its tenant policy; evaluation therefore takes the most restrictive decision
//! across all matching rules (deny beats ask beats allow) rather than letting a
//! lower-scope `allow` undo a higher-scope restriction. A rule whose conditions do not
//! hold is not a match, and a rule document that cannot be parsed is a hard failure:
//! evaluation must deny rather than silently drop the rule that might have denied.

use chrono::{DateTime, Timelike, Utc};
use quansio_capability::{EffectClass, PolicyDecision, ResourceSelector, Tier, UserRuleDecision};
use serde::{Deserialize, Serialize};

use crate::policy::error::PolicyError;
use crate::policy::escalation::TrustLevel;
use crate::policy::guards::{DataClass, SequenceContext, SequenceGuard};

/// The scope a policy applies at (DOMAIN.md §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyScope {
    /// Tenant-wide policy.
    Tenant,
    /// Workspace policy, which can only narrow the tenant policy.
    Workspace,
}

impl PolicyScope {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Workspace => "workspace",
        }
    }

    /// Parse a canonical scope.
    ///
    /// # Errors
    /// Returns the rejected value when it is not `tenant` or `workspace`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "tenant" => Ok(Self::Tenant),
            "workspace" => Ok(Self::Workspace),
            other => Err(other.to_string()),
        }
    }
}

/// A `HH:MM`–`HH:MM` daily window in UTC (DOMAIN.md §7.3 `conditions.time_window`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeWindow {
    /// Inclusive start minute of day (`HH:MM`).
    pub start: String,
    /// Exclusive end minute of day (`HH:MM`); a window may wrap midnight.
    pub end: String,
}

impl TimeWindow {
    /// Build a window.
    #[must_use]
    pub fn new(start: impl Into<String>, end: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            end: end.into(),
        }
    }

    /// Whether `now` falls inside the window.
    ///
    /// An unparseable bound is not a match: the condition fails closed.
    #[must_use]
    pub fn holds(&self, now: DateTime<Utc>) -> bool {
        let (Some(start), Some(end)) = (minute_of_day(&self.start), minute_of_day(&self.end))
        else {
            return false;
        };
        let current = now.hour() * 60 + now.minute();
        if start <= end {
            current >= start && current < end
        } else {
            current >= start || current < end
        }
    }
}

fn minute_of_day(value: &str) -> Option<u32> {
    let (hours, minutes) = value.split_once(':')?;
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    if hours < 24 && minutes < 60 {
        Some(hours * 60 + minutes)
    } else {
        None
    }
}

/// Rule conditions (DOMAIN.md §7.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuleConditions {
    /// The rule applies only when the derivation is at most this untrusted.
    pub trust_max: Option<TrustLevel>,
    /// The rule applies only when the effective tier is at most this.
    pub tier_max: Option<Tier>,
    /// The rule applies only when every proposal data class is in this set.
    pub data_classes: Vec<DataClass>,
    /// The rule applies only inside this UTC window.
    pub time_window: Option<TimeWindow>,
    /// Sequence guards the rule requires.
    pub sequence_guards: Vec<SequenceGuard>,
}

/// The facts a rule's conditions are evaluated against.
#[derive(Debug, Clone, Copy)]
pub struct RuleContext<'a> {
    /// Most-untrusted derivation of the proposal.
    pub trust: TrustLevel,
    /// Effective tier after escalation.
    pub effective_tier: Tier,
    /// Data classes carried by the proposal.
    pub data_classes: &'a [DataClass],
    /// When the decision is made.
    pub now: DateTime<Utc>,
    /// Durable sequence facts.
    pub sequence: &'a SequenceContext,
}

impl RuleConditions {
    /// Whether every condition holds for `context`.
    ///
    /// A condition that cannot be proven to hold is treated as not holding, so a rule
    /// can only ever fail to apply — never apply more widely than written.
    #[must_use]
    pub fn hold(&self, context: &RuleContext<'_>) -> bool {
        if let Some(trust_max) = self.trust_max {
            if context.trust.rank() > trust_max.rank() {
                return false;
            }
        }
        if let Some(tier_max) = self.tier_max {
            if context.effective_tier > tier_max {
                return false;
            }
        }
        if !self.data_classes.is_empty()
            && !context
                .data_classes
                .iter()
                .all(|class| self.data_classes.contains(class))
        {
            return false;
        }
        if let Some(window) = &self.time_window {
            if !window.holds(context.now) {
                return false;
            }
        }
        self.sequence_guards
            .iter()
            .all(|guard| guard.holds(context.sequence))
    }
}

/// One policy rule (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    /// The effect class the rule governs.
    pub effect_class: EffectClass,
    /// The resource region the rule applies to.
    #[serde(alias = "resource")]
    pub resource_selector: ResourceSelector,
    /// The decision the rule produces.
    pub decision: PolicyDecision,
    /// Conditions under which the rule applies.
    #[serde(default)]
    pub conditions: RuleConditions,
}

impl PolicyRule {
    /// Whether the rule governs this effect class and resource region.
    #[must_use]
    pub fn matches(&self, effect_class: &EffectClass, resource: &ResourceSelector) -> bool {
        self.effect_class == *effect_class && self.resource_selector.overlaps(resource)
    }

    /// Whether the rule governs the proposal and all its conditions hold.
    #[must_use]
    pub fn applies(
        &self,
        effect_class: &EffectClass,
        resource: &ResourceSelector,
        context: &RuleContext<'_>,
    ) -> bool {
        self.matches(effect_class, resource) && self.conditions.hold(context)
    }
}

/// A tenant- or workspace-scope policy document (DOMAIN.md §7.3 `policies` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The `pol_…` policy id.
    pub id: String,
    /// The scope the policy applies at.
    pub scope: PolicyScope,
    /// Owning workspace, when the scope is workspace.
    pub workspace_id: Option<String>,
    /// The typed rules.
    pub rules: Vec<PolicyRule>,
    /// Default TTL for human questions.
    pub question_default_ttl_seconds: i64,
    /// Default TTL for approval requests.
    pub approval_default_ttl_seconds: i64,
    /// Maximum plan nodes.
    pub max_plan_nodes: i32,
    /// Policy version.
    pub version: i32,
}

impl Policy {
    /// Build a policy from its typed parts.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        scope: PolicyScope,
        workspace_id: Option<String>,
        rules: Vec<PolicyRule>,
    ) -> Self {
        Self {
            id: id.into(),
            scope,
            workspace_id,
            rules,
            question_default_ttl_seconds: 86_400,
            approval_default_ttl_seconds: 3_600,
            max_plan_nodes: 25,
            version: 1,
        }
    }

    /// Parse the canonical `rules` JSONB array from the `policies` row (DOMAIN.md §7.3).
    ///
    /// # Errors
    /// Returns [`PolicyError::MalformedRule`] for any rule that is not canonical; the
    /// caller must deny rather than evaluate a partial policy.
    pub fn parse_rules(&self, value: &serde_json::Value) -> Result<Vec<PolicyRule>, PolicyError> {
        serde_json::from_value(value.clone()).map_err(|error| PolicyError::MalformedRule {
            policy_id: self.id.clone(),
            reason: error.to_string(),
        })
    }

    /// Return a copy whose rules are the parsed `rules` JSONB value.
    ///
    /// # Errors
    /// Returns [`PolicyError::MalformedRule`] when the rules cannot be parsed.
    pub fn with_parsed_rules(mut self, value: &serde_json::Value) -> Result<Self, PolicyError> {
        self.rules = self.parse_rules(value)?;
        Ok(self)
    }
}

/// The tenant and workspace policies effective for one decision (DOMAIN.md §7.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicySet {
    /// The tenant policy, when one exists.
    pub tenant: Option<Policy>,
    /// The workspace policy, when one exists.
    pub workspace: Option<Policy>,
}

impl PolicySet {
    /// The policies in evaluation order: tenant first, then workspace.
    #[must_use]
    pub fn in_order(&self) -> Vec<&Policy> {
        let mut policies = Vec::with_capacity(2);
        if let Some(policy) = &self.tenant {
            policies.push(policy);
        }
        if let Some(policy) = &self.workspace {
            policies.push(policy);
        }
        policies
    }

    /// All rules that govern the proposal and whose conditions hold.
    #[must_use]
    pub fn matching_rules(
        &self,
        effect_class: &EffectClass,
        resource: &ResourceSelector,
        context: &RuleContext<'_>,
    ) -> Vec<&PolicyRule> {
        self.in_order()
            .into_iter()
            .flat_map(|policy| policy.rules.iter())
            .filter(|rule| rule.applies(effect_class, resource, context))
            .collect()
    }

    /// The policy version tuple recorded on the decision row.
    #[must_use]
    pub fn versions(&self) -> Vec<(String, i32)> {
        self.in_order()
            .into_iter()
            .map(|policy| (policy.id.clone(), policy.version))
            .collect()
    }
}

/// One `user_rules` row (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRule {
    /// The `rule_…` rule id.
    pub id: String,
    /// Owning user.
    pub user_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// The effect class the rule governs.
    pub effect_class: EffectClass,
    /// The resource region the rule applies to.
    pub resource_selector: ResourceSelector,
    /// The decision (`ask | always | never`).
    pub decision: UserRuleDecision,
    /// Instant after which the rule is void.
    pub expires_at: Option<DateTime<Utc>>,
}

impl UserRule {
    /// Whether the rule governs this proposal for this user and is active at `now`.
    #[must_use]
    pub fn applies(
        &self,
        user_id: &str,
        effect_class: &EffectClass,
        resource: &ResourceSelector,
        now: DateTime<Utc>,
    ) -> bool {
        self.user_id == user_id
            && self.effect_class == *effect_class
            && self.resource_selector.overlaps(resource)
            && self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

/// Serialize a user-rule decision to its canonical stored and wire form.
#[must_use]
pub const fn user_rule_decision_str(decision: UserRuleDecision) -> &'static str {
    match decision {
        UserRuleDecision::Ask => "ask",
        UserRuleDecision::Always => "always",
        UserRuleDecision::Never => "never",
    }
}

/// Whether a `UserRule` decision is permitted for an effect class at `tier`.
///
/// Tier 4 never accepts `always` (DOMAIN.md §7.1); this is the single check used both
/// when storing a rule and when evaluating one, so a rule cannot be stored as effective
/// and then silently ignored.
///
/// # Errors
/// Returns [`PolicyError::TierFourAlwaysRejected`] when the rule is an illegal `always`.
pub fn validate_user_rule(
    rule_id: &str,
    effect_class: &EffectClass,
    decision: UserRuleDecision,
    tier: Tier,
) -> Result<(), PolicyError> {
    if decision == UserRuleDecision::Always && tier.get() == 4 {
        return Err(PolicyError::TierFourAlwaysRejected {
            rule_id: rule_id.to_string(),
            effect_class: effect_class.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quansio_capability::PolicyDecision;

    fn class(value: &str) -> EffectClass {
        EffectClass::parse(value).expect("effect class")
    }

    fn selector(value: &str) -> ResourceSelector {
        ResourceSelector::from_parts("domain", value, &[]).expect("selector")
    }

    #[test]
    fn scope_round_trips() {
        assert_eq!(
            PolicyScope::parse("tenant").expect("scope"),
            PolicyScope::Tenant
        );
        assert!(PolicyScope::parse("global").is_err());
    }

    #[test]
    fn time_window_handles_wrap_and_rejects_garbage() {
        let window = TimeWindow::new("22:00", "06:00");
        let inside = "2026-09-12T23:30:00Z"
            .parse::<DateTime<Utc>>()
            .expect("time");
        let outside = "2026-09-12T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("time");
        assert!(window.holds(inside));
        assert!(!window.holds(outside));
        assert!(!TimeWindow::new("25:00", "06:00").holds(inside));
        assert!(!TimeWindow::new("oops", "06:00").holds(inside));
    }

    #[test]
    fn conditions_fail_closed_on_unlisted_data_class() {
        let conditions = RuleConditions {
            data_classes: vec![DataClass::parse("pii").expect("class")],
            ..RuleConditions::default()
        };
        let sequence = SequenceContext::default();
        let now = Utc::now();
        let listed = [DataClass::parse("pii").expect("class")];
        let unlisted = [DataClass::parse("public").expect("class")];
        let holds = conditions.hold(&RuleContext {
            trust: TrustLevel::TrustedUser,
            effective_tier: Tier::new(2).expect("tier"),
            data_classes: &listed,
            now,
            sequence: &sequence,
        });
        assert!(holds);
        let fails = conditions.hold(&RuleContext {
            trust: TrustLevel::TrustedUser,
            effective_tier: Tier::new(2).expect("tier"),
            data_classes: &unlisted,
            now,
            sequence: &sequence,
        });
        assert!(!fails);
    }

    #[test]
    fn rule_conditions_deny_above_tier_and_trust_ceilings() {
        let conditions = RuleConditions {
            tier_max: Some(Tier::new(2).expect("tier")),
            trust_max: Some(TrustLevel::AgentGenerated),
            ..RuleConditions::default()
        };
        let sequence = SequenceContext::default();
        let now = Utc::now();
        assert!(conditions.hold(&RuleContext {
            trust: TrustLevel::AgentGenerated,
            effective_tier: Tier::new(2).expect("tier"),
            data_classes: &[],
            now,
            sequence: &sequence,
        }));
        assert!(!conditions.hold(&RuleContext {
            trust: TrustLevel::UntrustedExternal,
            effective_tier: Tier::new(2).expect("tier"),
            data_classes: &[],
            now,
            sequence: &sequence,
        }));
        assert!(!conditions.hold(&RuleContext {
            trust: TrustLevel::AgentGenerated,
            effective_tier: Tier::new(3).expect("tier"),
            data_classes: &[],
            now,
            sequence: &sequence,
        }));
    }

    #[test]
    fn canonical_rule_json_parses_and_unknown_fields_fail_closed() {
        let policy = Policy::new("pol_x", PolicyScope::Tenant, None, Vec::new());
        let value = serde_json::json!([{
            "effect_class": "message.send",
            "resource_selector": {"kind": "domain", "selector": "*"},
            "decision": "ask",
            "conditions": {"tier_max": 3}
        }]);
        let rules = policy.parse_rules(&value).expect("rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].decision, PolicyDecision::Ask);
        assert_eq!(rules[0].resource_selector, selector("*"));

        let unknown = serde_json::json!([{
            "effect_class": "message.send",
            "resource_selector": {"kind": "domain", "selector": "*"},
            "decision": "ask",
            "conditions": {"surprise": true}
        }]);
        assert!(policy.parse_rules(&unknown).is_err());
    }

    #[test]
    fn workspace_policy_can_only_narrow_tenant_policy() {
        let tenant = Policy::new(
            "pol_t",
            PolicyScope::Tenant,
            None,
            vec![PolicyRule {
                effect_class: class("message.send"),
                resource_selector: selector("*"),
                decision: PolicyDecision::Deny,
                conditions: RuleConditions::default(),
            }],
        );
        let workspace = Policy::new(
            "pol_w",
            PolicyScope::Workspace,
            Some("ws_x".to_string()),
            vec![PolicyRule {
                effect_class: class("message.send"),
                resource_selector: selector("*"),
                decision: PolicyDecision::Allow,
                conditions: RuleConditions::default(),
            }],
        );
        let set = PolicySet {
            tenant: Some(tenant),
            workspace: Some(workspace),
        };
        let sequence = SequenceContext::default();
        let context = RuleContext {
            trust: TrustLevel::TrustedUser,
            effective_tier: Tier::new(3).expect("tier"),
            data_classes: &[],
            now: Utc::now(),
            sequence: &sequence,
        };
        let matching = set.matching_rules(
            &class("message.send"),
            &selector("api.example.com"),
            &context,
        );
        // Both rules match; the caller takes the most restrictive (deny) decision.
        assert_eq!(matching.len(), 2);
        assert!(matching
            .iter()
            .any(|rule| rule.decision == PolicyDecision::Deny));
    }

    #[test]
    fn always_on_tier_four_is_rejected() {
        let payment = class("payment.execute");
        assert!(validate_user_rule(
            "rule_1",
            &payment,
            UserRuleDecision::Always,
            Tier::new(4).expect("tier")
        )
        .is_err());
        assert!(validate_user_rule(
            "rule_1",
            &payment,
            UserRuleDecision::Ask,
            Tier::new(4).expect("tier")
        )
        .is_ok());
        assert!(validate_user_rule(
            "rule_1",
            &class("message.send"),
            UserRuleDecision::Always,
            Tier::new(3).expect("tier")
        )
        .is_ok());
    }
}
