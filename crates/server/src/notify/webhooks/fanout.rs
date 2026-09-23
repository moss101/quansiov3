//! The webhook fan-out: RuntimeEvents to deliveries (DOMAIN.md §13.5).
//!
//! The consumer reads the tenant's events after its `event_cursors` position and enqueues one
//! `webhook_deliveries` row per (subscription, event) the subscription selects — in the
//! transaction that advances the cursor, with the ledger's unique key underneath, so a
//! replayed batch, a restart or two instances scanning at once enqueue an event once.
//!
//! Scope is structural: the tenant's subscriptions are the only ones the transaction can see
//! (row-level security), a workspace subscription only matches its workspace's events, and a
//! subscription only matches events committed after it (`start_sequence`). Events of the
//! `webhook` family and events a protected thread withholds from non-participants are never
//! enqueued. A paused subscription still enqueues — its deliveries are held, not lost.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_events::{advance_cursor_in_tx, EventStore, RuntimeEvent};
use serde_json::Value;
use sqlx::Row;

use super::{deliverable, event_selected, WebhookError};
use crate::control::collaboration::ThreadGate;

/// The cursor stream the consumer advances.
pub const FANOUT_STREAM: &str = "webhooks:events";
/// The consumer name.
pub const FANOUT_CONSUMER: &str = "webhook-fanout";

struct Target {
    id: String,
    workspace_id: Option<String>,
    event_types: Vec<String>,
    start_sequence: i64,
}

/// Whether a subscription selects an event: same workspace when it is scoped to one, after
/// its start, through its filter.
fn selects(target: &Target, event: &RuntimeEvent) -> bool {
    let in_scope = match &target.workspace_id {
        Some(workspace) => event.workspace_id.as_deref() == Some(workspace.as_str()),
        None => true,
    };
    in_scope
        && event.sequence.get() > target.start_sequence
        && event_selected(&target.event_types, &event.event_type.to_string())
}

/// The fan-out for one tenant.
#[derive(Debug, Clone)]
pub struct WebhookFanout {
    events: EventStore,
    tenant_id: String,
}

impl WebhookFanout {
    /// A consumer over the pool for one tenant.
    #[must_use]
    pub fn new(pool: sqlx::PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// Consume up to `limit` events. Returns how many deliveries were enqueued.
    ///
    /// # Errors
    /// A database or event error; nothing is enqueued and the cursor does not move.
    pub async fn scan(&self, limit: i64) -> Result<usize, WebhookError> {
        let position = self
            .events
            .load_cursor(&self.tenant_id, FANOUT_STREAM, FANOUT_CONSUMER)
            .await?
            .map(|cursor| cursor.sequence());
        let batch = self
            .events
            .read_events_after(&self.tenant_id, position, limit)
            .await?;
        let Some(last) = batch.last().map(|event| event.sequence) else {
            return Ok(0);
        };
        let targets = self.targets().await?;
        // A gate with no user admits what any non-participant may see.
        let mut gate = ThreadGate::new(self.events.pool().clone(), &self.tenant_id, None);
        let mut rows: Vec<(String, &RuntimeEvent)> = Vec::new();
        for event in &batch {
            if !deliverable(event) {
                continue;
            }
            let selected: Vec<&Target> = targets.iter().filter(|t| selects(t, event)).collect();
            if selected.is_empty() || !gate.admits_event(event).await {
                continue;
            }
            for target in selected {
                rows.push((target.id.clone(), event));
            }
        }
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let mut enqueued = 0usize;
        for (subscription_id, event) in rows {
            let id = CanonicalId::generate(Prefix::WebhookDelivery, &mut UlidGenerator::new())
                .to_string();
            enqueued += usize::try_from(
                sqlx::query(
                    "INSERT INTO webhook_deliveries (id, tenant_id, subscription_id, workspace_id, \
                     event_id, event_type, event_sequence) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (subscription_id, event_id) DO NOTHING",
                )
                .bind(&id)
                .bind(&self.tenant_id)
                .bind(&subscription_id)
                .bind(event.workspace_id.as_deref())
                .bind(event.event_id.to_string())
                .bind(event.event_type.to_string())
                .bind(event.sequence.get())
                .execute(&mut *tx)
                .await?
                .rows_affected(),
            )
            .unwrap_or(0);
        }
        advance_cursor_in_tx(
            &mut tx,
            &self.tenant_id,
            FANOUT_STREAM,
            FANOUT_CONSUMER,
            last,
        )
        .await?;
        tx.commit().await?;
        Ok(enqueued)
    }

    async fn targets(&self) -> Result<Vec<Target>, WebhookError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(
            "SELECT id, workspace_id, event_types, start_sequence FROM webhook_subscriptions \
             WHERE tenant_id = $1",
        )
        .bind(&self.tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|row| {
                let types: Value = row.try_get("event_types")?;
                Ok(Target {
                    id: row.try_get("id")?,
                    workspace_id: row.try_get("workspace_id")?,
                    event_types: types
                        .as_array()
                        .map(|types| {
                            types
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    start_sequence: row.try_get("start_sequence")?,
                })
            })
            .collect()
    }
}
