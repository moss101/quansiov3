//! Runtime budgets, quotas and capacity control (RUN-010, DOMAIN.md §13.2, §9.2 `usage.*`).
//!
//! A budget is explicit resource policy: `limits` per scope, `consumed` against them, and a
//! `parent_budget_id` when the scope is nested. Two rules make autonomy bounded rather than
//! hopeful:
//!
//! 1. **Child ≤ parent remaining.** A child budget may never be granted more than its parent has
//!    left, which is what stops a run from authorising itself an allowance its workspace does not
//!    have. A request beyond the parent is refused, never clamped silently.
//! 2. **Exhaustion stops new work and touches nothing in flight.** Consuming past a limit flips
//!    the budget to `exhausted`, emits a `usage.*` event and refuses the charge — the service
//!    writes only the budget row and its usage event, so an effect already reserved or dispatched
//!    is left for the Effect Ledger to settle or reconcile rather than being cancelled underneath
//!    it.
//!
//! The `concurrency` meter is the same limit RUN-004's orchestration capacity gate takes from its
//! caller ([`service::BudgetService::capacity_limit`]), so the bound the orchestrator enforces and
//! the bound policy states are one number.

pub mod limits;
pub mod service;

use thiserror::Error;

pub use limits::{BudgetConsumed, BudgetLimits, Meter, MeterUsage, EXHAUSTED_METERS};
pub use service::{BudgetService, BudgetSpec, ChargeOutcome};

/// Repository path of this module's canonical owner.
pub const BUDGETS_OWNER: &str = "crates/server/src/runtime/budgets";

/// A budget refusal.
#[derive(Debug, Error)]
pub enum BudgetError {
    /// The charge would exceed a limit; no work may start against this budget.
    #[error("budget {budget_id} is exhausted on {meter}: {consumed} of {limit} used")]
    Exhausted {
        /// Budget that ran out.
        budget_id: String,
        /// Meter that ran out.
        meter: Meter,
        /// Amount consumed.
        consumed: u64,
        /// The limit.
        limit: u64,
    },
    /// A child budget asked for more than its parent has left.
    #[error(
        "budget {child} requests {requested} {meter} but parent {parent} has {remaining} remaining"
    )]
    ExceedsParent {
        /// Child budget being created.
        child: String,
        /// Parent budget.
        parent: String,
        /// Meter over-requested.
        meter: Meter,
        /// Requested allowance.
        requested: u64,
        /// Parent's remaining allowance.
        remaining: u64,
    },
    /// The budget does not exist for this tenant.
    #[error("budget {id} was not found for this tenant")]
    NotFound {
        /// Budget identity.
        id: String,
    },
    /// The stored budget is malformed.
    #[error("budget {id} is malformed: {detail}")]
    Malformed {
        /// Budget identity.
        id: String,
        /// What is wrong.
        detail: String,
    },
    /// An event-store failure while recording usage.
    #[error(transparent)]
    Event(#[from] quansio_events::EventError),
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl BudgetError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Exhausted { .. } => "BUDGET_EXHAUSTED",
            Self::ExceedsParent { .. } => "VALIDATION_BOUNDS",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::Malformed { .. } => "VALIDATION_SCHEMA",
            Self::Event(_) | Self::Database(_) => "INTERNAL",
        }
    }
}
