//! The budgets a run is governed by, enforced by the runtime (RUN-010, DOMAIN.md §13.2).
//!
//! A run is governed by the budget of its agent thread, of its workspace and of its tenant, each when
//! one was granted. The engine asks, before a model call or a tool dispatch, whether any of them is
//! exhausted — and starts nothing new when one is — and charges each of them with what the call used.

use async_trait::async_trait;
use sqlx::PgPool;

use super::limits::exhausted_meters;
use super::service::{BudgetRecord, BudgetService};
use super::{BudgetError, MeterUsage};
use crate::runtime::state_machine::{BudgetPort, Run, RuntimeError, RuntimeIdentity};

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

fn runtime_error(error: BudgetError) -> RuntimeError {
    match error {
        BudgetError::Database(error) => RuntimeError::Database(error),
        BudgetError::Event(error) => RuntimeError::Event(error),
        other => RuntimeError::Database(sqlx::Error::Protocol(other.to_string())),
    }
}
