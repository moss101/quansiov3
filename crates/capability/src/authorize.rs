//! Authorized-decision API (DOMAIN.md §6.3, §7).
//!
//! [`authorize`] answers allow / ask / deny for one proposed effect against a
//! projection, naming the matched grant and the reason. A **stale** projection cannot
//! authorize dispatch: a changed `inputs_digest` or a passed `expires_at` returns
//! [`CapabilityError::StaleProjection`], forcing recomputation before any decision.

use chrono::{DateTime, Utc};

use crate::error::{CapabilityError, StaleProjection};
use crate::grant::{Approval, EffectClass, Grant, Tier};
use crate::layer::ProjectionInput;
use crate::projection::CapabilityProjection;
use crate::selector::ResourceSelector;

/// An authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    /// The effect may proceed.
    Allow,
    /// The effect requires an approval receipt before dispatch.
    Ask,
    /// The effect is refused.
    Deny,
}

impl Decision {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Deny => 0,
            Self::Ask => 1,
            Self::Allow => 2,
        }
    }
}

impl core::fmt::Display for Decision {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why [`authorize`] returned its decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorizationReason {
    /// A grant matched and permits the effect.
    MatchedGrant,
    /// No grant covers the effect class and resource.
    NoMatchingGrant,
    /// A grant matched but its `max_tier` is below the requested tier.
    TierAboveMaxTier,
    /// Every matching grant has expired.
    GrantExpired,
    /// The matched grant refuses the effect (`never`).
    ApprovalNever,
    /// The matched grant requires an approval receipt.
    ApprovalRequired,
    /// The matched grant is satisfied (`always`, or a receipt was supplied).
    ApprovalSatisfied,
    /// A tier-4 effect never accepts `always`; an approval receipt is required.
    TierFourRequiresApproval,
}

impl AuthorizationReason {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MatchedGrant => "matched_grant",
            Self::NoMatchingGrant => "no_matching_grant",
            Self::TierAboveMaxTier => "tier_above_max_tier",
            Self::GrantExpired => "grant_expired",
            Self::ApprovalNever => "approval_never",
            Self::ApprovalRequired => "approval_required",
            Self::ApprovalSatisfied => "approval_satisfied",
            Self::TierFourRequiresApproval => "tier_four_requires_approval",
        }
    }
}

impl core::fmt::Display for AuthorizationReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An authorization result: the decision, the matched grant and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    /// The decision.
    pub decision: Decision,
    /// The grant that produced it, when a grant matched.
    pub grant: Option<Grant>,
    /// Why the decision was reached.
    pub reason: AuthorizationReason,
}

/// One authorization request.
#[derive(Debug, Clone)]
pub struct AuthorizationRequest<'a> {
    /// The proposed effect class.
    pub effect_class: &'a EffectClass,
    /// The exact resource of the proposed effect.
    pub resource: &'a ResourceSelector,
    /// The consequence tier of the proposed effect.
    pub tier: Tier,
    /// The approval the caller already holds (`always` means a receipt is available).
    pub requested_approval: Approval,
    /// When the decision is made.
    pub now: DateTime<Utc>,
    /// The current projection inputs; when supplied, a digest mismatch is stale.
    pub current_inputs: Option<&'a [ProjectionInput]>,
}

impl<'a> AuthorizationRequest<'a> {
    /// Build a request at the current instant.
    #[must_use]
    pub fn new(
        effect_class: &'a EffectClass,
        resource: &'a ResourceSelector,
        tier: Tier,
        requested_approval: Approval,
    ) -> Self {
        Self {
            effect_class,
            resource,
            tier,
            requested_approval,
            now: Utc::now(),
            current_inputs: None,
        }
    }

    /// Fix the decision instant.
    #[must_use]
    pub fn with_now(mut self, now: DateTime<Utc>) -> Self {
        self.now = now;
        self
    }

    /// Supply the current inputs, enabling the `inputs_digest` staleness check.
    #[must_use]
    pub fn with_current_inputs(mut self, current_inputs: &'a [ProjectionInput]) -> Self {
        self.current_inputs = Some(current_inputs);
        self
    }
}

/// Authorize one proposed effect against a projection.
///
/// # Errors
/// Returns [`CapabilityError::StaleProjection`] when the projection is expired or its
/// `inputs_digest` no longer matches the current inputs; a stale projection must be
/// recomputed and cannot authorize dispatch.
pub fn authorize(
    projection: &CapabilityProjection,
    request: &AuthorizationRequest<'_>,
) -> Result<Authorization, CapabilityError> {
    if let Some(reason) = projection.staleness(request.current_inputs, request.now) {
        return Err(CapabilityError::StaleProjection(Box::new(
            StaleProjection {
                projection_id: projection.id.to_string(),
                reason,
            },
        )));
    }

    let matching: Vec<&Grant> = projection
        .grants
        .iter()
        .filter(|grant| {
            grant.effect_class == *request.effect_class && grant.resource.covers(request.resource)
        })
        .collect();
    if matching.is_empty() {
        return Ok(deny(None, AuthorizationReason::NoMatchingGrant));
    }

    let unexpired: Vec<&Grant> = matching
        .iter()
        .copied()
        .filter(|grant| {
            grant
                .constraints
                .expires_at
                .is_none_or(|expires_at| expires_at > request.now)
        })
        .collect();
    if unexpired.is_empty() {
        return Ok(deny(
            matching.first().map(|grant| (*grant).clone()),
            AuthorizationReason::GrantExpired,
        ));
    }

    let within_tier: Vec<&Grant> = unexpired
        .iter()
        .copied()
        .filter(|grant| Tier::rank(grant.constraints.max_tier) >= request.tier.get())
        .collect();
    if within_tier.is_empty() {
        return Ok(deny(
            unexpired.first().map(|grant| (*grant).clone()),
            AuthorizationReason::TierAboveMaxTier,
        ));
    }

    let mut best: Option<Authorization> = None;
    for grant in within_tier {
        let candidate = outcome_for(grant, request);
        if best
            .as_ref()
            .is_none_or(|current| candidate.decision.rank() > current.decision.rank())
        {
            best = Some(candidate);
        }
    }
    Ok(best.expect("at least one matching grant produced an outcome"))
}

fn outcome_for(grant: &Grant, request: &AuthorizationRequest<'_>) -> Authorization {
    let (approval, reason) = match grant.constraints.approval {
        Approval::Never => (Approval::Never, AuthorizationReason::ApprovalNever),
        Approval::Always if request.tier.get() >= crate::grant::MAX_TIER => {
            (Approval::Ask, AuthorizationReason::TierFourRequiresApproval)
        }
        Approval::Always => (Approval::Always, AuthorizationReason::ApprovalSatisfied),
        Approval::Ask => (Approval::Ask, AuthorizationReason::ApprovalRequired),
    };

    let decision = match approval {
        Approval::Never => Decision::Deny,
        Approval::Always => Decision::Allow,
        Approval::Ask if request.requested_approval == Approval::Always => Decision::Allow,
        Approval::Ask => Decision::Ask,
    };
    let reason = if decision == Decision::Allow && reason == AuthorizationReason::ApprovalRequired {
        AuthorizationReason::MatchedGrant
    } else {
        reason
    };
    Authorization {
        decision,
        grant: Some(grant.clone()),
        reason,
    }
}

fn deny(grant: Option<Grant>, reason: AuthorizationReason) -> Authorization {
    Authorization {
        decision: Decision::Deny,
        grant,
        reason,
    }
}
