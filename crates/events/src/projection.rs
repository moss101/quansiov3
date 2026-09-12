//! Rebuildable client read models derived from RuntimeEvents (CORE-009).
//!
//! A projection is a derived read model (DOSSIER.md §5 "Product state display: client
//! projections only"): it is never a source of truth, never a second event store and is
//! never consulted for recovery. It is a pure function of the event stream, so
//! rebuilding from zero yields exactly the state incremental application produced.
//!
//! Every projection row is written in the same transaction as the projection's
//! checkpoint (`event_cursors`, stream `projection:<name>`), so a crash can never leave
//! the checkpoint ahead of the rows. `last_sequence` guards each upsert, making
//! re-application idempotent: an already-applied event never double-counts or regresses
//! state.

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use quansio_core::Sequence;
use serde_json::{Map, Value};
use sqlx::{PgConnection, Row};

use crate::cursor::{advance_cursor_in_tx, clear_cursor_in_tx, PROJECTION_CONSUMER};
use crate::envelope::RuntimeEvent;
use crate::error::EventError;
use crate::event_type::EventFamily;
use crate::store::EventStore;

/// A derived read model that consumes the tenant RuntimeEvent stream.
///
/// Implementations must be deterministic: the same events in the same order must
/// produce the same rows. `apply` is idempotent per `sequence`.
#[async_trait]
pub trait Projection: Send + Sync + 'static {
    /// Stable projection name; also the checkpoint stream suffix.
    fn name(&self) -> &'static str;

    /// Apply one event inside the caller's transaction.
    ///
    /// Returns whether a projection row was written. An event the projection does not
    /// model is consumed without a write (`false`).
    ///
    /// # Errors
    /// Returns a database error.
    async fn apply(
        &self,
        conn: &mut PgConnection,
        event: &RuntimeEvent,
    ) -> Result<bool, EventError>;

    /// Remove every row of this projection for `tenant_id` (rebuild from zero).
    ///
    /// # Errors
    /// Returns a database error.
    async fn truncate(&self, conn: &mut PgConnection, tenant_id: &str) -> Result<(), EventError>;

    /// Canonical byte representation of the projection state, used to prove that
    /// incremental application and a rebuild are identical.
    ///
    /// # Errors
    /// Returns a database or JSON error.
    async fn snapshot(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
    ) -> Result<Vec<u8>, EventError>;
}

/// What applying one event did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The projection row was inserted or advanced.
    Applied,
    /// The event was consumed and checkpointed but is not modelled by the projection.
    Unchanged,
    /// The event was at or below the checkpoint and was skipped without a write.
    Duplicate,
}

/// Result of one catch-up pass over the tenant stream.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CatchUpReport {
    /// Sequences consumed and checkpointed, in order.
    pub consumed: Vec<Sequence>,
    /// Events that wrote a projection row.
    pub applied: u64,
    /// Events consumed without a row change.
    pub unchanged: u64,
    /// Events skipped because they were already applied.
    pub duplicates: u64,
}

impl CatchUpReport {
    fn merge(&mut self, other: Self) {
        self.consumed.extend(other.consumed);
        self.applied += other.applied;
        self.unchanged += other.unchanged;
        self.duplicates += other.duplicates;
    }
}

/// Drives one [`Projection`] over the tenant RuntimeEvent stream.
#[derive(Debug, Clone)]
pub struct ProjectionRunner<P: Projection> {
    store: EventStore,
    projection: P,
}

impl<P: Projection> ProjectionRunner<P> {
    /// Build a runner for one projection.
    #[must_use]
    pub fn new(store: EventStore, projection: P) -> Self {
        Self { store, projection }
    }

    /// The projection being driven.
    #[must_use]
    pub fn projection(&self) -> &P {
        &self.projection
    }

    /// The checkpoint stream identity (`projection:<name>`).
    #[must_use]
    pub fn stream_id(&self) -> String {
        format!("projection:{}", self.projection.name())
    }

    /// The projection's checkpoint: the last consumed tenant sequence.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn checkpoint(&self, tenant_id: &str) -> Result<Option<Sequence>, EventError> {
        Ok(self
            .store
            .load_cursor(tenant_id, &self.stream_id(), PROJECTION_CONSUMER)
            .await?
            .map(|cursor| cursor.sequence()))
    }

    /// Apply one event.
    ///
    /// The projection row and the checkpoint advance commit together. An event at or
    /// below the checkpoint is a duplicate and is skipped without touching the database.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn apply_event(
        &self,
        tenant_id: &str,
        event: &RuntimeEvent,
    ) -> Result<ApplyOutcome, EventError> {
        if let Some(position) = self.checkpoint(tenant_id).await? {
            if event.sequence <= position {
                return Ok(ApplyOutcome::Duplicate);
            }
        }
        let stream_id = self.stream_id();
        let mut tx = self.store.begin_tenant_transaction(tenant_id).await?;
        let changed = self.projection.apply(&mut tx, event).await?;
        advance_cursor_in_tx(
            &mut tx,
            tenant_id,
            &stream_id,
            PROJECTION_CONSUMER,
            event.sequence,
        )
        .await?;
        tx.commit().await?;
        Ok(if changed {
            ApplyOutcome::Applied
        } else {
            ApplyOutcome::Unchanged
        })
    }

    /// Consume at most `limit` events after the checkpoint, in sequence order.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn catch_up(&self, tenant_id: &str, limit: i64) -> Result<CatchUpReport, EventError> {
        let mut report = CatchUpReport::default();
        let position = self.checkpoint(tenant_id).await?;
        let events = self
            .store
            .read_events_after(tenant_id, position, limit)
            .await?;
        for event in events {
            match self.apply_event(tenant_id, &event).await? {
                ApplyOutcome::Applied => report.applied += 1,
                ApplyOutcome::Unchanged => report.unchanged += 1,
                ApplyOutcome::Duplicate => report.duplicates += 1,
            }
            report.consumed.push(event.sequence);
        }
        Ok(report)
    }

    /// Consume every event after the checkpoint in `batch`-sized passes.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn catch_up_all(
        &self,
        tenant_id: &str,
        batch: i64,
    ) -> Result<CatchUpReport, EventError> {
        let mut total = CatchUpReport::default();
        loop {
            let report = self.catch_up(tenant_id, batch).await?;
            if report.consumed.is_empty() {
                return Ok(total);
            }
            total.merge(report);
        }
    }

    /// Rebuild the projection from zero: truncate its rows, clear its checkpoint and
    /// replay the whole tenant stream.
    ///
    /// The truncate and checkpoint reset commit together; replay is resumable, so a
    /// crash mid-rebuild leaves a valid checkpoint rather than a half-state that looks
    /// complete.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn rebuild(&self, tenant_id: &str, batch: i64) -> Result<CatchUpReport, EventError> {
        let stream_id = self.stream_id();
        let mut tx = self.store.begin_tenant_transaction(tenant_id).await?;
        self.projection.truncate(&mut tx, tenant_id).await?;
        clear_cursor_in_tx(&mut tx, tenant_id, &stream_id, PROJECTION_CONSUMER).await?;
        tx.commit().await?;
        self.catch_up_all(tenant_id, batch).await
    }

    /// Canonical bytes of the projection state for `tenant_id`.
    ///
    /// # Errors
    /// Returns a database or JSON error.
    pub async fn snapshot(&self, tenant_id: &str) -> Result<Vec<u8>, EventError> {
        let mut tx = self.store.begin_tenant_transaction(tenant_id).await?;
        let bytes = self.projection.snapshot(&mut tx, tenant_id).await?;
        tx.commit().await?;
        Ok(bytes)
    }
}

/// Canonical run lifecycle states (DOMAIN.md §5.2).
const RUN_STATUSES: &[&str] = &[
    "CREATED",
    "QUEUED",
    "RUNNING",
    "WAITING",
    "WAITING_APPROVAL",
    "WAITING_QUESTION",
    "WAITING_EVENT",
    "WAITING_TIMER",
    "WAITING_CHILD",
    "WAITING_TAKEOVER",
    "VERIFYING",
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
    "BLOCKED_UNRECOVERABLE",
    "SUSPENDED",
];

/// Terminal run states (DOMAIN.md §5.2).
const TERMINAL_RUN_STATUSES: &[&str] =
    &["SUCCEEDED", "FAILED", "CANCELLED", "BLOCKED_UNRECOVERABLE"];

/// Canonical WorkNode states (DOMAIN.md §4.1).
const WORK_NODE_STATUSES: &[&str] = &[
    "draft",
    "ready",
    "blocked",
    "in_progress",
    "waiting",
    "verifying",
    "done",
    "failed",
    "cancelled",
];

/// One row per Run: lifecycle status derived from the `run.*` event family.
///
/// The status is derived from the event type (DOMAIN.md §9.2, §5.2); an exact
/// `WAITING_*` state may be carried in the event payload's canonical `status` field.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunStatusProjection;

impl RunStatusProjection {
    /// Build the projection.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Projection for RunStatusProjection {
    fn name(&self) -> &'static str {
        "run_status"
    }

    async fn apply(
        &self,
        conn: &mut PgConnection,
        event: &RuntimeEvent,
    ) -> Result<bool, EventError> {
        if event.aggregate_type != "run" {
            return Ok(false);
        }
        let Some(status) = run_status(event) else {
            return Ok(false);
        };
        let occurred_at = event.occurred_at;
        let started_at =
            matches!(event.event_type.name(), "started" | "resumed").then_some(occurred_at);
        let ended_at = TERMINAL_RUN_STATUSES
            .contains(&status.as_str())
            .then_some(occurred_at);
        let current_turn_id = payload_str(event, "current_turn_id");
        let terminal_reason = payload_str(event, "terminal_reason");
        let result = sqlx::query(
            "INSERT INTO run_status_projection (tenant_id, run_id, workspace_id, work_node_id, \
             agent_thread_id, status, last_event_type, current_turn_id, terminal_reason, \
             started_at, ended_at, first_occurred_at, last_occurred_at, event_count, last_sequence) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12, 1, $13) \
             ON CONFLICT (tenant_id, run_id) DO UPDATE SET \
             workspace_id = COALESCE(EXCLUDED.workspace_id, run_status_projection.workspace_id), \
             work_node_id = COALESCE(EXCLUDED.work_node_id, run_status_projection.work_node_id), \
             agent_thread_id = COALESCE(EXCLUDED.agent_thread_id, run_status_projection.agent_thread_id), \
             status = EXCLUDED.status, \
             last_event_type = EXCLUDED.last_event_type, \
             current_turn_id = COALESCE(EXCLUDED.current_turn_id, run_status_projection.current_turn_id), \
             terminal_reason = COALESCE(EXCLUDED.terminal_reason, run_status_projection.terminal_reason), \
             started_at = COALESCE(run_status_projection.started_at, EXCLUDED.started_at), \
             ended_at = COALESCE(run_status_projection.ended_at, EXCLUDED.ended_at), \
             last_occurred_at = EXCLUDED.last_occurred_at, \
             event_count = run_status_projection.event_count + 1, \
             last_sequence = EXCLUDED.last_sequence \
             WHERE run_status_projection.last_sequence < EXCLUDED.last_sequence",
        )
        .bind(&event.tenant_id)
        .bind(&event.aggregate_id)
        .bind(&event.workspace_id)
        .bind(payload_str(event, "work_node_id"))
        .bind(payload_str(event, "agent_thread_id"))
        .bind(&status)
        .bind(event.event_type.to_string())
        .bind(current_turn_id)
        .bind(terminal_reason)
        .bind(started_at)
        .bind(ended_at)
        .bind(occurred_at)
        .bind(event.sequence.get())
        .execute(conn)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn truncate(&self, conn: &mut PgConnection, tenant_id: &str) -> Result<(), EventError> {
        sqlx::query("DELETE FROM run_status_projection WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(conn)
            .await?;
        Ok(())
    }

    async fn snapshot(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
    ) -> Result<Vec<u8>, EventError> {
        let rows = sqlx::query(
            "SELECT run_id, workspace_id, work_node_id, agent_thread_id, status, last_event_type, \
             current_turn_id, terminal_reason, started_at, ended_at, first_occurred_at, \
             last_occurred_at, event_count, last_sequence \
             FROM run_status_projection WHERE tenant_id = $1 ORDER BY run_id",
        )
        .bind(tenant_id)
        .fetch_all(conn)
        .await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut record = Map::new();
            record.insert("run_id".into(), Value::String(row.try_get("run_id")?));
            record.insert(
                "workspace_id".into(),
                optional_str(row.try_get("workspace_id")?),
            );
            record.insert(
                "work_node_id".into(),
                optional_str(row.try_get("work_node_id")?),
            );
            record.insert(
                "agent_thread_id".into(),
                optional_str(row.try_get("agent_thread_id")?),
            );
            record.insert("status".into(), Value::String(row.try_get("status")?));
            record.insert(
                "last_event_type".into(),
                Value::String(row.try_get("last_event_type")?),
            );
            record.insert(
                "current_turn_id".into(),
                optional_str(row.try_get("current_turn_id")?),
            );
            record.insert(
                "terminal_reason".into(),
                optional_str(row.try_get("terminal_reason")?),
            );
            record.insert(
                "started_at".into(),
                optional_timestamp(row.try_get("started_at")?),
            );
            record.insert(
                "ended_at".into(),
                optional_timestamp(row.try_get("ended_at")?),
            );
            record.insert(
                "first_occurred_at".into(),
                timestamp(row.try_get("first_occurred_at")?),
            );
            record.insert(
                "last_occurred_at".into(),
                timestamp(row.try_get("last_occurred_at")?),
            );
            record.insert(
                "event_count".into(),
                Value::Number(row.try_get::<i64, _>("event_count")?.into()),
            );
            record.insert(
                "last_sequence".into(),
                Value::Number(row.try_get::<i64, _>("last_sequence")?.into()),
            );
            records.push(Value::Object(record));
        }
        Ok(serde_json::to_vec(&Value::Array(records))?)
    }
}

/// One row per WorkNode: status derived from the `work.*` node events.
///
/// Modelled events are `work.node_created`, `work.node_status_changed` and a
/// `work.node_updated` that carries an explicit canonical status; other `work.*` events
/// are consumed without a row change.
#[derive(Debug, Default, Clone, Copy)]
pub struct WorkNodeStatusProjection;

impl WorkNodeStatusProjection {
    /// Build the projection.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Projection for WorkNodeStatusProjection {
    fn name(&self) -> &'static str {
        "work_node_status"
    }

    async fn apply(
        &self,
        conn: &mut PgConnection,
        event: &RuntimeEvent,
    ) -> Result<bool, EventError> {
        if event.aggregate_type != "work" {
            return Ok(false);
        }
        let Some(status) = work_node_status(event) else {
            return Ok(false);
        };
        let result = sqlx::query(
            "INSERT INTO work_node_status_projection (tenant_id, work_node_id, workspace_id, kind, \
             title, status, owner_agent_thread_id, revision, last_event_type, first_occurred_at, \
             last_occurred_at, event_count, last_sequence) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, 1, $11) \
             ON CONFLICT (tenant_id, work_node_id) DO UPDATE SET \
             workspace_id = COALESCE(EXCLUDED.workspace_id, work_node_status_projection.workspace_id), \
             kind = COALESCE(EXCLUDED.kind, work_node_status_projection.kind), \
             title = COALESCE(EXCLUDED.title, work_node_status_projection.title), \
             status = EXCLUDED.status, \
             owner_agent_thread_id = COALESCE(EXCLUDED.owner_agent_thread_id, work_node_status_projection.owner_agent_thread_id), \
             revision = COALESCE(EXCLUDED.revision, work_node_status_projection.revision), \
             last_event_type = EXCLUDED.last_event_type, \
             last_occurred_at = EXCLUDED.last_occurred_at, \
             event_count = work_node_status_projection.event_count + 1, \
             last_sequence = EXCLUDED.last_sequence \
             WHERE work_node_status_projection.last_sequence < EXCLUDED.last_sequence",
        )
        .bind(&event.tenant_id)
        .bind(&event.aggregate_id)
        .bind(&event.workspace_id)
        .bind(payload_str(event, "kind"))
        .bind(payload_str(event, "title"))
        .bind(&status)
        .bind(payload_str(event, "owner_agent_thread_id"))
        .bind(payload_i64(event, "revision"))
        .bind(event.event_type.to_string())
        .bind(event.occurred_at)
        .bind(event.sequence.get())
        .execute(conn)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn truncate(&self, conn: &mut PgConnection, tenant_id: &str) -> Result<(), EventError> {
        sqlx::query("DELETE FROM work_node_status_projection WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(conn)
            .await?;
        Ok(())
    }

    async fn snapshot(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
    ) -> Result<Vec<u8>, EventError> {
        let rows = sqlx::query(
            "SELECT work_node_id, workspace_id, kind, title, status, owner_agent_thread_id, \
             revision, last_event_type, first_occurred_at, last_occurred_at, event_count, \
             last_sequence \
             FROM work_node_status_projection WHERE tenant_id = $1 ORDER BY work_node_id",
        )
        .bind(tenant_id)
        .fetch_all(conn)
        .await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut record = Map::new();
            record.insert(
                "work_node_id".into(),
                Value::String(row.try_get("work_node_id")?),
            );
            record.insert(
                "workspace_id".into(),
                optional_str(row.try_get("workspace_id")?),
            );
            record.insert("kind".into(), optional_str(row.try_get("kind")?));
            record.insert("title".into(), optional_str(row.try_get("title")?));
            record.insert("status".into(), Value::String(row.try_get("status")?));
            record.insert(
                "owner_agent_thread_id".into(),
                optional_str(row.try_get("owner_agent_thread_id")?),
            );
            record.insert("revision".into(), optional_i64(row.try_get("revision")?));
            record.insert(
                "last_event_type".into(),
                Value::String(row.try_get("last_event_type")?),
            );
            record.insert(
                "first_occurred_at".into(),
                timestamp(row.try_get("first_occurred_at")?),
            );
            record.insert(
                "last_occurred_at".into(),
                timestamp(row.try_get("last_occurred_at")?),
            );
            record.insert(
                "event_count".into(),
                Value::Number(row.try_get::<i64, _>("event_count")?.into()),
            );
            record.insert(
                "last_sequence".into(),
                Value::Number(row.try_get::<i64, _>("last_sequence")?.into()),
            );
            records.push(Value::Object(record));
        }
        Ok(serde_json::to_vec(&Value::Array(records))?)
    }
}

/// Run status derived from the event type, honouring an explicit canonical
/// `payload.status` for the generic waiting event.
fn run_status(event: &RuntimeEvent) -> Option<String> {
    if event.event_type.family() != EventFamily::Run {
        return None;
    }
    let derived = match event.event_type.name() {
        "created" => "CREATED",
        "queued" => "QUEUED",
        "started" | "resumed" => "RUNNING",
        "waiting" => "WAITING",
        "verifying" | "verification_passed" => "VERIFYING",
        "verification_failed" => "RUNNING",
        "succeeded" => "SUCCEEDED",
        "failed" => "FAILED",
        "cancelled" => "CANCELLED",
        "blocked" => "BLOCKED_UNRECOVERABLE",
        "suspended" => "SUSPENDED",
        _ => return None,
    };
    let status = payload_str(event, "status")
        .filter(|value| RUN_STATUSES.contains(value))
        .unwrap_or(derived);
    Some(status.to_string())
}

/// WorkNode status derived from the node event.
fn work_node_status(event: &RuntimeEvent) -> Option<String> {
    if event.event_type.family() != EventFamily::Work {
        return None;
    }
    let name = event.event_type.name();
    if !matches!(
        name,
        "node_created" | "node_updated" | "node_status_changed"
    ) {
        return None;
    }
    let explicit = payload_str(event, "status").filter(|value| WORK_NODE_STATUSES.contains(value));
    match (name, explicit) {
        (_, Some(status)) => Some(status.to_string()),
        ("node_created", None) => Some("draft".to_string()),
        _ => None,
    }
}

fn payload_str<'a>(event: &'a RuntimeEvent, key: &str) -> Option<&'a str> {
    event.payload.get(key).and_then(Value::as_str)
}

fn payload_i64(event: &RuntimeEvent, key: &str) -> Option<i64> {
    event.payload.get(key).and_then(Value::as_i64)
}

fn optional_str(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::String)
}

fn optional_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, |number| Value::Number(number.into()))
}

fn timestamp(value: DateTime<Utc>) -> Value {
    Value::String(value.to_rfc3339_opts(SecondsFormat::Micros, true))
}

fn optional_timestamp(value: Option<DateTime<Utc>>) -> Value {
    value.map_or(Value::Null, timestamp)
}
