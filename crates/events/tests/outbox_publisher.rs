//! Outbox publishing, crash replay and fail-closed tests (CORE-003).
//!
//! The publisher is at-least-once. A real broker deduplicates by message id
//! (`Nats-Msg-Id = event_id`), so an event sent twice is externally visible once. The
//! recording transport here models that broker contract.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::{
    Actor, EventDraft, EventError, EventStore, EventTransport, EventType, OutboxPublisher,
    TransportError, UnavailableTransport,
};
use serde_json::Value;
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, drop_pool, fresh_migrated_database, scratch_name, seed_tenant,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

/// A broker stub that records every publish attempt, keeps only the first delivery per
/// message id (the broker-side dedup) and can fail once after accepting a message.
#[derive(Default)]
struct RecordingTransport {
    sent: Mutex<Vec<String>>,
    visible: Mutex<HashMap<String, Value>>,
    fail_after_record: AtomicBool,
}

impl RecordingTransport {
    fn new(fail_after_record: bool) -> Self {
        let transport = Self::default();
        transport
            .fail_after_record
            .store(fail_after_record, Ordering::SeqCst);
        transport
    }

    fn attempts(&self, msg_id: &str) -> usize {
        self.sent
            .lock()
            .expect("sent lock")
            .iter()
            .filter(|candidate| candidate.as_str() == msg_id)
            .count()
    }

    fn visible(&self, msg_id: &str) -> bool {
        self.visible
            .lock()
            .expect("visible lock")
            .contains_key(msg_id)
    }

    fn visible_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .visible
            .lock()
            .expect("visible lock")
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}

#[async_trait]
impl EventTransport for RecordingTransport {
    async fn publish(
        &self,
        _subject: &str,
        msg_id: &str,
        payload: &Value,
    ) -> Result<(), TransportError> {
        self.sent
            .lock()
            .expect("sent lock")
            .push(msg_id.to_string());
        // Broker-side dedup on Nats-Msg-Id: the first delivery is the visible one.
        self.visible
            .lock()
            .expect("visible lock")
            .entry(msg_id.to_string())
            .or_insert_with(|| payload.clone());
        if self.fail_after_record.swap(false, Ordering::SeqCst) {
            return Err(TransportError::Unavailable(
                "simulated crash after the broker accepted the message".to_string(),
            ));
        }
        Ok(())
    }
}

fn draft(index: u64) -> EventDraft {
    let mut generator = UlidGenerator::new();
    let correlation_id = CorrelationId::generate(&mut generator);
    EventDraft::new(
        "run",
        "run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        index,
        EventType::parse("run.succeeded").expect("canonical type"),
        correlation_id,
        Actor::system("outbox_test"),
    )
    .with_payload(serde_json::json!({"index": index}))
}

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_migrated_database(&name).await?;
    seed_tenant(&pool, TENANT, WORKSPACE).await;
    Some((name, pool))
}

async fn write_events(store: &EventStore, count: u64) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..count {
        let staged = draft(index + 1);
        let event_id = staged.event_id.to_string();
        store
            .commit_mutation(TENANT, move |conn, batch| {
                Box::pin(async move {
                    sqlx::query("UPDATE tenants SET name = $2 WHERE id = $1")
                        .bind(TENANT)
                        .bind(format!("event-{index}"))
                        .execute(&mut *conn)
                        .await?;
                    batch.emit(staged);
                    Ok(())
                })
            })
            .await
            .expect("commit event");
        ids.push(event_id);
    }
    ids
}

#[tokio::test]
async fn crash_between_send_and_mark_cannot_duplicate_and_loses_no_event() {
    let Some((name, pool)) = prepare("crash").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    let event_ids = write_events(&store, 3).await;
    let first = event_ids.first().expect("first event").clone();

    let transport = Arc::new(RecordingTransport::new(true));
    let publisher = OutboxPublisher::new(store.clone(), Arc::clone(&transport));

    // The broker accepted the first message, then the publisher "crashed" before it
    // could mark the row published.
    let error = publisher
        .publish_pending(TENANT)
        .await
        .expect_err("the simulated crash surfaces as a transport error");
    assert!(matches!(error, EventError::Transport(_)));
    assert!(
        transport.visible(&first),
        "the broker accepted the first event"
    );
    assert_eq!(transport.attempts(&first), 1);

    let unpublished: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE tenant_id = $1 AND published_at IS NULL",
    )
    .bind(TENANT)
    .fetch_one(&pool)
    .await
    .expect("unpublished count");
    assert_eq!(
        unpublished, 3,
        "nothing is marked published after the crash"
    );

    let (attempts, last_error, published): (
        i32,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        "SELECT attempts, last_error, published_at FROM event_outbox WHERE event_id = $1",
    )
    .bind(&first)
    .fetch_one(&pool)
    .await
    .expect("first outbox row");
    assert_eq!(attempts, 1);
    assert!(
        last_error.is_some(),
        "the failure is recorded for diagnosis"
    );
    assert!(published.is_none());

    // Retry: the same event is re-sent, the broker dedups it, and the rest are relayed.
    let report = publisher.publish_pending(TENANT).await.expect("retry");
    assert_eq!(report.published, 3);
    assert_eq!(report.attempted, 3);
    assert_eq!(transport.attempts(&first), 2, "the first event was retried");
    assert!(transport.visible(&first), "still exactly one visible event");

    // At-least-once delivery with no duplicates: every event visible exactly once, no
    // event lost, and every outbox row marked published.
    let mut expected = event_ids.clone();
    expected.sort();
    assert_eq!(
        transport.visible_ids(),
        expected,
        "no event lost, no duplicates"
    );
    let still_unpublished: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE tenant_id = $1 AND published_at IS NULL",
    )
    .bind(TENANT)
    .fetch_one(&pool)
    .await
    .expect("unpublished count");
    assert_eq!(still_unpublished, 0);

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn unavailable_transport_fails_closed_instead_of_dropping() {
    let Some((name, pool)) = prepare("unavailable").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    let event_ids = write_events(&store, 1).await;

    let publisher = OutboxPublisher::new(store.clone(), Arc::new(UnavailableTransport));
    let error = publisher
        .publish_pending(TENANT)
        .await
        .expect_err("no broker configured must fail");
    assert!(matches!(
        error,
        EventError::Transport(TransportError::Unavailable(_))
    ));

    let (unpublished, attempts, last_error): (i64, i32, Option<String>) = sqlx::query_as(
        "SELECT count(*)::bigint, max(attempts), max(last_error) FROM event_outbox WHERE tenant_id = $1",
    )
    .bind(TENANT)
    .fetch_one(&pool)
    .await
    .expect("outbox row");
    assert_eq!(unpublished, 1, "the event stays in the outbox");
    assert_eq!(attempts, 1);
    assert!(last_error.is_some());

    // The event itself is untouched: failing to publish never deletes canonical state.
    let stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM runtime_events WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_one(&pool)
            .await
            .expect("event count");
    assert_eq!(stored, 1);
    assert_eq!(event_ids.len(), 1);

    drop_pool(&pool, &name).await;
}
