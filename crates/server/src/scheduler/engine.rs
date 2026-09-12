//! The canonical scheduler loop: one bounded, leased tick over a tenant's due work.
//!
//! [`Scheduler::tick`] is deliberately a function of durable state only: it reads due
//! timers, due routines and queued runs from PostgreSQL, fires what is due, and asks the
//! [`RunDispatch`] port to move Runs through the canonical state machine. Process restart
//! therefore loses no due work, and two instances racing on the same tick claim disjoint
//! timers (see [`crate::scheduler::TimerStore`]).
//!
//! Backpressure is explicit: every tick is bounded by the configured batch sizes, and a
//! due backlog larger than the configured queue capacity returns
//! [`SchedulerError::QueueSaturated`] before anything is claimed, so work is never
//! dropped silently and the scheduler's own memory stays proportional to one batch.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Utc;
use quansio_events::EventStore;
use sqlx::PgPool;

use super::dispatch::{DispatchOutcome, RunDispatch, RunDispatchRequest, RunResumeRequest};
use super::routine::RoutineScheduler;
use super::timer::{FireOutcome, TimerStore};
use super::SchedulerError;

/// Default claim lease for a scheduler instance, in seconds.
pub const DEFAULT_CLAIM_LEASE_SECONDS: f64 = 30.0;
/// Default maximum timers claimed and fired per tick.
pub const DEFAULT_MAX_TIMERS_PER_TICK: i64 = 64;
/// Default maximum routines evaluated per tick.
pub const DEFAULT_MAX_ROUTINES_PER_TICK: i64 = 32;
/// Default maximum queued runs dispatched per tick.
pub const DEFAULT_MAX_RUNS_PER_TICK: i64 = 32;
/// Default bound on the due backlog before the scheduler reports saturation.
pub const DEFAULT_DUE_QUEUE_CAPACITY: i64 = 1024;

/// Bounds and identity of one scheduler instance.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Stable identity of this instance; it is the timer lease owner.
    pub instance_id: String,
    /// Lease duration for a claimed timer, in seconds.
    pub claim_lease_seconds: f64,
    /// Timers claimed and fired per tick; memory is bounded by this.
    pub max_timers_per_tick: i64,
    /// Routines evaluated per tick.
    pub max_routines_per_tick: i64,
    /// Queued runs dispatched per tick.
    pub max_runs_per_tick: i64,
    /// Due backlog above this returns [`SchedulerError::QueueSaturated`].
    pub due_queue_capacity: i64,
}

impl SchedulerConfig {
    /// Configuration with the documented defaults for one instance.
    #[must_use]
    pub fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            claim_lease_seconds: DEFAULT_CLAIM_LEASE_SECONDS,
            max_timers_per_tick: DEFAULT_MAX_TIMERS_PER_TICK,
            max_routines_per_tick: DEFAULT_MAX_ROUTINES_PER_TICK,
            max_runs_per_tick: DEFAULT_MAX_RUNS_PER_TICK,
            due_queue_capacity: DEFAULT_DUE_QUEUE_CAPACITY,
        }
    }
}

/// What one tick did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Timers fired (each committed exactly one event).
    pub timers_fired: usize,
    /// Timers refused by fencing and cancelled.
    pub timers_fenced: usize,
    /// Timer claims lost to a competing instance.
    pub timers_claim_lost: usize,
    /// Fired timers whose run the graph would not resume.
    pub resumes_not_dispatchable: usize,
    /// Queued runs moved to `RUNNING`.
    pub runs_dispatched: usize,
    /// Queued runs the graph would not dispatch.
    pub runs_not_dispatchable: usize,
    /// Routine windows fired.
    pub routines_fired: usize,
    /// Routine windows skipped by policy.
    pub routines_skipped: usize,
    /// Routine windows fired from a backlog under the `queue` policy.
    pub routines_queued: usize,
}

/// A queued Run discovered for dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRun {
    /// Run identity.
    pub run_id: String,
    /// Generation observed when it was read.
    pub generation: u64,
}

/// The canonical runtime scheduler for one tenant.
pub struct Scheduler {
    tenant_id: String,
    config: SchedulerConfig,
    events: EventStore,
    dispatch: Arc<dyn RunDispatch>,
}

impl Scheduler {
    /// Build a scheduler over a pool, a configuration and the graph dispatch port.
    ///
    /// # Errors
    /// Returns a schema error when the tenant id is not a canonical `tn_` id.
    pub fn new(
        pool: PgPool,
        tenant_id: impl Into<String>,
        config: SchedulerConfig,
        dispatch: Arc<dyn RunDispatch>,
    ) -> Result<Self, SchedulerError> {
        let tenant_id = tenant_id.into();
        crate::control::schema::validate_tenant_id(&tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool),
            tenant_id,
            config,
            dispatch,
        })
    }

    /// The scheduler's configuration.
    #[must_use]
    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    /// The event store the scheduler commits through.
    #[must_use]
    pub fn events(&self) -> &EventStore {
        &self.events
    }

    /// Run one bounded tick.
    ///
    /// # Errors
    /// Returns [`SchedulerError::QueueSaturated`] when the due backlog exceeds the
    /// configured capacity (nothing is claimed in that case), and a typed error for any
    /// database, event or dispatch failure.
    pub async fn tick(&self) -> Result<TickReport, SchedulerError> {
        let now = Utc::now();
        let timers = TimerStore::new(self.events.pool().clone(), self.tenant_id.clone())?;
        let routines = RoutineScheduler::new(self.events.pool().clone(), self.tenant_id.clone())?;

        let due_timers = timers.due_count().await?;
        if due_timers > self.config.due_queue_capacity {
            return Err(SchedulerError::QueueSaturated {
                pending: due_timers,
                capacity: self.config.due_queue_capacity,
            });
        }
        let due_routines = routines.due_count(now).await?;
        if due_routines > self.config.due_queue_capacity {
            return Err(SchedulerError::QueueSaturated {
                pending: due_routines,
                capacity: self.config.due_queue_capacity,
            });
        }

        let mut report = TickReport::default();

        let routine_report = routines
            .tick(
                self.dispatch.as_ref(),
                now,
                self.config.max_routines_per_tick,
            )
            .await?;
        report.routines_fired = routine_report.fired;
        report.routines_skipped = routine_report.skipped;
        report.routines_queued = routine_report.queued;

        let claimed = timers
            .claim_due(
                &self.config.instance_id,
                self.config.claim_lease_seconds,
                self.config.max_timers_per_tick,
            )
            .await?;
        for timer in claimed {
            match timers.fire(&timer).await? {
                FireOutcome::Fired { .. } => {
                    report.timers_fired += 1;
                    let outcome = self
                        .dispatch
                        .resume_timer_wait(RunResumeRequest {
                            tenant_id: self.tenant_id.clone(),
                            run_id: timer.run_id.clone(),
                            generation: timer.generation,
                            wait_key: timer.wait_key.clone(),
                            timer_id: timer.id.clone(),
                        })
                        .await?;
                    if !matches!(
                        outcome,
                        DispatchOutcome::Dispatched | DispatchOutcome::AlreadyRunning
                    ) {
                        report.resumes_not_dispatchable += 1;
                    }
                }
                FireOutcome::Fenced { .. } => {
                    timers.cancel(&timer.id, "fenced").await?;
                    report.timers_fenced += 1;
                }
                FireOutcome::WaitMissing => {
                    timers.cancel(&timer.id, "wait_missing").await?;
                    report.timers_fenced += 1;
                }
                FireOutcome::ClaimLost => report.timers_claim_lost += 1,
            }
        }

        for run in self.queued_runs(self.config.max_runs_per_tick).await? {
            let outcome = self
                .dispatch
                .dispatch_queued(RunDispatchRequest {
                    tenant_id: self.tenant_id.clone(),
                    run_id: run.run_id,
                    generation: run.generation,
                })
                .await?;
            if outcome == DispatchOutcome::Dispatched {
                report.runs_dispatched += 1;
            } else {
                report.runs_not_dispatchable += 1;
            }
        }

        Ok(report)
    }

    /// Runs in `QUEUED`, oldest first, bounded by `limit`.
    ///
    /// This is read-only discovery; the transition itself goes through the dispatch port.
    ///
    /// # Errors
    /// Returns a scheduler error when the query fails.
    pub async fn queued_runs(&self, limit: i64) -> Result<Vec<QueuedRun>, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(
            "SELECT id, generation FROM runs WHERE tenant_id = $1 AND status = 'QUEUED' \
             ORDER BY created_at ASC, id LIMIT $2",
        )
        .bind(&self.tenant_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let runs = rows
            .iter()
            .map(|row| {
                use sqlx::Row;
                Ok(QueuedRun {
                    run_id: row.try_get("id")?,
                    generation: row.try_get::<i64, _>("generation")?.unsigned_abs(),
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        tx.commit().await?;
        Ok(runs)
    }

    /// Run ticks at a fixed interval until `stop` is set.
    ///
    /// # Errors
    /// Returns the first tick error. The caller owns the interval and the stop flag, so a
    /// supervisor can cancel the loop without a second scheduler.
    pub async fn run(
        &self,
        interval: StdDuration,
        stop: Arc<AtomicBool>,
    ) -> Result<(), SchedulerError> {
        loop {
            self.tick().await?;
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            tokio::time::sleep(interval).await;
        }
    }
}
