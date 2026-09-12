//! Policy rules and user rules (DOMAIN.md §7.3).
//!
//! Policy and user rules filter and narrow the running grant set during projection
//! assembly; they never add a grant. A policy is a source of *permission*, and a
//! user rule may only move an approval because policy permits it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CapabilityError;
use crate::grant::{EffectClass, Grant, Tier};
use crate::selector::ResourceSelector;

/// A policy rule decision (DOMAIN.md §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyDecision {
    /// The matching grant may proceed.
    Allow,
    /// The matching grant requires an approval receipt.
    Ask,
    /// The matching grant is refused.
    Deny,
}

/// One policy rule (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyRule {
    /// The effect class the rule governs.
    pub effect_class: EffectClass,
    /// The resource region the rule applies to.
    pub resource: ResourceSelector,
    /// The decision.
    pub decision: PolicyDecision,
    /// A rule-level tier ceiling, narrowing any grant it matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_max: Option<Tier>,
    /// Content-trust ceilings (DOMAIN.md §12); carried for completeness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_max: Vec<String>,
    /// Data classes the rule is restricted to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_classes: Vec<String>,
}

impl PolicyRule {
    /// Whether the rule governs a grant: same effect class and an overlapping region.
    #[must_use]
    pub fn matches(&self, grant: &Grant) -> bool {
        self.effect_class == grant.effect_class && self.resource.overlaps(&grant.resource)
    }
}

/// A tenant or workspace policy document (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDocument {
    /// The `pol_…` policy id.
    pub reference: String,
    /// The policy rules.
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
}

impl PolicyDocument {
    /// Build a policy document.
    #[must_use]
    pub fn new(reference: impl Into<String>, rules: Vec<PolicyRule>) -> Self {
        Self {
            reference: reference.into(),
            rules,
        }
    }

    /// Parse a policy document from canonical JSON.
    ///
    /// # Errors
    /// Returns [`CapabilityError::MalformedGrant`] when the document is not canonical;
    /// a caller resolves this fail-closed rather than applying a partial policy.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, CapabilityError> {
        serde_json::from_value(value.clone())
            .map_err(|error| CapabilityError::MalformedGrant(error.to_string()))
    }

    /// Whether any rule explicitly allows a grant (the only thing that permits a user
    /// rule to widen `ask` to `always`).
    #[must_use]
    pub fn explicitly_allows(&self, grant: &Grant) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.decision == PolicyDecision::Allow && rule.matches(grant))
    }
}

/// A user rule decision (DOMAIN.md §7.3): `ask`, `always` or `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRuleDecision {
    /// Keep the grant, requiring an approval receipt.
    Ask,
    /// Permit the grant under a standing rule, where policy allows it.
    Always,
    /// Refuse the grant.
    Never,
}

/// One user rule (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserRule {
    /// The `rule_…` rule id.
    pub id: String,
    /// The effect class the rule governs.
    pub effect_class: EffectClass,
    /// The resource region the rule applies to.
    pub resource: ResourceSelector,
    /// The decision.
    pub decision: UserRuleDecision,
    /// Instant after which the rule is void.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl UserRule {
    /// Whether the rule governs a grant and is active at `now`.
    #[must_use]
    pub fn matches(&self, grant: &Grant, now: DateTime<Utc>) -> bool {
        self.effect_class == grant.effect_class
            && self.resource.overlaps(&grant.resource)
            && self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}
