//! Execution orchestration over the canonical WorkGraph (RUN-004, DOMAIN.md §4, §5.2, §13.2).
//!
//! Canonical owner: `crates/server/src/runtime/orchestration`. The orchestrator is the part
//! of the runtime that decides **which ready work may start**, **how much of it may run at
//! once**, **what a cancellation does to a subtree** and **when a join completes**. It is
//! not a second scheduler and not a second runtime: every run it starts goes through the
//! existing `RunDispatch` port into the canonical Run state machine, and every WorkGraph
//! change goes through a [`WorkGraphPort`] whose production implementation applies it as one
//! event-emitting graph transaction (`crates/graph`, CORE-005).
//!
//! Four invariants hold here:
//!
//! 1. **No dependent work starts before its prerequisites.** A node whose `depends_on`
//!    predecessor is not `done` is never selected, even when a Run is already queued for it
//!    (the refusal is typed, not a silent skip). `depends_on` reads `from` depends on `to`,
//!    so `to` is the prerequisite.
//! 2. **Bounded concurrency.** Concurrent work is capped by the budget's `concurrency` limit
//!    (DOMAIN.md §13.2) counted from *durable* run state, so the bound survives a restart. A
//!    saturated limit is reported, never exceeded.
//! 3. **Cancellation is idempotent and propagates without duplicating effects.** Cancelling a
//!    node cancels its `parent_id` subtree and blocks its dependents; each run goes through
//!    the one authoritative `RuntimeStore::cancel`, so a cancel storm produces one
//!    cancellation per run and never a second dispatch of a settled effect.
//! 4. **Joins complete once.** A parent waiting on its children is released exactly when the
//!    last child is `done`, and becomes `blocked` (never silently ready) if a child failed or
//!    was cancelled.

pub mod cancel;
pub mod capacity;
pub mod graph;
pub mod queue;
pub mod service;

use quansio_events::EventError;
use thiserror::Error;

pub use cancel::{CancelPlan, CancelRequest, CancellationOutcome, CancelledNode};
pub use capacity::{CapacityGate, CapacityRefusal};
pub use graph::{
    DependencyEdge, GraphSnapshot, NodeTransition, ReleaseBatch, ReleaseOutcome, RunRef,
    UnavailableWorkGraph, WorkGraphPort, WorkNodeView, GRAPH_TRANSACTION_OWNER,
};
pub use queue::{
    ensure_acyclic, join_plan, node_ids, project, release_plan, select, DeferralReason,
    DispatchCandidate, JoinPlan, ReleasePlan, Selection,
};
pub use service::{Orchestrator, TickReport, MAX_CANCEL_ATTEMPTS, MAX_REVISION_ATTEMPTS};

/// Repository path of this module's canonical owner.
pub const ORCHESTRATION_OWNER: &str = "crates/server/src/runtime/orchestration";

/// The node statuses orchestration reasons about (DOMAIN.md §4.1).
pub mod status {
    /// A node whose prerequisites are satisfied and that may be started.
    pub const READY: &str = "ready";
    /// A node that cannot run because a prerequisite cannot complete.
    pub const BLOCKED: &str = "blocked";
    /// A node the plan started.
    pub const IN_PROGRESS: &str = "in_progress";
    /// A node parked on children, a timer, a question or an approval.
    pub const WAITING: &str = "waiting";
    /// A node whose work is finished but not yet verified.
    pub const VERIFYING: &str = "verifying";
    /// A node that completed.
    pub const DONE: &str = "done";
    /// A node whose work failed.
    pub const FAILED: &str = "failed";
    /// A node that was cancelled.
    pub const CANCELLED: &str = "cancelled";
    /// A node that has not started yet.
    pub const DRAFT: &str = "draft";
}

/// A node status that can never change again.
#[must_use]
pub fn is_terminal_node_status(status: &str) -> bool {
    matches!(status, status::DONE | status::FAILED | status::CANCELLED)
}

/// A Run status that can never change again (DOMAIN.md §5.2).
#[must_use]
pub fn is_terminal_run_status(status: &str) -> bool {
    matches!(
        status,
        "SUCCEEDED" | "FAILED" | "CANCELLED" | "BLOCKED_UNRECOVERABLE"
    )
}

/// A Run status that is holding its execution slot.
#[must_use]
pub fn is_active_run_status(status: &str) -> bool {
    !is_terminal_run_status(status)
}

/// A refusal from the orchestration layer.
#[derive(Debug, Error)]
pub enum OrchestrationError {
    /// The WorkGraph port is not wired yet; the named task owns it.
    #[error("the WorkGraph port is not available: {detail}; owned by {owner}")]
    PortUnavailable {
        /// Owning task.
        owner: &'static str,
        /// What is missing.
        detail: String,
    },
    /// The concurrency budget admits no more work (DOMAIN.md §13.2).
    #[error("concurrency budget is exhausted: {in_flight} in flight, limit {limit}")]
    CapacitySaturated {
        /// Runs already holding a slot.
        in_flight: usize,
        /// The configured limit.
        limit: usize,
    },
    /// A node cannot start because a prerequisite has not completed.
    #[error("node {node_id} cannot start: prerequisite {prerequisite_id} is {status}")]
    PrerequisiteUnsatisfied {
        /// The node that was not selected.
        node_id: String,
        /// The prerequisite that is not `done`.
        prerequisite_id: String,
        /// The prerequisite's status.
        status: String,
    },
    /// A node transition is not legal in the WorkGraph state machine.
    #[error("illegal node transition for {node_id}: {from} -> {to}")]
    IllegalNodeTransition {
        /// Node identity.
        node_id: String,
        /// Current status.
        from: String,
        /// Requested status.
        to: String,
    },
    /// The orchestrator observed a dependency cycle, which the graph must never contain.
    #[error("the WorkGraph contains a dependency cycle through {node_id}")]
    DependencyCycle {
        /// A node on the cycle.
        node_id: String,
    },
    /// A batch lost the graph revision race and must be recomputed.
    #[error("graph revision conflict for {workspace_id}: expected {expected}, current {current}")]
    RevisionConflict {
        /// Workspace whose graph moved.
        workspace_id: String,
        /// Revision the batch was built against.
        expected: u64,
        /// Revision the graph now holds.
        current: u64,
    },
    /// The requested entity does not exist for this tenant.
    #[error("{entity} {id} was not found for this tenant")]
    NotFound {
        /// Entity kind.
        entity: &'static str,
        /// Entity identity.
        id: String,
    },
    /// The run dispatch seam refused the dispatch.
    #[error("run {run_id} could not be dispatched: {reason}")]
    NotDispatchable {
        /// Run that stayed queued.
        run_id: String,
        /// Why.
        reason: String,
    },
    /// A runtime transition or store failure.
    #[error(transparent)]
    Runtime(#[from] crate::runtime::state_machine::RuntimeError),
    /// A scheduler failure while dispatching a queued run.
    #[error(transparent)]
    Scheduler(#[from] crate::scheduler::SchedulerError),
    /// An identity or generation primitive refusal.
    #[error(transparent)]
    Core(#[from] quansio_core::CoreError),
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// An event-store failure.
    #[error(transparent)]
    Event(#[from] EventError),
}

impl OrchestrationError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PortUnavailable { .. } => "INTERNAL",
            Self::CapacitySaturated { .. } => "BUDGET_EXHAUSTED",
            Self::PrerequisiteUnsatisfied { .. } => "CONFLICT_STATE",
            Self::IllegalNodeTransition { .. } => "RUNTIME_ILLEGAL_TRANSITION",
            Self::DependencyCycle { .. } => "VALIDATION_SCHEMA",
            Self::RevisionConflict { .. } => "CONFLICT_REVISION",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::NotDispatchable { .. } => "CONFLICT_STATE",
            Self::Runtime(error) => error.code(),
            Self::Core(_) => "VALIDATION_SCHEMA",
            Self::Scheduler(_) | Self::Database(_) | Self::Event(_) => "INTERNAL",
        }
    }
}
