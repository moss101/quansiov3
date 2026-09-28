//! The budgets a run is governed by, enforced by the runtime (RUN-010, DOMAIN.md §13.2).
//!
//! A run is governed by the budget of its agent thread, of its workspace and of its tenant, each when
//! one was granted. The engine asks, before a model call or a tool dispatch, whether any of them is
//! exhausted — and starts nothing new when one is — and charges each of them with what the call used.

use async_trait::async_trait;
use sqlx::PgPool;

use super::limits::{exhausted_meters, BudgetConsumed, BudgetLimits, Meter};
use super::service::{BudgetRecord, BudgetService};
use super::{BudgetError, MeterUsage};
use crate::runtime::state_machine::{BudgetPort, Run, RuntimeError, RuntimeIdentity};
use crate::usage::{admit_effect, QuotaLimit, QuotaMeter, QuotaPolicy, QuotaUsage};

/// The runtime's budget port over [`BudgetService`].
#[derive(Clone)]
pub struct RuntimeBudgets {
    service: BudgetService,
}

impl RuntimeBudgets {
    /// Budgets of the identity's tenant.
    ///
    /// # Errors
    /// Returns a runtime error when the tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        Ok(Self {
            service: BudgetService::new(pool, identity).map_err(runtime_error)?,
        })
    }

    /// The budgets governing `run`, innermost first.
    async fn chain(&self, run: &Run) -> Result<Vec<BudgetRecord>, RuntimeError> {
        let agent_thread = run.agent_thread_id.to_string();
        let mut chain = Vec::with_capacity(3);
        for (scope, scope_ref) in [
            ("agent_thread", Some(agent_thread.as_str())),
            ("workspace", Some(run.workspace_id.as_str())),
            ("tenant", None),
        ] {
            if let Some(budget) = self
                .service
                .for_scope(scope, scope_ref)
                .await
                .map_err(runtime_error)?
            {
                chain.push(budget);
            }
        }
        Ok(chain)
    }
}

#[async_trait]
impl BudgetPort for RuntimeBudgets {
    async fn exhausted(&self, run: &Run) -> Result<Option<String>, RuntimeError> {
        Ok(self.chain(run).await?.into_iter().find_map(|budget| {
            (budget.status == "exhausted"
                || !exhausted_meters(&budget.limits, &budget.consumed).is_empty())
            .then_some(budget.id)
        }))
    }

    async fn admit(&self, run: &Run, meter: Meter, requested: u64) -> Result<u64, RuntimeError> {
        let mut admitted = requested;
        for budget in self.chain(run).await? {
            if !budget.is_active() {
                return Ok(0);
            }
            if let Some(units) = admitted_units(&budget.limits, &budget.consumed, meter, admitted) {
                admitted = admitted.min(units);
            }
        }
        Ok(admitted)
    }

    async fn charge(&self, run: &Run, usage: &MeterUsage) -> Result<(), RuntimeError> {
        if usage.entries().iter().all(|(_, amount)| *amount == 0) {
            return Ok(());
        }
        for budget in self.chain(run).await? {
            self.service
                .record(&budget.id, usage)
                .await
                .map_err(runtime_error)?;
        }
        Ok(())
    }
}

impl QuotaMeter for Meter {
    fn name(self) -> &'static str {
        self.as_str()
    }
}

/// A budget's consumption as a quota ledger: its limits are the hard quota (DOMAIN.md §13.2).
#[derive(Debug, Clone)]
pub struct BudgetQuota {
    consumed: BudgetConsumed,
}

impl QuotaUsage for BudgetQuota {
    type Meter = Meter;
    type Delta = (Meter, u64);

    fn meter_of(delta: &(Meter, u64)) -> Meter {
        delta.0
    }

    #[allow(clippy::cast_precision_loss)]
    fn quantity_of(delta: &(Meter, u64)) -> f64 {
        delta.1 as f64
    }

    #[allow(clippy::cast_precision_loss)]
    fn total(&self, meter: Meter) -> f64 {
        self.consumed.get(meter) as f64
    }

    fn record(&mut self, delta: (Meter, u64)) {
        self.consumed
            .set(delta.0, self.consumed.get(delta.0).saturating_add(delta.1));
    }
}

/// How many of `requested` single units of `meter` one budget's hard quota admits, each
/// admitted through [`admit_effect`] before its effect may be reserved; `None` when the
/// budget sets no limit on the meter.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn admitted_units(
    limits: &BudgetLimits,
    consumed: &BudgetConsumed,
    meter: Meter,
    requested: u64,
) -> Option<u64> {
    if !limits.has(meter) {
        return None;
    }
    let policy = QuotaPolicy::new().with(QuotaLimit {
        meter,
        // The soft quota is announced by the budget service when a charge crosses it.
        soft: None,
        hard: Some(limits.get(meter) as f64),
    });
    let mut ledger = BudgetQuota {
        consumed: consumed.clone(),
    };
    let mut admitted = 0;
    while admitted < requested {
        let reserved = admit_effect(&mut ledger, &policy, (meter, 1), || {
            Ok::<(), std::convert::Infallible>(())
        });
        if reserved.is_err() {
            break;
        }
        admitted += 1;
    }
    Some(admitted)
}

fn runtime_error(error: BudgetError) -> RuntimeError {
    match error {
        BudgetError::Database(error) => RuntimeError::Database(error),
        BudgetError::Event(error) => RuntimeError::Event(error),
        other => RuntimeError::Database(sqlx::Error::Protocol(other.to_string())),
    }
}
