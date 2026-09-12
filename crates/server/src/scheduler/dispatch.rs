//! Run and objective dispatch port for the scheduler (DOMAIN.md §5.2, §13.1).
//!
//! The scheduler owns *when* work becomes due; the StateGraph owns the Run state machine
//! and the WorkGraph owns the Objective. So the scheduler does not write `runs`,
//! `work_nodes` or `agent_threads`: it hands a typed request to a [`RunDispatch`]
//! implementation, which performs the transition through the canonical graph store
//! (`quansio_graph::GraphStore`).
//!
//! The port exists because `quansio-graph` depends on `quansio-server` for
//! `control::schema`; `crates/server` therefore cannot depend on `quansio-graph` without
//! a package cycle. The implementation is supplied by the composition root (and by the
//! scheduler integration tests), so there is exactly one Run state machine and one
//! Objective creation path.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::SchedulerError;

/// A Run that is `QUEUED` and should start executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDispatchRequest {
    /// Owning tenant.
    pub tenant_id: String,
    /// Run to move `QUEUED → RUNNING`.
    pub run_id: String,
    /// Generation the dispatcher observed, used for fencing.
    pub generation: u64,
}

/// A fired timer waiting to move its Run `WAITING_TIMER → RUNNING`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResumeRequest {
    /// Owning tenant.
    pub tenant_id: String,
    /// Run that was parked on the timer.
    pub run_id: String,
    /// Generation the timer was scheduled under.
    pub generation: u64,
    /// Wait key the timer resolved.
    pub wait_key: String,
    /// Durable timer that fired.
    pub timer_id: String,
}

/// Result of asking the graph to move a Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The transition was applied.
    Dispatched,
    /// The Run was already in the target state; the request was a no-op.
    AlreadyRunning,
    /// The Run generation is newer than the request, so the transition was refused.
    Fenced,
    /// The Run is not in a state from which the requested transition is legal.
    NotDispatchable,
}

/// A Routine firing that must become an Objective plus its Run.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineFireRequest {
    /// Owning tenant.
    pub tenant_id: String,
    /// Workspace the Objective and Run belong to.
    pub workspace_id: String,
    /// Routine that fired (`rtn_…`).
    pub routine_id: String,
    /// User that owns the Routine.
    pub owner_user_id: String,
    /// Teammate the Routine runs as, when fixed.
    pub teammate_id: Option<String>,
    /// WorkNode template plus CompletionContract (DOMAIN.md §13.1), opaque JSON.
    pub objective_template: Value,
    /// The scheduled window this firing is for.
    pub fire_window: DateTime<Utc>,
    /// Stable identity of this firing for command-path idempotency.
    pub fire_key: String,
}

/// Identity of the work a Routine firing created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineFireOutcome {
    /// Created Objective WorkNode (`wn_…`).
    pub objective_id: String,
    /// Created Run (`run_…`).
    pub run_id: String,
}

/// Canonical transition port used by the scheduler.
///
/// Implementations must perform every transition through the graph store's transition
/// rules (or an equivalent single authority); the scheduler never issues a second state
/// machine of its own.
#[async_trait]
pub trait RunDispatch: Send + Sync {
    /// Apply `QUEUED → RUNNING` for a due Run.
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError>;

    /// Apply `WAITING_TIMER → RUNNING` for the Run a fired timer was parked on.
    async fn resume_timer_wait(
        &self,
        request: RunResumeRequest,
    ) -> Result<DispatchOutcome, SchedulerError>;

    /// Create the Objective and Run for a Routine firing through the canonical command
    /// path.
    async fn fire_routine(
        &self,
        request: RoutineFireRequest,
    ) -> Result<RoutineFireOutcome, SchedulerError>;
}
