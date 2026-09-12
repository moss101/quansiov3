//! Scheduler, waits, durable timers and routine triggering (CORE-008, APP-010).
//!
//! This is the canonical runtime scheduler (DOSSIER.md §5). It owns *when* durable work
//! becomes due for one tenant and nothing else: Run transitions go through the graph
//! store via [`RunDispatch`], state transitions are recorded through the canonical
//! RuntimeEvent store, and no feature module grows its own polling loop.
//!
//! * [`TimerStore`] — durable `wait.timer` timers in PostgreSQL, leased with
//!   `FOR UPDATE SKIP LOCKED` and fired exactly once with a canonical `run.resumed`
//!   event committed in the same transaction that clears the wait.
//! * [`WaitRegistry`] — register/resolve waits for approvals, questions, events,
//!   children and timers (DOMAIN.md §5.7); a mismatched resolution fails closed.
//! * [`RoutineScheduler`] — cron/interval/event timing, `next_due_at`, absence policy and
//!   Objective creation through the canonical command path (DOMAIN.md §13.1).
//! * [`Scheduler`] — one bounded, backpressured tick over all of the above, plus the
//!   canonical loop.

pub mod dispatch;
pub mod engine;
pub mod routine;
pub mod timer;
pub mod wait;

use quansio_events::EventError;

pub use dispatch::{
    DispatchOutcome, RoutineFireOutcome, RoutineFireRequest, RunDispatch, RunDispatchRequest,
    RunResumeRequest,
};
pub use engine::{
    QueuedRun, Scheduler, SchedulerConfig, TickReport, DEFAULT_CLAIM_LEASE_SECONDS,
    DEFAULT_DUE_QUEUE_CAPACITY, DEFAULT_MAX_ROUTINES_PER_TICK, DEFAULT_MAX_RUNS_PER_TICK,
    DEFAULT_MAX_TIMERS_PER_TICK,
};
pub use routine::{
    AbsencePolicy, Routine, RoutineScheduler, RoutineTickReport, RoutineTrigger, RoutineTriggerKind,
};
pub use timer::{
    ClaimedTimer, DurableTimer, FenceReason, FireOutcome, NewTimer, TimerStatus, TimerStore,
};
pub use wait::{WaitError, WaitRegistry};

/// Errors returned by the scheduler.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// PostgreSQL rejected a statement.
    #[error("scheduler database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store rejected the commit.
    #[error("event store: {0}")]
    Event(#[from] EventError),
    /// The tenant context could not be established.
    #[error("schema: {0}")]
    Schema(#[from] crate::control::schema::SchemaError),
    /// The wait registry refused a register/resolve.
    #[error("wait registry: {0}")]
    Wait(#[from] WaitError),
    /// The due backlog exceeds the configured bound; nothing was claimed or dropped.
    #[error("scheduler queue is saturated: {pending} due items exceed the configured capacity {capacity}; nothing was dropped")]
    QueueSaturated {
        /// Observed due items.
        pending: i64,
        /// Configured bound.
        capacity: i64,
    },
    /// The timer was no longer claimable by this instance.
    #[error("timer {timer_id} is no longer claimable")]
    TimerNotClaimed {
        /// Timer identity.
        timer_id: String,
    },
    /// The timer does not exist in this tenant.
    #[error("timer {timer_id} not found")]
    TimerNotFound {
        /// Timer identity.
        timer_id: String,
    },
    /// The graph refused or failed a Run transition requested by the scheduler.
    #[error("run dispatch failed for {run_id}: {reason}")]
    RunDispatch {
        /// Run the transition targeted.
        run_id: String,
        /// Why it failed.
        reason: String,
    },
    /// A routine trigger could not be parsed or has no reachable occurrence.
    #[error("invalid routine trigger {spec:?}: {reason}")]
    InvalidTrigger {
        /// The rejected spec.
        spec: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The routine changed while it was being fired.
    #[error("routine {routine_id} is no longer active")]
    RoutineNotActive {
        /// Routine identity.
        routine_id: String,
    },
    /// A timezone is not resolvable by this timing seam.
    #[error("unsupported timezone {0:?}; use UTC or a fixed offset such as +02:00")]
    UnsupportedTimezone(String),
    /// A configuration or stored value is not valid.
    #[error("invalid scheduler configuration: {0}")]
    InvalidConfig(String),
}
