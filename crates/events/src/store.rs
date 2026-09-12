//! Transactional RuntimeEvent store (DOMAIN.md §9.1).
//!
//! State mutation, RuntimeEvent and outbox row are written in one PostgreSQL
//! transaction. The caller's mutation runs inside that transaction and stages its
//! events on an [`EventBatch`]; the store refuses to commit a transaction that staged
//! no event, so there is no API path that commits state without its event.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use quansio_core::{
    CanonicalId, CausationId, CommandId, CorrelationId, EventId, Generation, Prefix, Sequence,
    TypedId,
};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};

use crate::envelope::{Actor, EventDraft, RuntimeEvent};
use crate::error::EventError;
use crate::event_type::EventType;

/// Boxed future returned by a mutation closure.
///
/// A boxed future is what lets the closure borrow the transaction and the batch for the
/// duration of the call without naming a lifetime at the call site.
pub type BoxEventFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, EventError>> + Send + 'a>>;

/// Events staged by a mutation, committed with the state it produced.
#[derive(Debug, Default)]
pub struct EventBatch {
    staged: Vec<EventDraft>,
}

impl EventBatch {
    /// Stage an event to be written when the transaction commits.
    pub fn emit(&mut self, draft: EventDraft) {
        self.staged.push(draft);
    }

    /// Number of staged events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.staged.len()
    }

    /// Whether no event has been staged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty()
    }
}

/// The canonical RuntimeEvent store.
#[derive(Debug, Clone)]
pub struct EventStore {
    pool: PgPool,
}

impl EventStore {
    /// Build a store over a PostgreSQL pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The underlying pool (used by the outbox publisher and tests).
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Begin a transaction with the tenant context set, so row-level security applies
    /// (DOSSIER.md §16).
    pub async fn begin_tenant_transaction(
        &self,
        tenant_id: &str,
    ) -> Result<Transaction<'_, Postgres>, EventError> {
        validate_tenant_id(tenant_id)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
            .bind(tenant_id)
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    /// Commit one state mutation together with its RuntimeEvent and outbox row.
    ///
    /// The mutation closure receives the open connection and an [`EventBatch`]. When it
    /// returns `Ok`, the store assigns the tenant-monotonic `sequence` to every staged
    /// event, writes `runtime_events` and `event_outbox`, and commits. When it returns
    /// `Err` — or stages no event — the transaction is rolled back and neither the state
    /// nor the event exists.
    ///
    /// # Errors
    /// Returns [`EventError::NoEventStaged`] if the mutation committed no event, and
    /// propagates database or envelope errors.
    pub async fn commit_mutation<T, F>(&self, tenant_id: &str, mutation: F) -> Result<T, EventError>
    where
        T: Send,
        F: for<'a> FnOnce(&'a mut PgConnection, &'a mut EventBatch) -> BoxEventFuture<'a, T>,
    {
        let mut tx = self.begin_tenant_transaction(tenant_id).await?;
        let mut batch = EventBatch::default();
        let output = mutation(&mut tx, &mut batch).await?;
        if batch.is_empty() {
            return Err(EventError::NoEventStaged);
        }
        for draft in batch.staged {
            let sequence = self.next_sequence(&mut tx, tenant_id).await?;
            let event = RuntimeEvent::from_draft(draft, tenant_id, sequence);
            insert_event(&mut tx, &event).await?;
        }
        tx.commit().await?;
        Ok(output)
    }

    /// Assign the next tenant sequence inside the caller's transaction.
    ///
    /// `tenant_event_sequences` holds one counter row per tenant. The upsert takes an
    /// exclusive row lock held until commit, so concurrent writers serialize and each
    /// sees the previous committed value; a rollback rolls the increment back, keeping
    /// the stream gap-free.
    async fn next_sequence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: &str,
    ) -> Result<Sequence, EventError> {
        let value: i64 = sqlx::query_scalar(
            "INSERT INTO tenant_event_sequences (tenant_id, next_sequence) VALUES ($1, 1) \
             ON CONFLICT (tenant_id) DO UPDATE \
             SET next_sequence = tenant_event_sequences.next_sequence + 1, updated_at = now() \
             RETURNING next_sequence",
        )
        .bind(tenant_id)
        .fetch_one(&mut **tx)
        .await?;
        Sequence::new(value).map_err(|error| EventError::MalformedEventRow(error.to_string()))
    }

    /// Read events for a tenant with `sequence` strictly after `after`, in order.
    ///
    /// `after = None` starts at the beginning of the tenant stream.
    ///
    /// # Errors
    /// Returns a database error or an envelope error for a row that does not satisfy
    /// the RuntimeEvent contract.
    pub async fn read_events_after(
        &self,
        tenant_id: &str,
        after: Option<Sequence>,
        limit: i64,
    ) -> Result<Vec<RuntimeEvent>, EventError> {
        let mut tx = self.begin_tenant_transaction(tenant_id).await?;
        let after_value = after.map_or(0, Sequence::get);
        let rows = sqlx::query(
            "SELECT id, tenant_id, workspace_id, sequence, aggregate_type, aggregate_id, \
             aggregate_version, type, schema_version, occurred_at, command_id, correlation_id, \
             causation_id, actor, generation, payload \
             FROM runtime_events WHERE tenant_id = $1 AND sequence > $2 \
             ORDER BY sequence ASC LIMIT $3",
        )
        .bind(tenant_id)
        .bind(after_value)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let events = rows.iter().map(row_to_event).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(events)
    }

    /// Read the event history of one aggregate, in tenant sequence order.
    ///
    /// # Errors
    /// Returns a database error or an envelope error for an invalid row.
    pub async fn events_for_aggregate(
        &self,
        tenant_id: &str,
        aggregate_type: &str,
        aggregate_id: &str,
    ) -> Result<Vec<RuntimeEvent>, EventError> {
        let mut tx = self.begin_tenant_transaction(tenant_id).await?;
        let rows = sqlx::query(
            "SELECT id, tenant_id, workspace_id, sequence, aggregate_type, aggregate_id, \
             aggregate_version, type, schema_version, occurred_at, command_id, correlation_id, \
             causation_id, actor, generation, payload \
             FROM runtime_events WHERE tenant_id = $1 AND aggregate_type = $2 AND aggregate_id = $3 \
             ORDER BY sequence ASC",
        )
        .bind(tenant_id)
        .bind(aggregate_type)
        .bind(aggregate_id)
        .fetch_all(&mut *tx)
        .await?;
        let events = rows.iter().map(row_to_event).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(events)
    }
}

/// Validate a canonical `tn_<ULID>` tenant id before it is used as a scope.
pub(crate) fn validate_tenant_id(tenant_id: &str) -> Result<(), EventError> {
    CanonicalId::parse_typed(tenant_id, Prefix::Tenant)
        .map(|_| ())
        .map_err(|_| EventError::InvalidTenantId(tenant_id.to_string()))
}

/// Insert one RuntimeEvent plus its outbox row inside an open transaction.
pub(crate) async fn insert_event(
    tx: &mut Transaction<'_, Postgres>,
    event: &RuntimeEvent,
) -> Result<(), EventError> {
    let actor = serde_json::to_value(&event.actor)?;
    sqlx::query(
        "INSERT INTO runtime_events (id, tenant_id, workspace_id, sequence, aggregate_type, \
         aggregate_id, aggregate_version, type, schema_version, occurred_at, command_id, \
         correlation_id, causation_id, actor, generation, payload) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(event.event_id.to_string())
    .bind(&event.tenant_id)
    .bind(&event.workspace_id)
    .bind(event.sequence.get())
    .bind(&event.aggregate_type)
    .bind(&event.aggregate_id)
    .bind(event.aggregate_version as i64)
    .bind(event.event_type.to_string())
    .bind(&event.schema_version)
    .bind(event.occurred_at)
    .bind(event.command_id.as_ref().map(ToString::to_string))
    .bind(event.correlation_id.to_string())
    .bind(event.causation_id.as_ref().map(ToString::to_string))
    .bind(actor)
    .bind(event.generation.map(|generation| generation.get() as i64))
    .bind(&event.payload)
    .execute(&mut **tx)
    .await?;

    let subject = event.subject();
    let envelope = event.to_json_value();
    sqlx::query(
        "INSERT INTO event_outbox (event_id, tenant_id, subject, payload) VALUES ($1, $2, $3, $4)",
    )
    .bind(event.event_id.to_string())
    .bind(&event.tenant_id)
    .bind(subject)
    .bind(envelope)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Rebuild a RuntimeEvent from a `runtime_events` row.
pub(crate) fn row_to_event(row: &PgRow) -> Result<RuntimeEvent, EventError> {
    let malformed = |reason: &str| EventError::MalformedEventRow(reason.to_string());
    let event_id = EventId::parse(&row.try_get::<String, _>("id")?)
        .map_err(|error| malformed(&error.to_string()))?;
    let sequence = Sequence::new(row.try_get::<i64, _>("sequence")?)
        .map_err(|error| malformed(&error.to_string()))?;
    let event_type = EventType::parse(&row.try_get::<String, _>("type")?)?;
    let aggregate_version = row.try_get::<i64, _>("aggregate_version")?;
    if aggregate_version < 1 {
        return Err(malformed("aggregate_version must be positive"));
    }
    let command_id = row
        .try_get::<Option<String>, _>("command_id")?
        .map(|value| CommandId::parse(&value))
        .transpose()
        .map_err(|error| malformed(&error.to_string()))?;
    let correlation_id = row.try_get::<Option<String>, _>("correlation_id")?;
    let correlation_id = correlation_id.ok_or_else(|| malformed("correlation_id is null"))?;
    let correlation_id = correlation_id
        .parse::<CorrelationId>()
        .map_err(|error| malformed(&error.to_string()))?;
    let causation_id = row
        .try_get::<Option<String>, _>("causation_id")?
        .map(|value| value.parse::<CausationId>())
        .transpose()
        .map_err(|error| malformed(&error.to_string()))?;
    let actor: Actor = serde_json::from_value(row.try_get::<Value, _>("actor")?)
        .map_err(|error| malformed(&error.to_string()))?;
    let generation = row
        .try_get::<Option<i64>, _>("generation")?
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| malformed("generation must not be negative"))
                .and_then(|value| {
                    Generation::new(value).map_err(|error| malformed(&error.to_string()))
                })
        })
        .transpose()?;
    Ok(RuntimeEvent {
        event_id,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        sequence,
        aggregate_type: row.try_get("aggregate_type")?,
        aggregate_id: row.try_get("aggregate_id")?,
        aggregate_version: aggregate_version.unsigned_abs(),
        event_type,
        schema_version: row.try_get("schema_version")?,
        occurred_at: row.try_get::<DateTime<Utc>, _>("occurred_at")?,
        command_id,
        correlation_id,
        causation_id,
        actor,
        generation,
        payload: row.try_get("payload")?,
    })
}
