//! Durable timers keyed to a Run/Step `wait.timer` (CORE-008, DOMAIN.md §5.7).
//!
//! A timer's due time lives in `durable_timers` (migration 0004), never in process
//! memory, so a restart resumes the same due work. Firing is exactly-once:
//!
//! * [`TimerStore::claim_due`] claims a bounded batch with `FOR UPDATE SKIP LOCKED` and a
//!   lease (`claim_owner`, `claimed_until`), so two scheduler instances never hold the
//!   same timer;
//! * [`TimerStore::fire`] performs one guarded `claimed → fired` update inside the same
//!   transaction that removes the wait from protocol state and stages the canonical
//!   `run.resumed` event, so a losing racer matches no row, stages no event and rolls
//!   back.
//!
//! Fencing: the timer carries the Run generation it was scheduled under. A fire whose Run
//! is no longer in `WAITING_TIMER`, or whose Run generation has moved on, is refused and
//! the timer is cancelled instead of waking obsolete work.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use quansio_core::{CorrelationId, Generation, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventError, EventStore, EventType};
use serde_json::json;
use sqlx::{PgPool, Row};

use super::wait::{register_in, resolve_in};
use super::SchedulerError;
use crate::runtime::protocol_state::{Wait, WaitKind};

/// Lifecycle of a durable timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerStatus {
    /// Waiting for its due time.
    Scheduled,
    /// Leased by one scheduler instance.
    Claimed,
    /// Fired; exactly one `run.resumed` event was committed with it.
    Fired,
    /// Cancelled or fenced before firing.
    Cancelled,
}

impl TimerStatus {
    /// Parse the stored form.
    ///
    /// # Errors
    /// Returns [`SchedulerError::InvalidConfig`] for a value the schema cannot produce.
    pub fn from_db_str(value: &str) -> Result<Self, SchedulerError> {
        match value {
            "scheduled" => Ok(Self::Scheduled),
            "claimed" => Ok(Self::Claimed),
            "fired" => Ok(Self::Fired),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(SchedulerError::InvalidConfig(format!(
                "unknown durable timer status {other:?}"
            ))),
        }
    }
}

/// A timer to persist for a run.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTimer {
    /// Run parked on the timer (`run_…`).
    pub run_id: String,
    /// Owning workspace, when known.
    pub workspace_id: Option<String>,
    /// Step that registered the wait, when known (`stp_…`).
    pub step_id: Option<String>,
    /// Wait key shared with the protocol-state wait entry.
    pub wait_key: String,
    /// The exact due time.
    pub due_at: DateTime<Utc>,
    /// Run generation the timer is fenced to.
    pub generation: Generation,
    /// Optional expiry for the wait entry, RFC 3339.
    pub expires_at: Option<String>,
}

/// A persisted durable timer.
#[derive(Debug, Clone, PartialEq)]
pub struct DurableTimer {
    /// Timer identity (`tmr_…`).
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: Option<String>,
    /// Parked run.
    pub run_id: String,
    /// Step that registered the wait.
    pub step_id: Option<String>,
    /// Wait key.
    pub wait_key: String,
    /// Exact due time.
    pub due_at: DateTime<Utc>,
    /// Current status.
    pub status: TimerStatus,
    /// Fencing generation.
    pub generation: u64,
    /// Lease owner while claimed.
    pub claim_owner: Option<String>,
    /// Lease expiry while claimed.
    pub claimed_until: Option<DateTime<Utc>>,
    /// When the timer fired.
    pub fired_at: Option<DateTime<Utc>>,
}

/// A timer leased by this scheduler instance.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedTimer {
    /// Timer identity.
    pub id: String,
    /// Owning workspace.
    pub workspace_id: Option<String>,
    /// Parked run.
    pub run_id: String,
    /// Step that registered the wait.
    pub step_id: Option<String>,
    /// Wait key.
    pub wait_key: String,
    /// Exact due time.
    pub due_at: DateTime<Utc>,
    /// Fencing generation.
    pub generation: u64,
    /// Lease owner that must still hold the claim when firing.
    pub claim_owner: String,
}

/// Why a fire was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceReason {
    /// The Run generation is newer than the timer's.
    StaleGeneration,
    /// The Run is not parked in `WAITING_TIMER`.
    RunNotWaiting,
}

/// Outcome of firing a claimed timer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FireOutcome {
    /// Fired: the wait was cleared and one event was committed.
    Fired {
        /// The committed `run.resumed` event identity.
        event_id: String,
    },
    /// Refused by the Run generation or state; the caller cancels the timer.
    Fenced {
        /// Why it was refused.
        reason: FenceReason,
    },
    /// The protocol state no longer held the wait the timer resolved.
    WaitMissing,
    /// Another instance holds (or already used) the claim.
    ClaimLost,
}

/// Durable timer store for one tenant.
#[derive(Debug, Clone)]
pub struct TimerStore {
    events: EventStore,
    tenant_id: String,
}

impl TimerStore {
    /// Bind a timer store to a tenant.
    ///
    /// # Errors
    /// Returns a schema error when the tenant id is not a canonical `tn_` id.
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Result<Self, SchedulerError> {
        let tenant_id = tenant_id.into();
        crate::control::schema::validate_tenant_id(&tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool),
            tenant_id,
        })
    }

    /// Persist a timer, register its wait and emit `run.waiting` in one transaction.
    ///
    /// # Errors
    /// Returns a wait error when the run already waits on the same key or has no protocol
    /// state to park in, and a scheduler error for any database failure.
    pub async fn schedule(&self, timer: NewTimer) -> Result<DurableTimer, SchedulerError> {
        let timer_id = format!("tmr_{}", UlidGenerator::new().generate());
        let tenant_id = self.tenant_id.clone();
        let timer_id_in = timer_id.clone();
        let abort: Arc<Mutex<Option<SchedulerError>>> = Arc::new(Mutex::new(None));
        let abort_in = Arc::clone(&abort);

        let result = self
            .events
            .commit_mutation(&self.tenant_id, |conn, batch| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO durable_timers (id, tenant_id, workspace_id, run_id, step_id, \
                         wait_key, due_at, status, generation) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, 'scheduled', $8)",
                    )
                    .bind(&timer_id_in)
                    .bind(&tenant_id)
                    .bind(timer.workspace_id.as_deref())
                    .bind(&timer.run_id)
                    .bind(timer.step_id.as_deref())
                    .bind(&timer.wait_key)
                    .bind(timer.due_at)
                    .bind(timer.generation.get() as i64)
                    .execute(&mut *conn)
                    .await?;

                    let wait = Wait {
                        kind: WaitKind::Timer,
                        key: timer.wait_key.clone(),
                        expires_at: timer.expires_at.clone(),
                    };
                    if let Err(error) = register_in(conn, &tenant_id, &timer.run_id, wait).await {
                        set_abort(&abort_in, SchedulerError::Wait(error));
                        return Ok(());
                    }

                    let mut generator = UlidGenerator::new();
                    let mut draft = EventDraft::new(
                        "run",
                        timer.run_id.clone(),
                        timer.generation.get(),
                        EventType::parse("run.waiting")?,
                        CorrelationId::generate(&mut generator),
                        Actor::system("scheduler"),
                    )
                    .with_generation(timer.generation)
                    .with_payload(json!({
                        "wait_kind": "timer",
                        "wait_key": timer.wait_key,
                        "timer_id": timer_id_in,
                        "due_at": timer.due_at.to_rfc3339(),
                    }));
                    if let Some(workspace_id) = timer.workspace_id.clone() {
                        draft = draft.with_workspace(workspace_id);
                    }
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await;

        let abort = abort.lock().expect("scheduler abort mutex").take();
        match (result, abort) {
            (Ok(()), None) => {}
            (Err(EventError::NoEventStaged), Some(error)) => return Err(error),
            (Err(error), _) => return Err(error.into()),
            (Ok(()), Some(error)) => return Err(error),
        }
        self.get(&timer_id).await
    }

    /// Claim up to `limit` due timers for `owner` with a lease.
    ///
    /// Rows are selected with `FOR UPDATE SKIP LOCKED`, so concurrent scheduler instances
    /// claim disjoint sets; an expired lease can be reclaimed, which is safe because the
    /// old holder's fire is guarded by `claim_owner`.
    ///
    /// # Errors
    /// Returns a scheduler error when the claim transaction fails.
    pub async fn claim_due(
        &self,
        owner: &str,
        lease_seconds: f64,
        limit: i64,
    ) -> Result<Vec<ClaimedTimer>, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(
            "UPDATE durable_timers SET status = 'claimed', claim_owner = $1, \
             claimed_until = now() + make_interval(secs => $2) \
             WHERE tenant_id = $3 AND due_at <= now() \
               AND (status = 'scheduled' OR (status = 'claimed' AND claimed_until < now())) \
               AND id IN ( \
                   SELECT id FROM durable_timers \
                   WHERE tenant_id = $3 AND due_at <= now() \
                     AND (status = 'scheduled' OR (status = 'claimed' AND claimed_until < now())) \
                   ORDER BY due_at ASC LIMIT $4 FOR UPDATE SKIP LOCKED \
               ) \
             RETURNING id, workspace_id, run_id, step_id, wait_key, due_at, generation, claim_owner",
        )
        .bind(owner)
        .bind(lease_seconds)
        .bind(&self.tenant_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let claimed = rows
            .iter()
            .map(|row| {
                Ok(ClaimedTimer {
                    id: row.try_get("id")?,
                    workspace_id: row.try_get("workspace_id")?,
                    run_id: row.try_get("run_id")?,
                    step_id: row.try_get("step_id")?,
                    wait_key: row.try_get("wait_key")?,
                    due_at: row.try_get("due_at")?,
                    generation: row.try_get::<i64, _>("generation")?.unsigned_abs(),
                    claim_owner: row.try_get("claim_owner")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        tx.commit().await?;
        Ok(claimed)
    }

    /// Fire a claimed timer: clear its wait and commit exactly one `run.resumed` event.
    ///
    /// # Errors
    /// Returns a scheduler error when the transaction fails.
    pub async fn fire(&self, timer: &ClaimedTimer) -> Result<FireOutcome, SchedulerError> {
        let tenant_id = self.tenant_id.clone();
        let timer_id = timer.id.clone();
        let run_id = timer.run_id.clone();
        let wait_key = timer.wait_key.clone();
        let workspace_id = timer.workspace_id.clone();
        let claim_owner = timer.claim_owner.clone();
        let due_at = timer.due_at;
        let generation = timer.generation;
        let abort: Arc<Mutex<Option<FireAbort>>> = Arc::new(Mutex::new(None));
        let abort_in = Arc::clone(&abort);
        let event_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let event_id_in = Arc::clone(&event_id);

        let result = self
            .events
            .commit_mutation(&self.tenant_id, |conn, batch| {
                Box::pin(async move {
                    // Fence against the Run before mutating anything: a cancelled, moved-on
                    // or non-parked Run must not be woken.
                    let run = sqlx::query(
                        "SELECT generation, status FROM runs WHERE id = $1 AND tenant_id = $2",
                    )
                    .bind(&run_id)
                    .bind(&tenant_id)
                    .fetch_optional(&mut *conn)
                    .await?;
                    let Some(run) = run else {
                        set_abort(&abort_in, FireAbort::RunNotWaiting);
                        return Ok(());
                    };
                    let run_generation = run.try_get::<i64, _>("generation")?.unsigned_abs();
                    let run_status: String = run.try_get("status")?;
                    if run_generation != generation {
                        set_abort(&abort_in, FireAbort::StaleGeneration);
                        return Ok(());
                    }
                    if run_status != "WAITING_TIMER" {
                        set_abort(&abort_in, FireAbort::RunNotWaiting);
                        return Ok(());
                    }

                    // Guarded claim -> fired. A competing racer matched no row, so it
                    // stages no event and this transaction rolls back.
                    let updated = sqlx::query(
                        "UPDATE durable_timers SET status = 'fired', fired_at = now(), \
                         claim_owner = NULL, claimed_until = NULL \
                         WHERE id = $1 AND tenant_id = $2 AND status = 'claimed' \
                           AND claim_owner = $3",
                    )
                    .bind(&timer_id)
                    .bind(&tenant_id)
                    .bind(&claim_owner)
                    .execute(&mut *conn)
                    .await?;
                    if updated.rows_affected() != 1 {
                        set_abort(&abort_in, FireAbort::ClaimLost);
                        return Ok(());
                    }

                    if resolve_in(conn, &tenant_id, &run_id, WaitKind::Timer, &wait_key)
                        .await
                        .is_err()
                    {
                        set_abort(&abort_in, FireAbort::WaitMissing);
                        return Ok(());
                    }

                    let generation = Generation::new(generation).map_err(|error| bridge(&error))?;
                    let mut generator = UlidGenerator::new();
                    let mut draft = EventDraft::new(
                        "run",
                        run_id.clone(),
                        generation.get(),
                        EventType::parse("run.resumed")?,
                        CorrelationId::generate(&mut generator),
                        Actor::system("scheduler"),
                    )
                    .with_generation(generation)
                    .with_payload(json!({
                        "wait_kind": "timer",
                        "wait_key": wait_key,
                        "timer_id": timer_id,
                        "due_at": due_at.to_rfc3339(),
                    }));
                    if let Some(workspace_id) = workspace_id.clone() {
                        draft = draft.with_workspace(workspace_id);
                    }
                    set_abort(&event_id_in, draft.event_id.to_string());
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await;

        let abort = abort.lock().expect("scheduler abort mutex").take();
        let event_id = event_id
            .lock()
            .expect("scheduler event mutex")
            .take()
            .unwrap_or_default();
        match (result, abort) {
            (Ok(()), None) => Ok(FireOutcome::Fired { event_id }),
            (Err(EventError::NoEventStaged), Some(reason)) => Ok(match reason {
                FireAbort::ClaimLost => FireOutcome::ClaimLost,
                FireAbort::StaleGeneration => FireOutcome::Fenced {
                    reason: FenceReason::StaleGeneration,
                },
                FireAbort::RunNotWaiting => FireOutcome::Fenced {
                    reason: FenceReason::RunNotWaiting,
                },
                FireAbort::WaitMissing => FireOutcome::WaitMissing,
            }),
            (Err(error), _) => Err(error.into()),
            (Ok(()), Some(reason)) => Err(SchedulerError::TimerNotClaimed {
                timer_id: format!("{}: {reason:?}", timer.id),
            }),
        }
    }

    /// Cancel a scheduled or claimed timer and clear its wait, when still registered.
    ///
    /// # Errors
    /// Returns a scheduler error when the transaction fails.
    pub async fn cancel(&self, timer_id: &str, reason: &str) -> Result<bool, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(
            "UPDATE durable_timers SET status = 'cancelled', cancel_reason = $1, \
             claim_owner = NULL, claimed_until = NULL \
             WHERE id = $2 AND tenant_id = $3 AND status IN ('scheduled', 'claimed') \
             RETURNING run_id, wait_key",
        )
        .bind(reason)
        .bind(timer_id)
        .bind(&self.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(false);
        };
        let run_id: String = row.try_get("run_id")?;
        let wait_key: String = row.try_get("wait_key")?;
        match resolve_in(
            &mut tx,
            &self.tenant_id,
            &run_id,
            WaitKind::Timer,
            &wait_key,
        )
        .await
        {
            Ok(_) | Err(super::wait::WaitError::NotRegistered { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Number of due, unclaimed timers in this tenant (the scheduler's bounded-queue
    /// pressure signal).
    ///
    /// # Errors
    /// Returns a scheduler error when the count query fails.
    pub async fn due_count(&self) -> Result<i64, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM durable_timers WHERE tenant_id = $1 AND due_at <= now() \
             AND status = 'scheduled'",
        )
        .bind(&self.tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(count)
    }

    /// Load one timer.
    ///
    /// # Errors
    /// Returns [`SchedulerError::TimerNotFound`] when it does not exist in this tenant.
    pub async fn get(&self, timer_id: &str) -> Result<DurableTimer, SchedulerError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(
            "SELECT id, tenant_id, workspace_id, run_id, step_id, wait_key, due_at, status, \
             generation, claim_owner, claimed_until, fired_at \
             FROM durable_timers WHERE id = $1 AND tenant_id = $2",
        )
        .bind(timer_id)
        .bind(&self.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let row = row.ok_or_else(|| SchedulerError::TimerNotFound {
            timer_id: timer_id.to_string(),
        })?;
        timer_from_row(&row)
    }
}

fn set_abort<T>(slot: &Arc<Mutex<Option<T>>>, value: T) {
    *slot.lock().expect("scheduler abort mutex") = Some(value);
}

#[derive(Debug, Clone, Copy)]
enum FireAbort {
    ClaimLost,
    StaleGeneration,
    RunNotWaiting,
    WaitMissing,
}

/// Bridge a non-event-store failure into the event closure's error type.
pub(crate) fn bridge(error: &dyn std::fmt::Display) -> EventError {
    EventError::Database(sqlx::Error::Protocol(error.to_string()))
}

fn timer_from_row(row: &sqlx::postgres::PgRow) -> Result<DurableTimer, SchedulerError> {
    let status: String = row.try_get("status")?;
    Ok(DurableTimer {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        run_id: row.try_get("run_id")?,
        step_id: row.try_get("step_id")?,
        wait_key: row.try_get("wait_key")?,
        due_at: row.try_get("due_at")?,
        status: TimerStatus::from_db_str(&status)?,
        generation: row.try_get::<i64, _>("generation")?.unsigned_abs(),
        claim_owner: row.try_get("claim_owner")?,
        claimed_until: row.try_get("claimed_until")?,
        fired_at: row.try_get("fired_at")?,
    })
}
