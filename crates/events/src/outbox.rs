//! Transactional outbox publisher (DOMAIN.md §9.1).
//!
//! The store writes an `event_outbox` row in the same transaction as the event. A
//! publisher relays unpublished rows to an [`EventTransport`] and marks them published.
//! Delivery is at-least-once: a crash between "sent" and "marked published" replays the
//! row, and deduplication is the transport's job through the message id (NATS
//! `Nats-Msg-Id = event_id`), so the same event is never externally visible twice.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::Row;

use crate::error::{EventError, TransportError};
use crate::store::EventStore;

/// Transport for published RuntimeEvents.
///
/// Implementations relay one message to the broker and must make `msg_id` the
/// broker-side deduplication key (NATS `Nats-Msg-Id` per DOMAIN.md §9.1). The subject
/// is `q.<tenant>.<aggregate_type>.<type>`.
#[async_trait]
pub trait EventTransport: Send + Sync {
    /// Publish one event. The implementation must be idempotent for a given `msg_id`.
    ///
    /// # Errors
    /// Returns [`TransportError`] when the broker is unreachable or rejects the message;
    /// the caller leaves the outbox row unpublished and retries later.
    async fn publish(
        &self,
        subject: &str,
        msg_id: &str,
        payload: &Value,
    ) -> Result<(), TransportError>;
}

/// Fail-closed transport used when no broker is configured.
///
/// Publishing returns [`TransportError::Unavailable`] instead of succeeding silently, so
/// events stay in the outbox and are retried once a broker is configured; nothing is
/// dropped.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableTransport;

#[async_trait]
impl EventTransport for UnavailableTransport {
    async fn publish(
        &self,
        _subject: &str,
        _msg_id: &str,
        _payload: &Value,
    ) -> Result<(), TransportError> {
        Err(TransportError::Unavailable(
            "no NATS JetStream event transport is configured".to_string(),
        ))
    }
}

/// One batch of outbox publishing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishReport {
    /// Rows that were sent and marked published.
    pub published: usize,
    /// Rows attempted in this batch (a transport failure stops the batch).
    pub attempted: usize,
}

/// Relays unpublished outbox rows for one tenant.
pub struct OutboxPublisher<T: EventTransport> {
    store: EventStore,
    transport: Arc<T>,
    batch_size: i64,
}

impl<T: EventTransport> OutboxPublisher<T> {
    /// Build a publisher with the default batch size.
    #[must_use]
    pub fn new(store: EventStore, transport: Arc<T>) -> Self {
        Self {
            store,
            transport,
            batch_size: 64,
        }
    }

    /// Set the maximum number of rows relayed per call.
    #[must_use]
    pub fn with_batch_size(mut self, batch_size: i64) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// The store this publisher reads from.
    #[must_use]
    pub fn store(&self) -> &EventStore {
        &self.store
    }

    /// Publish pending rows for one tenant.
    ///
    /// Rows are locked `FOR UPDATE SKIP LOCKED` so concurrent publishers do not collide.
    /// Each row is sent with `msg_id = event_id`, then marked published in the same
    /// transaction. If the transport fails, the attempt count and last error are
    /// committed and the error is returned; the row stays unpublished for a retry.
    ///
    /// # Errors
    /// Returns [`EventError::Transport`] when the transport fails, or a database error.
    pub async fn publish_pending(&self, tenant_id: &str) -> Result<PublishReport, EventError> {
        let mut tx = self.store.begin_tenant_transaction(tenant_id).await?;
        let rows = sqlx::query(
            "SELECT event_id, subject, payload FROM event_outbox \
             WHERE tenant_id = $1 AND published_at IS NULL \
             ORDER BY created_at, event_id LIMIT $2 FOR UPDATE SKIP LOCKED",
        )
        .bind(tenant_id)
        .bind(self.batch_size)
        .fetch_all(&mut *tx)
        .await?;

        let mut published = 0usize;
        let attempted = rows.len();
        for row in rows {
            let event_id: String = row.try_get("event_id")?;
            let subject: String = row.try_get("subject")?;
            let payload: Value = row.try_get("payload")?;
            match self.transport.publish(&subject, &event_id, &payload).await {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE event_outbox SET published_at = now(), attempts = attempts + 1, \
                         last_error = NULL WHERE event_id = $1",
                    )
                    .bind(&event_id)
                    .execute(&mut *tx)
                    .await?;
                    published += 1;
                }
                Err(error) => {
                    sqlx::query(
                        "UPDATE event_outbox SET attempts = attempts + 1, last_error = $2 \
                         WHERE event_id = $1",
                    )
                    .bind(&event_id)
                    .bind(error.to_string())
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Err(EventError::Transport(error));
                }
            }
        }
        tx.commit().await?;
        Ok(PublishReport {
            published,
            attempted,
        })
    }
}
