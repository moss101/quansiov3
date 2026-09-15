//! Hard/soft quota policy over the usage projection. Billing stays a projection.

use super::meters::UsageMeter;
use super::project::{UsageDelta, UsageProjection};
use super::UsageError;

/// Soft and hard limits for one meter. Absent means unbounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuotaLimit {
    /// Meter.
    pub meter: UsageMeter,
    /// Crossing this emits a budget alert but still admits.
    pub soft: Option<f64>,
    /// Crossing this refuses; no effect is reserved.
    pub hard: Option<f64>,
}

/// Entitlement policy for a tenant/plan. Not runtime truth.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuotaPolicy {
    limits: Vec<QuotaLimit>,
}

impl QuotaPolicy {
    /// Empty policy: everything is allowed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one meter's limits.
    #[must_use]
    pub fn with(mut self, limit: QuotaLimit) -> Self {
        self.limits.retain(|existing| existing.meter != limit.meter);
        self.limits.push(limit);
        self
    }

    /// Look up a meter.
    #[must_use]
    pub fn limit(&self, meter: UsageMeter) -> Option<QuotaLimit> {
        self.limits.iter().copied().find(|item| item.meter == meter)
    }
}

/// What the gate decided. Alerts are not refusals.
#[derive(Debug, Clone, PartialEq)]
pub enum QuotaDecision {
    /// Under every limit.
    Allow,
    /// Soft limit crossed; the increment is still admitted.
    SoftAlert {
        /// Meter.
        meter: UsageMeter,
        /// Total after the increment.
        used: f64,
        /// Soft limit.
        soft: f64,
    },
    /// Hard limit would be exceeded; the increment is refused.
    HardDeny {
        /// Meter.
        meter: UsageMeter,
        /// Total that would result.
        used: f64,
        /// Hard limit.
        hard: f64,
    },
}

impl QuotaDecision {
    /// Whether an effect may be reserved.
    #[must_use]
    pub const fn admits(&self) -> bool {
        !matches!(self, Self::HardDeny { .. })
    }

    /// Whether a budget alert should fire.
    #[must_use]
    pub const fn alerts(&self) -> bool {
        matches!(self, Self::SoftAlert { .. })
    }
}

/// A budget alert (soft quota). Never mutates runtime/effect state.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetAlert {
    /// Meter that crossed its soft limit.
    pub meter: UsageMeter,
    /// Total after the increment.
    pub used: f64,
    /// Soft limit.
    pub soft: f64,
}

/// Evaluate `current + delta` against the policy without mutating anything.
#[must_use]
pub fn evaluate(
    projection: &UsageProjection,
    policy: &QuotaPolicy,
    delta: &UsageDelta,
) -> QuotaDecision {
    let used = projection.total(delta.meter) + delta.quantity;
    let Some(limit) = policy.limit(delta.meter) else {
        return QuotaDecision::Allow;
    };
    if let Some(hard) = limit.hard {
        if used > hard {
            return QuotaDecision::HardDeny {
                meter: delta.meter,
                used,
                hard,
            };
        }
    }
    if let Some(soft) = limit.soft {
        if used > soft {
            return QuotaDecision::SoftAlert {
                meter: delta.meter,
                used,
                soft,
            };
        }
    }
    QuotaDecision::Allow
}

/// Admit a usage increment, then (and only then) run `commit_effect`.
///
/// A hard-quota denial returns [`UsageError::QuotaExceeded`] and **does not** call
/// `commit_effect`, so a refused quota cannot partially commit an EffectRecord.
///
/// # Errors
/// [`UsageError::QuotaExceeded`] on hard deny; [`UsageError::Effect`] if commit fails
/// after admission (usage is not applied in that case either).
pub fn admit_effect<E>(
    projection: &mut UsageProjection,
    policy: &QuotaPolicy,
    delta: UsageDelta,
    mut commit_effect: impl FnMut() -> Result<(), E>,
) -> Result<QuotaDecision, UsageError>
where
    E: std::fmt::Display,
{
    let decision = evaluate(projection, policy, &delta);
    if let QuotaDecision::HardDeny { meter, used, hard } = &decision {
        return Err(UsageError::QuotaExceeded {
            meter: meter.as_str(),
            used: *used,
            hard: *hard,
        });
    }
    commit_effect().map_err(|error| UsageError::Effect {
        detail: error.to_string(),
    })?;
    projection.push(delta.into_record());
    Ok(decision)
}
