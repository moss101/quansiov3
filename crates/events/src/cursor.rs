//! Durable consumer cursors over the RuntimeEvent stream (DOMAIN.md §1.2, §9.3).
//!
//! `event_cursors` persists the last delivered `sequence` per `(tenant, stream_id,
//! consumer)`. Clients receive the opaque [`Cursor`](quansio_core::Cursor) token; on
//! reconnect the token is decoded and the store replays exactly the events after the
//! stored sequence, in tenant order.

use chrono::{DateTime, Utc};
use quansio_core::{Cursor, Sequence, UlidGenerator};
use sqlx::Row;

use crate::envelope::RuntimeEvent;
use crate::error::EventError;
use crate::store::EventStore;

/// A persisted consumer position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorState {
    /// The stream this consumer follows.
    pub stream_id: String,
    /// The consumer identity.
    pub consumer: String,
    /// The opaque resumable position.
    pub cursor: Cursor,
    /// When the position was last advanced.
    pub updated_at: DateTime<Utc>,
}

/// One batch of a resumed stream.
#[derive(Debug, Clone)]
pub struct ResumeBatch {
    /// The events after the resume position, in tenant sequence order.
    pub events: Vec<RuntimeEvent>,
    /// The cursor to present on the next resume (`None` only for an empty stream).
    pub cursor: Option<Cursor>,
}

impl EventStore {
    /// Load the persisted position for `(stream_id, consumer)`, if any.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn load_cursor_state(
        &self,
        tenant_id: &str,
        stream_id: &str,
        consumer: &str,
    ) -> Result<Option<CursorState>, EventError> {
        let mut tx = self.begin_tenant_transaction(tenant_id).await?;
        let row = sqlx::query(
            "SELECT stream_id, consumer, sequence, updated_at FROM event_cursors \
             WHERE tenant_id = $1 AND stream_id = $2 AND consumer = $3",
        )
        .bind(tenant_id)
        .bind(stream_id)
        .bind(consumer)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let sequence = Sequence::new(row.try_get::<i64, _>("sequence")?)
            .map_err(|error| EventError::MalformedEventRow(error.to_string()))?;
        Ok(Some(CursorState {
            stream_id: row.try_get("stream_id")?,
            consumer: row.try_get("consumer")?,
            cursor: Cursor::new(stream_id, sequence),
            updated_at: row.try_get("updated_at")?,
        }))
    }

    /// Load only the opaque cursor for `(stream_id, consumer)`.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn load_cursor(
        &self,
        tenant_id: &str,
        stream_id: &str,
        consumer: &str,
    ) -> Result<Option<Cursor>, EventError> {
        Ok(self
            .load_cursor_state(tenant_id, stream_id, consumer)
            .await?
            .map(|state| state.cursor))
    }

    /// Persist an advanced cursor, creating the row on first use.
    ///
    /// # Errors
    /// Returns [`EventError::CursorStreamMismatch`] when the cursor belongs to another
    /// stream, or a database error.
    pub async fn save_cursor(
        &self,
        tenant_id: &str,
        stream_id: &str,
        consumer: &str,
        cursor: &Cursor,
    ) -> Result<CursorState, EventError> {
        if cursor.stream_id() != stream_id {
            return Err(EventError::CursorStreamMismatch {
                expected: stream_id.to_string(),
                found: cursor.stream_id().to_string(),
            });
        }
        let mut tx = self.begin_tenant_transaction(tenant_id).await?;
        let id = format!("ecr_{}", UlidGenerator::new().generate());
        let row = sqlx::query(
            "INSERT INTO event_cursors (id, tenant_id, stream_id, consumer, sequence) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant_id, stream_id, consumer) \
             DO UPDATE SET sequence = EXCLUDED.sequence, updated_at = now() \
             RETURNING stream_id, consumer, sequence, updated_at",
        )
        .bind(id)
        .bind(tenant_id)
        .bind(stream_id)
        .bind(consumer)
        .bind(cursor.sequence().get())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let sequence = Sequence::new(row.try_get::<i64, _>("sequence")?)
            .map_err(|error| EventError::MalformedEventRow(error.to_string()))?;
        Ok(CursorState {
            stream_id: row.try_get("stream_id")?,
            consumer: row.try_get("consumer")?,
            cursor: Cursor::new(stream_id, sequence),
            updated_at: row.try_get("updated_at")?,
        })
    }

    /// Resume a stream.
    ///
    /// When `token` is supplied it is decoded with [`Cursor::decode`] and must belong to
    /// `stream_id`; otherwise the persisted cursor for `(stream_id, consumer)` is used.
    /// Returns exactly the events after that position, in order, plus the cursor to
    /// present on the next resume.
    ///
    /// # Errors
    /// Returns [`EventError::CursorStreamMismatch`] for a token from another stream, a
    /// cursor error for a malformed token, or a database error.
    pub async fn resume(
        &self,
        tenant_id: &str,
        stream_id: &str,
        consumer: &str,
        token: Option<&str>,
        limit: i64,
    ) -> Result<ResumeBatch, EventError> {
        let position = match token {
            Some(token) => {
                let decoded = Cursor::decode(token)?;
                if decoded.stream_id() != stream_id {
                    return Err(EventError::CursorStreamMismatch {
                        expected: stream_id.to_string(),
                        found: decoded.stream_id().to_string(),
                    });
                }
                Some(decoded)
            }
            None => self.load_cursor(tenant_id, stream_id, consumer).await?,
        };
        let events = self
            .read_events_after(
                tenant_id,
                position.as_ref().map(|cursor| cursor.sequence()),
                limit,
            )
            .await?;
        let cursor = events
            .last()
            .map(|event| Cursor::new(stream_id, event.sequence))
            .or(position);
        Ok(ResumeBatch { events, cursor })
    }
}
