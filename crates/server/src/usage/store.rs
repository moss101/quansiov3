//! The persisted usage projection (DOMAIN.md §13.4): `usage_records`, materialized from the
//! tenant's RuntimeEvents by the background loop, and read together with the events it has not
//! reached yet, so a read is complete and costs the same however long the tenant's history is.
//!
//! Materializing is idempotent — a row's identity is a function of (tenant, source event, meter) —
//! and moves the projection's cursor in the same transaction, so a crash between batches repeats
//! nothing and loses nothing. The rows remain a projection: dropping them and the cursor rebuilds
//! them from the events.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use quansio_core::Sequence;
use quansio_events::{advance_cursor_in_tx, EventError, EventStore};
use serde_json::Value;
use sqlx::{PgPool, Row};

use super::meters::UsageMeter;
use super::project::{project_event, UsageRecord};

/// The event stream the projection consumes.
pub const USAGE_STREAM: &str = "usage:events";
/// The projection's consumer name on that stream.
pub const USAGE_CONSUMER: &str = "usage-projection";
/// Events one materialization batch reads.
pub const MATERIALIZE_BATCH: i64 = 1024;
/// Events a read projects beyond the materialized rows; the background loop keeps the backlog far
/// below this, and a read past it says it is incomplete rather than guessing.
pub const MAX_TAIL_EVENTS: i64 = 20_000;

/// One meter's total.
#[derive(Debug, Clone, PartialEq)]
pub struct MeterTotal {
    /// Meter.
    pub meter: UsageMeter,
    /// Sum of every record's quantity.
    pub total: f64,
    /// Unit, when any record exists.
    pub unit: Option<String>,
}

/// A tenant's (or one workspace's) usage as of one read.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSummary {
    /// Every meter, in [`UsageMeter::ALL`] order.
    pub meters: Vec<MeterTotal>,
    /// The newest records, newest first, at most the requested number.
    pub recent: Vec<UsageRecord>,
    /// How many records there are in all.
    pub record_count: u64,
    /// The last event sequence the summary reflects.
    pub through_sequence: i64,
    /// `false` when more events were waiting than a read projects ([`MAX_TAIL_EVENTS`]).
    pub complete: bool,
}

/// The usage projection of one tenant.
#[derive(Debug, Clone)]
pub struct UsageStore {
    events: EventStore,
    tenant_id: String,
}

impl UsageStore {
    /// The projection of `tenant_id`.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// Materialize up to `limit` events after the cursor. Returns how many events were consumed.
    ///
    /// # Errors
    /// Returns a database or event error; nothing is written and the cursor does not move.
    pub async fn materialize(&self, limit: i64) -> Result<usize, EventError> {
        let position = self
            .events
            .load_cursor(&self.tenant_id, USAGE_STREAM, USAGE_CONSUMER)
            .await?
            .map(|cursor| cursor.sequence());
        let batch = self
            .events
            .read_events_after(&self.tenant_id, position, limit)
            .await?;
        let Some(last) = batch.last().map(|event| event.sequence) else {
            return Ok(0);
        };
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        for record in batch.iter().flat_map(project_event) {
            sqlx::query(
                "INSERT INTO usage_records (id, tenant_id, workspace_id, scope_refs, meter, quantity, \
                                            unit, cost_minor_units, source_event_id, occurred_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (tenant_id, source_event_id, meter) DO NOTHING",
            )
            .bind(&record.id)
            .bind(&record.tenant_id)
            .bind(&record.workspace_id)
            .bind(&record.scope_refs)
            .bind(record.meter.as_str())
            .bind(record.quantity)
            .bind(record.unit)
            .bind(record.cost_minor_units)
            .bind(&record.source_event_id)
            .bind(record.occurred_at)
            .execute(&mut *tx)
            .await?;
        }
        advance_cursor_in_tx(&mut tx, &self.tenant_id, USAGE_STREAM, USAGE_CONSUMER, last).await?;
        tx.commit().await?;
        Ok(batch.len())
    }

    /// Materialize until the cursor is caught up or `max_batches` batches ran. Returns the events
    /// consumed.
    ///
    /// # Errors
    /// As [`Self::materialize`].
    pub async fn catch_up(&self, max_batches: usize) -> Result<usize, EventError> {
        let mut consumed = 0;
        for _ in 0..max_batches {
            let batch = self.materialize(MATERIALIZE_BATCH).await?;
            consumed += batch;
            if batch < usize::try_from(MATERIALIZE_BATCH).unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(consumed)
    }

    /// Everything metered for the tenant, or for one workspace: the materialized rows and the
    /// events after the projection's cursor, projected now; `recent` bounds the records returned.
    ///
    /// # Errors
    /// Returns a database or event error.
    pub async fn summary(
        &self,
        workspace_id: Option<&str>,
        recent: i64,
    ) -> Result<UsageSummary, EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        // The cursor and the totals come from one statement, so from one snapshot: the rows it
        // sums are exactly those of the events up to the cursor it returns.
        let rows = sqlx::query(
            "WITH position AS ( \
                 SELECT sequence FROM event_cursors \
                 WHERE tenant_id = $1 AND stream_id = $3 AND consumer = $4) \
             SELECT p.sequence AS position, u.meter, SUM(u.quantity)::float8 AS total, \
                    MAX(u.unit) AS unit, count(*)::bigint AS records \
             FROM usage_records u CROSS JOIN position p \
             WHERE u.tenant_id = $1 AND ($2::text IS NULL OR u.workspace_id = $2) \
             GROUP BY p.sequence, u.meter \
             UNION ALL \
             SELECT (SELECT sequence FROM position), NULL, NULL, NULL, 0::bigint",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .bind(USAGE_STREAM)
        .bind(USAGE_CONSUMER)
        .fetch_all(&mut *tx)
        .await?;
        let position: Option<i64> = rows.first().and_then(|row| row.get("position"));
        let mut totals: BTreeMap<&'static str, (f64, Option<String>)> = BTreeMap::new();
        let mut record_count = 0u64;
        for row in &rows {
            let Some(meter) = row.get::<Option<String>, _>("meter") else {
                continue;
            };
            let Ok(meter) = UsageMeter::parse(&meter) else {
                continue;
            };
            let total: f64 = row.get::<Option<f64>, _>("total").unwrap_or_default();
            totals.insert(meter.as_str(), (total, row.get("unit")));
            record_count += u64::try_from(row.get::<i64, _>("records")).unwrap_or_default();
        }
        let persisted = if position.is_some() {
            sqlx::query(
                "SELECT id, workspace_id, scope_refs, meter, quantity, unit, cost_minor_units, \
                        source_event_id, occurred_at \
                 FROM usage_records \
                 WHERE tenant_id = $1 AND ($2::text IS NULL OR workspace_id = $2) \
                 ORDER BY occurred_at DESC, id DESC LIMIT $3",
            )
            .bind(&self.tenant_id)
            .bind(workspace_id)
            .bind(recent)
            .fetch_all(&mut *tx)
            .await?
        } else {
            Vec::new()
        };
        tx.commit().await?;

        let after = position.map(Sequence::new).transpose()?;
        let tail = self
            .events
            .read_events_after(&self.tenant_id, after, MAX_TAIL_EVENTS + 1)
            .await?;
        let complete = i64::try_from(tail.len()).unwrap_or(i64::MAX) <= MAX_TAIL_EVENTS;
        let tail = &tail[..tail
            .len()
            .min(usize::try_from(MAX_TAIL_EVENTS).unwrap_or(usize::MAX))];
        let through_sequence = tail
            .last()
            .map(|event| event.sequence.get())
            .or(position)
            .unwrap_or(0);
        let mut fresh: Vec<UsageRecord> = tail
            .iter()
            .flat_map(project_event)
            .filter(|record| {
                workspace_id.is_none_or(|ws| record.workspace_id.as_deref() == Some(ws))
            })
            .collect();
        for record in &fresh {
            let entry = totals.entry(record.meter.as_str()).or_insert((0.0, None));
            entry.0 += record.quantity;
            entry.1.get_or_insert_with(|| record.unit.to_string());
        }
        record_count += u64::try_from(fresh.len()).unwrap_or_default();

        fresh.sort_by_key(|record| std::cmp::Reverse(record.occurred_at));
        let mut seen: HashSet<String> = HashSet::new();
        let mut recent_records: Vec<UsageRecord> = Vec::new();
        for record in fresh {
            if seen.insert(record.id.clone()) {
                recent_records.push(record);
            }
        }
        for row in &persisted {
            let id: String = row.get("id");
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(meter) = UsageMeter::parse(&row.get::<String, _>("meter")) else {
                continue;
            };
            recent_records.push(UsageRecord {
                id,
                tenant_id: self.tenant_id.clone(),
                workspace_id: row.get("workspace_id"),
                scope_refs: row.get::<Value, _>("scope_refs"),
                meter,
                quantity: row.get("quantity"),
                unit: meter.unit(),
                cost_minor_units: row.get("cost_minor_units"),
                source_event_id: row.get("source_event_id"),
                occurred_at: row.get::<DateTime<Utc>, _>("occurred_at"),
            });
        }
        recent_records.truncate(usize::try_from(recent).unwrap_or(0));

        let meters = UsageMeter::ALL
            .iter()
            .map(|meter| {
                let (total, unit) = totals.get(meter.as_str()).cloned().unwrap_or((0.0, None));
                MeterTotal {
                    meter: *meter,
                    total,
                    unit,
                }
            })
            .collect();
        Ok(UsageSummary {
            meters,
            recent: recent_records,
            record_count,
            through_sequence,
            complete,
        })
    }
}
