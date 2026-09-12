//! Budget limits, consumption and the inheritance rule (DOMAIN.md §13.2).
//!
//! Pure data and pure rules: parsing the stored JSON, computing what remains, deciding whether a
//! charge fits, and narrowing a child's request to what its parent has left. The durable effects
//! live in [`super::service`].

use serde_json::{Map, Value};

use super::BudgetError;

/// One metered dimension of a budget (DOMAIN.md §13.2 `limits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Meter {
    /// Model input plus output tokens.
    Tokens,
    /// Cost in minor currency units.
    CostMinorUnits,
    /// Wall-clock time.
    WallTimeMs,
    /// Tool calls.
    ToolCalls,
    /// Concurrent runs.
    Concurrency,
    /// Machine minutes on execution targets.
    MachineMinutes,
    /// Steps recorded by the turn loop.
    MaxSteps,
}

impl Meter {
    /// Every meter, in DOMAIN.md §13.2 order.
    pub const ALL: [Self; 7] = [
        Self::Tokens,
        Self::CostMinorUnits,
        Self::WallTimeMs,
        Self::ToolCalls,
        Self::Concurrency,
        Self::MachineMinutes,
        Self::MaxSteps,
    ];

    /// The JSON spelling of this meter.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::CostMinorUnits => "cost_minor_units",
            Self::WallTimeMs => "wall_time_ms",
            Self::ToolCalls => "tool_calls",
            Self::Concurrency => "concurrency",
            Self::MachineMinutes => "machine_minutes",
            Self::MaxSteps => "max_steps",
        }
    }

    /// Parse the JSON spelling.
    ///
    /// # Errors
    /// Returns the offending key when it is not a meter, so a typo in a budget cannot be ignored.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|meter| meter.as_str() == value)
            .ok_or_else(|| format!("{value:?} is not a budget meter"))
    }
}

impl std::fmt::Display for Meter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of limits, or a set of consumed amounts. Absent means unbounded (for limits) or zero
/// (for consumption).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MeterUsage {
    entries: Vec<(Meter, u64)>,
}

impl MeterUsage {
    /// An empty usage set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one meter.
    pub fn set(&mut self, meter: Meter, value: u64) {
        match self
            .entries
            .iter_mut()
            .find(|(existing, _)| *existing == meter)
        {
            Some(slot) => slot.1 = value,
            None => self.entries.push((meter, value)),
        }
    }

    /// The value of one meter.
    #[must_use]
    pub fn get(&self, meter: Meter) -> u64 {
        self.entries
            .iter()
            .find(|(existing, _)| *existing == meter)
            .map(|(_, value)| *value)
            .unwrap_or(0)
    }

    /// Whether the meter is set at all.
    #[must_use]
    pub fn has(&self, meter: Meter) -> bool {
        self.entries.iter().any(|(existing, _)| *existing == meter)
    }

    /// The meters, in canonical order.
    #[must_use]
    pub fn entries(&self) -> &[(Meter, u64)] {
        &self.entries
    }

    /// Render as the stored JSON object.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        for meter in Meter::ALL {
            if self.has(meter) {
                object.insert(meter.as_str().to_string(), Value::from(self.get(meter)));
            }
        }
        Value::Object(object)
    }

    /// Parse the stored JSON object.
    ///
    /// # Errors
    /// Returns [`BudgetError::Malformed`] for an unknown meter or a non-integer value.
    pub fn parse(budget_id: &str, value: &Value) -> Result<Self, BudgetError> {
        let malformed = |detail: String| BudgetError::Malformed {
            id: budget_id.to_string(),
            detail,
        };
        let Some(object) = value.as_object() else {
            return Err(malformed("meter sets must be JSON objects".to_string()));
        };
        // Every key must be a meter, then the set is built in canonical order so two parsed sets
        // that describe the same usage compare equal whatever order the JSON happened to store.
        for key in object.keys() {
            Meter::parse(key).map_err(malformed)?;
        }
        let mut usage = Self::new();
        for meter in Meter::ALL {
            if let Some(raw) = object.get(meter.as_str()) {
                let amount = raw.as_u64().ok_or_else(|| {
                    malformed(format!("{} must be a non-negative integer", meter.as_str()))
                })?;
                usage.set(meter, amount);
            }
        }
        Ok(usage)
    }
}

/// A budget's limits.
pub type BudgetLimits = MeterUsage;
/// A budget's consumed amounts.
pub type BudgetConsumed = MeterUsage;

/// What remains for one meter: `None` means unbounded.
#[must_use]
pub fn remaining(limits: &BudgetLimits, consumed: &BudgetConsumed, meter: Meter) -> Option<u64> {
    if !limits.has(meter) {
        return None;
    }
    Some(limits.get(meter).saturating_sub(consumed.get(meter)))
}

/// Whether charging `amount` more of `meter` fits within the limits.
#[must_use]
pub fn fits(limits: &BudgetLimits, consumed: &BudgetConsumed, meter: Meter, amount: u64) -> bool {
    match remaining(limits, consumed, meter) {
        None => true,
        Some(left) => amount <= left,
    }
}

/// The meter that a charge would exhaust, if any. Checked in canonical order so the reported
/// exhaustion is deterministic.
#[must_use]
pub fn first_exhausted(
    limits: &BudgetLimits,
    consumed: &BudgetConsumed,
    charges: &MeterUsage,
) -> Option<(Meter, u64, u64)> {
    for (meter, amount) in charges.entries() {
        if !fits(limits, consumed, *meter, *amount) {
            return Some((*meter, consumed.get(*meter), limits.get(*meter)));
        }
    }
    None
}

/// Every meter at or beyond its limit — the reason a budget is `exhausted`.
#[must_use]
pub fn exhausted_meters(limits: &BudgetLimits, consumed: &BudgetConsumed) -> Vec<Meter> {
    Meter::ALL
        .into_iter()
        .filter(|meter| remaining(limits, consumed, *meter) == Some(0))
        .collect()
}

/// The meters an exhausted budget reports, as strings (used in events and reports).
pub const EXHAUSTED_METERS: &str = "meters";

/// Apply a charge to a consumed set, saturating at [`u64::MAX`].
#[must_use]
pub fn applied(consumed: &BudgetConsumed, charges: &MeterUsage) -> BudgetConsumed {
    let mut next = consumed.clone();
    for (meter, amount) in charges.entries() {
        next.set(*meter, next.get(*meter).saturating_add(*amount));
    }
    next
}

/// Narrow a child's requested allowance to what its parent has left.
///
/// Returns the child limits unchanged when every meter fits, and the narrowed set plus the meters
/// that had to shrink otherwise — so a caller can refuse the grant instead of silently granting
/// less.
#[must_use]
pub fn narrow_to_parent(
    child: &BudgetLimits,
    parent_remaining: &BudgetLimits,
) -> (BudgetLimits, Vec<(Meter, u64, u64)>) {
    let mut narrowed = child.clone();
    let mut shrunk = Vec::new();
    for (meter, requested) in child.entries() {
        let Some(left) = remaining(parent_remaining, &BudgetConsumed::new(), *meter) else {
            continue;
        };
        if *requested > left {
            narrowed.set(*meter, left);
            shrunk.push((*meter, *requested, left));
        }
    }
    (narrowed, shrunk)
}

/// Whether a child allowance fits within the parent's remaining allowance (acceptance 2).
///
/// # Errors
/// Returns [`BudgetError::ExceedsParent`] naming the first meter that does not fit.
pub fn ensure_child_within_parent(
    child_id: &str,
    parent_id: &str,
    child: &BudgetLimits,
    parent_limits: &BudgetLimits,
    parent_consumed: &BudgetConsumed,
) -> Result<(), BudgetError> {
    for (meter, requested) in child.entries() {
        let Some(left) = remaining(parent_limits, parent_consumed, *meter) else {
            continue;
        };
        if *requested > left {
            return Err(BudgetError::ExceedsParent {
                child: child_id.to_string(),
                parent: parent_id.to_string(),
                meter: *meter,
                requested: *requested,
                remaining: left,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(pairs: &[(Meter, u64)]) -> BudgetLimits {
        let mut limits = BudgetLimits::new();
        for (meter, value) in pairs {
            limits.set(*meter, *value);
        }
        limits
    }

    fn consumed(pairs: &[(Meter, u64)]) -> BudgetConsumed {
        let mut used = BudgetConsumed::new();
        for (meter, value) in pairs {
            used.set(*meter, *value);
        }
        used
    }

    #[test]
    fn a_meter_set_round_trips_through_json() {
        let usage = limits(&[(Meter::Tokens, 1_000), (Meter::Concurrency, 2)]);
        let json = usage.to_json();
        assert_eq!(json["tokens"], serde_json::json!(1_000));
        assert_eq!(json["concurrency"], serde_json::json!(2));
        assert_eq!(MeterUsage::parse("bdg_1", &json).expect("parse"), usage);
        let error = MeterUsage::parse("bdg_1", &serde_json::json!({"tokenz": 1}))
            .expect_err("unknown meter");
        assert!(matches!(error, BudgetError::Malformed { .. }));
        assert_eq!(error.code(), "VALIDATION_SCHEMA");
        let error =
            MeterUsage::parse("bdg_1", &serde_json::json!({"tokens": -1})).expect_err("negative");
        assert!(matches!(error, BudgetError::Malformed { .. }));
    }

    #[test]
    fn an_absent_limit_is_unbounded_and_a_reached_one_is_exhausted() {
        let limits = limits(&[(Meter::ToolCalls, 3)]);
        let used = consumed(&[(Meter::ToolCalls, 2)]);
        assert_eq!(remaining(&limits, &used, Meter::ToolCalls), Some(1));
        assert_eq!(remaining(&limits, &used, Meter::Tokens), None);
        assert!(fits(&limits, &used, Meter::Tokens, 1_000_000));
        assert!(fits(&limits, &used, Meter::ToolCalls, 1));
        assert!(!fits(&limits, &used, Meter::ToolCalls, 2));
        let spent = consumed(&[(Meter::ToolCalls, 3)]);
        assert_eq!(exhausted_meters(&limits, &spent), vec![Meter::ToolCalls]);
        assert!(exhausted_meters(&limits, &used).is_empty());
    }

    #[test]
    fn the_first_exhausted_meter_is_reported_deterministically() {
        let limits = limits(&[(Meter::Tokens, 100), (Meter::ToolCalls, 1)]);
        let used = consumed(&[(Meter::Tokens, 90), (Meter::ToolCalls, 1)]);
        let mut charges = MeterUsage::new();
        charges.set(Meter::Tokens, 20);
        charges.set(Meter::ToolCalls, 1);
        let (meter, consumed_amount, limit) =
            first_exhausted(&limits, &used, &charges).expect("exhausted");
        assert_eq!(meter, Meter::Tokens, "canonical order decides the report");
        assert_eq!((consumed_amount, limit), (90, 100));
    }

    #[test]
    fn a_child_may_not_exceed_its_parent_remaining() {
        let parent_limits = limits(&[(Meter::Tokens, 1_000), (Meter::Concurrency, 4)]);
        let parent_used = consumed(&[(Meter::Tokens, 400)]);
        let child = limits(&[(Meter::Tokens, 600), (Meter::Concurrency, 2)]);
        ensure_child_within_parent(
            "bdg_child",
            "bdg_parent",
            &child,
            &parent_limits,
            &parent_used,
        )
        .expect("600 <= 600 remaining");

        let greedy = limits(&[(Meter::Tokens, 700)]);
        let error = ensure_child_within_parent(
            "bdg_child",
            "bdg_parent",
            &greedy,
            &parent_limits,
            &parent_used,
        )
        .expect_err("700 > 600 remaining");
        assert_eq!(error.code(), "VALIDATION_BOUNDS");
        assert!(matches!(
            error,
            BudgetError::ExceedsParent {
                meter: Meter::Tokens,
                requested: 700,
                remaining: 600,
                ..
            }
        ));

        // A meter the parent does not bound is not a constraint.
        let unbounded = limits(&[(Meter::MachineMinutes, 10_000)]);
        ensure_child_within_parent(
            "bdg_child",
            "bdg_parent",
            &unbounded,
            &parent_limits,
            &parent_used,
        )
        .expect("the parent sets no machine-minutes limit");
    }

    #[test]
    fn narrowing_reports_what_it_shrank() {
        let parent = limits(&[(Meter::Tokens, 100), (Meter::ToolCalls, 10)]);
        let child = limits(&[(Meter::Tokens, 500), (Meter::ToolCalls, 5)]);
        let (narrowed, shrunk) = narrow_to_parent(&child, &parent);
        assert_eq!(narrowed.get(Meter::Tokens), 100);
        assert_eq!(narrowed.get(Meter::ToolCalls), 5);
        assert_eq!(shrunk, vec![(Meter::Tokens, 500, 100)]);
    }

    #[test]
    fn a_charge_accumulates_per_meter() {
        let used = consumed(&[(Meter::Tokens, 10)]);
        let mut charges = MeterUsage::new();
        charges.set(Meter::Tokens, 5);
        charges.set(Meter::ToolCalls, 1);
        let after = applied(&used, &charges);
        assert_eq!(after.get(Meter::Tokens), 15);
        assert_eq!(after.get(Meter::ToolCalls), 1);
    }
}
