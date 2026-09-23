//! The firing ledger: one `routine_fires` row per fire key (DOMAIN.md §13.1).
//!
//! A fire moves `pending → running → fired`, or back to `pending` with a later
//! `next_attempt_at` after a failure, or to `failed` once [`MAX_FIRE_ATTEMPTS`] have failed.
//! The row is claimed with the routine's own bookkeeping in one transaction
//! ([`claim_in`]); executing it is leased ([`FireLedger::lease_due`]) so two scheduler
//! instances never execute the same fire at once, and a lease that expired (the executor
//! died) makes the fire claimable again. The steps an execution takes are recorded on the
//! row as it goes, so a resumed execution continues rather than repeats.

use chrono::{DateTime, Duration, Utc};
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

/// A fire that failed this many times is marked failed.
pub const MAX_FIRE_ATTEMPTS: i32 = 5;
/// How long an executor holds a fire before another may take it over.
pub const FIRE_LEASE_SECONDS: i64 = 120;

const COLUMNS: &str =
    "id, workspace_id, routine_id, fire_key, source, window_at, source_event_id, \
                       status, attempts, next_attempt_at, last_error, thread_id, message_id, \
                       objective_id, run_id";

/// Where a fire came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireSource {
    /// A scheduled window.
    Schedule,
    /// A matching RuntimeEvent.
    Event,
    /// `TriggerRoutineNow`.
    Manual,
}

impl FireSource {
    /// Column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Event => "event",
            Self::Manual => "manual",
        }
    }
}

/// A fire's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireStatus {
    /// Waiting for (another) execution attempt.
    Pending,
    /// Leased by an executor.
    Running,
    /// Its run exists and its turn ran.
    Fired,
    /// Every attempt failed.
    Failed,
}

impl FireStatus {
    fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "fired" => Self::Fired,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }

    /// Column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Fired => "fired",
            Self::Failed => "failed",
        }
    }
}

/// One fire.
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    /// `rfi_<ULID>` (carried opaquely).
    pub id: String,
    /// Workspace.
    pub workspace_id: String,
    /// Routine.
    pub routine_id: String,
    /// The unique key.
    pub fire_key: String,
    /// `schedule`, `event` or `manual`.
    pub source: String,
    /// The scheduled window, for a schedule fire.
    pub window_at: Option<DateTime<Utc>>,
    /// The RuntimeEvent, for an event fire.
    pub source_event_id: Option<String>,
    /// State.
    pub status: FireStatus,
    /// Execution attempts so far.
    pub attempts: i32,
    /// When the next attempt may start.
    pub next_attempt_at: DateTime<Utc>,
    /// The last failure.
    pub last_error: Option<String>,
    /// The routine's thread, once known.
    pub thread_id: Option<String>,
    /// The fire's message in that thread, once posted.
    pub message_id: Option<String>,
    /// The Objective WorkNode, once created.
    pub objective_id: Option<String>,
    /// The Run, once created.
    pub run_id: Option<String>,
}

impl Fire {
    /// The projection's spelling.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "routine_id": self.routine_id,
            "fire_key": self.fire_key,
            "source": self.source,
            "window_at": self.window_at,
            "source_event_id": self.source_event_id,
            "status": self.status.as_str(),
            "attempts": self.attempts,
            "next_attempt_at": self.next_attempt_at,
            "last_error": self.last_error,
            "thread_id": self.thread_id,
            "message_id": self.message_id,
            "objective_id": self.objective_id,
            "run_id": self.run_id,
        })
    }
}

/// The retry delay after `attempts` failures: 5 s doubling, capped at 10 minutes.
#[must_use]
pub fn backoff(attempts: i32) -> Duration {
    let exponent = u32::try_from(attempts.clamp(1, 16) - 1).unwrap_or(0);
    let seconds = 5_i64.saturating_mul(2_i64.saturating_pow(exponent));
    Duration::seconds(seconds.min(600))
}

/// Claim one fire inside the caller's transaction and stage `routine.fired`.
///
/// Returns `None` when the key was already claimed (a restart, a replayed event or another
/// instance got there first): nothing is staged and the caller moves on.
///
/// # Errors
/// Returns a database or event error.
#[allow(clippy::too_many_arguments)]
pub async fn claim_in(
    conn: &mut PgConnection,
    batch: &mut EventBatch,
    tenant_id: &str,
    workspace_id: &str,
    routine_id: &str,
    fire_key: &str,
    source: FireSource,
    window_at: Option<DateTime<Utc>>,
    source_event_id: Option<&str>,
) -> Result<Option<Fire>, EventError> {
    let id = format!("rfi_{}", UlidGenerator::new().generate());
    let row = sqlx::query(&format!(
        "INSERT INTO routine_fires (id, tenant_id, workspace_id, routine_id, fire_key, source, \
                                    window_at, source_event_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (tenant_id, fire_key) DO NOTHING RETURNING {COLUMNS}"
    ))
    .bind(&id)
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(routine_id)
    .bind(fire_key)
    .bind(source.as_str())
    .bind(window_at)
    .bind(source_event_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let fire = fire_from_row(&row).map_err(EventError::from)?;
    let version = next_version(conn, tenant_id, routine_id, batch).await?;
    batch.emit(
        EventDraft::new(
            "routine",
            routine_id.to_string(),
            version,
            EventType::parse("routine.fired")?,
            CorrelationId::generate(&mut UlidGenerator::new()),
            Actor::system("scheduler"),
        )
        .with_workspace(workspace_id.to_string())
        .with_payload(json!({
            "routine_id": routine_id,
            "fire_id": fire.id,
            "fire_key": fire.fire_key,
            "source": fire.source,
            "window": window_at.map(|at| at.to_rfc3339()),
            "source_event_id": source_event_id,
        })),
    );
    Ok(Some(fire))
}

/// The next `aggregate_version` for a routine, continuing committed and staged events.
///
/// # Errors
/// Returns a database error.
pub async fn next_version(
    conn: &mut PgConnection,
    tenant_id: &str,
    routine_id: &str,
    batch: &EventBatch,
) -> Result<u64, EventError> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(aggregate_version), 0) FROM runtime_events \
         WHERE tenant_id = $1 AND aggregate_type = 'routine' AND aggregate_id = $2",
    )
    .bind(tenant_id)
    .bind(routine_id)
    .fetch_one(&mut *conn)
    .await?;
    let staged = batch.staged_version("routine", routine_id).unwrap_or(0);
    Ok(u64::try_from(committed).unwrap_or(0).max(staged) + 1)
}

/// The firing ledger for one tenant.
#[derive(Debug, Clone)]
pub struct FireLedger {
    events: EventStore,
    tenant_id: String,
}

impl FireLedger {
    /// A ledger over the pool for one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// Lease up to `limit` fires that are due: pending with `next_attempt_at` passed, or
    /// running under a lease that expired. Each leased fire counts one attempt. "Passed" is
    /// judged by the later of the caller's clock and the database's, since a claim stamps
    /// the database's.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn lease_due(&self, now: DateTime<Utc>, limit: i64) -> Result<Vec<Fire>, EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "UPDATE routine_fires SET status = 'running', attempts = attempts + 1, \
                    lease_until = $2 + make_interval(secs => $3) \
             WHERE id IN ( \
                 SELECT id FROM routine_fires WHERE tenant_id = $1 \
                   AND ((status = 'pending' AND next_attempt_at <= GREATEST($2, now())) \
                        OR (status = 'running' AND lease_until < GREATEST($2, now()))) \
                 ORDER BY next_attempt_at, id LIMIT $4 FOR UPDATE SKIP LOCKED) \
             RETURNING {COLUMNS}"
        ))
        .bind(&self.tenant_id)
        .bind(now)
        .bind(FIRE_LEASE_SECONDS as f64)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|row| fire_from_row(row).map_err(EventError::from))
            .collect()
    }

    /// Record what an execution step produced, so a resumed execution continues from it.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn record_step(
        &self,
        fire_id: &str,
        thread_id: Option<&str>,
        message_id: Option<&str>,
        objective_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(), EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        sqlx::query(
            "UPDATE routine_fires SET thread_id = COALESCE($3, thread_id), \
                    message_id = COALESCE($4, message_id), objective_id = COALESCE($5, objective_id), \
                    run_id = COALESCE($6, run_id) \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(fire_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(objective_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Mark a fire done and stage `routine.fire_completed` with the run it produced.
    ///
    /// # Errors
    /// Returns a database or event error.
    pub async fn complete(
        &self,
        fire: &Fire,
        owner_user_id: &str,
        outcome: Value,
    ) -> Result<(), EventError> {
        let tenant = self.tenant_id.clone();
        let fire = fire.clone();
        let owner = owner_user_id.to_string();
        self.events
            .commit_mutation(&self.tenant_id, move |conn, batch| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE routine_fires SET status = 'fired', lease_until = NULL, last_error = NULL \
                         WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&tenant)
                    .bind(&fire.id)
                    .execute(&mut *conn)
                    .await?;
                    let version = next_version(conn, &tenant, &fire.routine_id, batch).await?;
                    let mut payload = json!({
                        "routine_id": fire.routine_id,
                        "fire_id": fire.id,
                        "fire_key": fire.fire_key,
                        "owner_user_id": owner,
                    });
                    if let (Value::Object(target), Value::Object(extra)) = (&mut payload, outcome) {
                        target.extend(extra);
                    }
                    batch.emit(
                        EventDraft::new(
                            "routine",
                            fire.routine_id.clone(),
                            version,
                            EventType::parse("routine.fire_completed")?,
                            CorrelationId::generate(&mut UlidGenerator::new()),
                            Actor::system("scheduler"),
                        )
                        .with_workspace(fire.workspace_id.clone())
                        .with_payload(payload),
                    );
                    Ok(())
                })
            })
            .await
    }

    /// Record a failed attempt: retry after [`backoff`], or — at [`MAX_FIRE_ATTEMPTS`] —
    /// mark the fire failed and stage `routine.fire_failed`. Returns whether it is final.
    ///
    /// # Errors
    /// Returns a database or event error.
    pub async fn fail(
        &self,
        fire: &Fire,
        owner_user_id: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, EventError> {
        let error = error.chars().take(2000).collect::<String>();
        if fire.attempts < MAX_FIRE_ATTEMPTS {
            let mut tx = self
                .events
                .begin_tenant_transaction(&self.tenant_id)
                .await?;
            sqlx::query(
                "UPDATE routine_fires SET status = 'pending', lease_until = NULL, last_error = $3, \
                        next_attempt_at = $4 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&self.tenant_id)
            .bind(&fire.id)
            .bind(&error)
            .bind(now + backoff(fire.attempts))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(false);
        }
        let tenant = self.tenant_id.clone();
        let fire = fire.clone();
        let owner = owner_user_id.to_string();
        self.events
            .commit_mutation(&self.tenant_id, move |conn, batch| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE routine_fires SET status = 'failed', lease_until = NULL, last_error = $3 \
                         WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&tenant)
                    .bind(&fire.id)
                    .bind(&error)
                    .execute(&mut *conn)
                    .await?;
                    let version = next_version(conn, &tenant, &fire.routine_id, batch).await?;
                    batch.emit(
                        EventDraft::new(
                            "routine",
                            fire.routine_id.clone(),
                            version,
                            EventType::parse("routine.fire_failed")?,
                            CorrelationId::generate(&mut UlidGenerator::new()),
                            Actor::system("scheduler"),
                        )
                        .with_workspace(fire.workspace_id.clone())
                        .with_payload(json!({
                            "routine_id": fire.routine_id,
                            "fire_id": fire.id,
                            "fire_key": fire.fire_key,
                            "attempts": fire.attempts,
                            "error": error,
                            "owner_user_id": owner,
                        })),
                    );
                    Ok(())
                })
            })
            .await?;
        Ok(true)
    }

    /// Give up on a fire without an attempt: its routine was paused, deleted or removed
    /// after the window was claimed. No notification: nothing failed.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn cancel(&self, fire: &Fire, reason: &str) -> Result<(), EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        sqlx::query(
            "UPDATE routine_fires SET status = 'failed', lease_until = NULL, last_error = $3 \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(&fire.id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The fires of one routine, newest first.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn fires_of(&self, routine_id: &str, limit: i64) -> Result<Vec<Fire>, EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM routine_fires WHERE tenant_id = $1 AND routine_id = $2 \
             ORDER BY created_at DESC, id DESC LIMIT $3"
        ))
        .bind(&self.tenant_id)
        .bind(routine_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|row| fire_from_row(row).map_err(EventError::from))
            .collect()
    }

    /// The routine's thread: the one its earlier fires used, if any.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn thread_of(&self, routine_id: &str) -> Result<Option<String>, EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let thread: Option<String> = sqlx::query_scalar(
            "SELECT thread_id FROM routine_fires WHERE tenant_id = $1 AND routine_id = $2 \
             AND thread_id IS NOT NULL ORDER BY created_at, id LIMIT 1",
        )
        .bind(&self.tenant_id)
        .bind(routine_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        tx.commit().await?;
        Ok(thread)
    }

    /// Load one fire by key.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn by_key(&self, fire_key: &str) -> Result<Option<Fire>, EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM routine_fires WHERE tenant_id = $1 AND fire_key = $2"
        ))
        .bind(&self.tenant_id)
        .bind(fire_key)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref()
            .map(|row| fire_from_row(row).map_err(EventError::from))
            .transpose()
    }
}

fn fire_from_row(row: &PgRow) -> Result<Fire, sqlx::Error> {
    let status: String = row.try_get("status")?;
    Ok(Fire {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        routine_id: row.try_get("routine_id")?,
        fire_key: row.try_get("fire_key")?,
        source: row.try_get("source")?,
        window_at: row.try_get("window_at")?,
        source_event_id: row.try_get("source_event_id")?,
        status: FireStatus::parse(&status),
        attempts: row.try_get("attempts")?,
        next_attempt_at: row.try_get("next_attempt_at")?,
        last_error: row.try_get("last_error")?,
        thread_id: row.try_get("thread_id")?,
        message_id: row.try_get("message_id")?,
        objective_id: row.try_get("objective_id")?,
        run_id: row.try_get("run_id")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_five_seconds_and_is_capped() {
        assert_eq!(backoff(1), Duration::seconds(5));
        assert_eq!(backoff(2), Duration::seconds(10));
        assert_eq!(backoff(4), Duration::seconds(40));
        assert_eq!(backoff(20), Duration::seconds(600));
        assert_eq!(backoff(0), Duration::seconds(5));
    }
}
