//! Hard/soft quota policy over metered usage. Billing stays a projection.
//!
//! The gate is written once over [`QuotaUsage`]: the usage projection (UsageRecord meters,
//! DOMAIN.md §13.4) is one ledger it admits against, and a runtime budget's consumption (the
//! hard quota of DOMAIN.md §13.2, `runtime::budgets`) is another — the served runtime admits
//! each tool call against it before the call's effect is reserved.

use super::meters::UsageMeter;
use super::project::{UsageDelta, UsageProjection};
use super::UsageError;

/// A meter a quota can limit.
pub trait QuotaMeter: Copy + PartialEq + std::fmt::Debug {
    /// Canonical spelling, used in refusals.
    fn name(self) -> &'static str;
}

impl QuotaMeter for UsageMeter {
    fn name(self) -> &'static str {
        self.as_str()
    }
}

/// What a quota admits against: current totals per meter, and how an admitted increment is
/// recorded so the next admission sees it.
pub trait QuotaUsage {
    /// The meters this ledger counts.
    type Meter: QuotaMeter;
    /// One proposed increment.
    type Delta;

    /// The meter an increment charges.
    fn meter_of(delta: &Self::Delta) -> Self::Meter;
    /// How much it charges.
    fn quantity_of(delta: &Self::Delta) -> f64;
    /// The total used so far on `meter`.
    fn total(&self, meter: Self::Meter) -> f64;
    /// Record an admitted increment.
    fn record(&mut self, delta: Self::Delta);
}

impl QuotaUsage for UsageProjection {
    type Meter = UsageMeter;
    type Delta = UsageDelta;

    fn meter_of(delta: &UsageDelta) -> UsageMeter {
        delta.meter
    }

    fn quantity_of(delta: &UsageDelta) -> f64 {
        delta.quantity
    }

    fn total(&self, meter: UsageMeter) -> f64 {
        UsageProjection::total(self, meter)
    }

    fn record(&mut self, delta: UsageDelta) {
        self.push(delta.into_record());
    }
}

/// Soft and hard limits for one meter. Absent means unbounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuotaLimit<M = UsageMeter> {
    /// Meter.
    pub meter: M,
    /// Crossing this emits a budget alert but still admits.
    pub soft: Option<f64>,
    /// Crossing this refuses; no effect is reserved.
    pub hard: Option<f64>,
}

/// Entitlement policy for a tenant/plan. Not runtime truth.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaPolicy<M = UsageMeter> {
    limits: Vec<QuotaLimit<M>>,
}

impl<M> Default for QuotaPolicy<M> {
    fn default() -> Self {
        Self { limits: Vec::new() }
    }
}

impl<M: QuotaMeter> QuotaPolicy<M> {
    /// Empty policy: everything is allowed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one meter's limits.
    #[must_use]
    pub fn with(mut self, limit: QuotaLimit<M>) -> Self {
        self.limits.retain(|existing| existing.meter != limit.meter);
        self.limits.push(limit);
        self
    }

    /// Look up a meter.
    #[must_use]
    pub fn limit(&self, meter: M) -> Option<QuotaLimit<M>> {
        self.limits.iter().copied().find(|item| item.meter == meter)
    }
}

/// What the gate decided. Alerts are not refusals.
#[derive(Debug, Clone, PartialEq)]
pub enum QuotaDecision<M = UsageMeter> {
    /// Under every limit.
    Allow,
    /// Soft limit crossed; the increment is still admitted.
    SoftAlert {
        /// Meter.
        meter: M,
        /// Total after the increment.
        used: f64,
        /// Soft limit.
        soft: f64,
    },
    /// Hard limit would be exceeded; the increment is refused.
    HardDeny {
        /// Meter.
        meter: M,
        /// Total that would result.
        used: f64,
        /// Hard limit.
        hard: f64,
    },
}

impl<M> QuotaDecision<M> {
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
pub fn evaluate<U: QuotaUsage>(
    usage: &U,
    policy: &QuotaPolicy<U::Meter>,
    delta: &U::Delta,
) -> QuotaDecision<U::Meter> {
    let meter = U::meter_of(delta);
    let used = usage.total(meter) + U::quantity_of(delta);
    let Some(limit) = policy.limit(meter) else {
        return QuotaDecision::Allow;
    };
    if let Some(hard) = limit.hard {
        if used > hard {
            return QuotaDecision::HardDeny { meter, used, hard };
        }
    }
    if let Some(soft) = limit.soft {
        if used > soft {
            return QuotaDecision::SoftAlert { meter, used, soft };
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
pub fn admit_effect<U, E>(
    usage: &mut U,
    policy: &QuotaPolicy<U::Meter>,
    delta: U::Delta,
    mut commit_effect: impl FnMut() -> Result<(), E>,
) -> Result<QuotaDecision<U::Meter>, UsageError>
where
    U: QuotaUsage,
    E: std::fmt::Display,
{
    let decision = evaluate(usage, policy, &delta);
    if let QuotaDecision::HardDeny { meter, used, hard } = &decision {
        return Err(UsageError::QuotaExceeded {
            meter: meter.name(),
            used: *used,
            hard: *hard,
        });
    }
    commit_effect().map_err(|error| UsageError::Effect {
        detail: error.to_string(),
    })?;
    usage.record(delta);
    Ok(decision)
}
