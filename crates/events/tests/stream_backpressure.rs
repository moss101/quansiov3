//! Slow-consumer backpressure test (CORE-009, DOMAIN.md §9.3, §15).
//!
//! A consumer that stops reading is disconnected with the typed `STREAM_BACKPRESSURE`
//! error once the bounded buffer is exhausted, instead of the server buffering without
//! bound; the event publisher is never blocked by that consumer.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use std::time::Duration;

use quansio_events::{Channel, EventStore, StreamConfig, StreamError, StreamSubscription};
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, commit_event, drop_pool, fresh_migrated_database, scratch_name,
    seed_tenant, SeedEvent,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0B0001";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0B0001";
const RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0B0001";

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_migrated_database(&name).await?;
    seed_tenant(&pool, TENANT, WORKSPACE).await;
    Some((name, pool))
}

async fn commit_run(store: &EventStore, version: u64) {
    commit_event(
        store,
        TENANT,
        SeedEvent {
            aggregate_type: "run",
            aggregate_id: RUN,
            aggregate_version: version,
            event_type: "run.queued",
            workspace_id: Some(WORKSPACE),
            payload: serde_json::json!({"run_id": RUN}),
        },
    )
    .await;
}

#[tokio::test]
async fn a_slow_consumer_is_disconnected_without_blocking_the_publisher() {
    let Some((name, pool)) = prepare("streambackpressure").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    for version in 1..=10 {
        commit_run(&store, version).await;
    }

    let channel = Channel::parse(TENANT, &format!("run:{RUN}")).expect("canonical channel");
    let config = StreamConfig {
        buffer_capacity: 2,
        live_capacity: 2,
        batch_size: 256,
    };
    let (mut session, live) = store
        .subscribe(
            StreamSubscription::new(TENANT, "slow-consumer", vec![channel]).expect("subscription"),
            config,
        )
        .await
        .expect("subscribe");
    // The consumer never reads. The reader must hit the bounded buffer and record the
    // backpressure failure instead of growing without bound.
    drop(live);
    let mut failed = false;
    for _ in 0..500 {
        if session.is_failed() {
            failed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        failed,
        "the reader must observe backpressure while the consumer is not reading"
    );

    let mut buffered = 0;
    let error = loop {
        match tokio::time::timeout(Duration::from_secs(5), session.next_frame()).await {
            Ok(Ok(frame)) => {
                assert_eq!(frame.kind(), "event");
                buffered += 1;
            }
            Ok(Err(error)) => break error,
            Err(_) => panic!("next_frame hung instead of reporting backpressure"),
        }
    };
    assert!(
        matches!(error, StreamError::Backpressure { capacity: 2 }),
        "typed backpressure error, got {error:?}"
    );
    assert_eq!(error.error_code(), "STREAM_BACKPRESSURE");
    assert!(error.is_retryable(), "reconnecting with a cursor recovers");
    assert_eq!(
        buffered, 2,
        "exactly the documented bounded buffer capacity is delivered"
    );
    session.close().await;

    // The publisher is not blocked by the disconnected consumer.
    tokio::time::timeout(Duration::from_secs(5), async {
        for version in 11..=13 {
            commit_run(&store, version).await;
        }
    })
    .await
    .expect("the event publisher must not be blocked by a slow consumer");
    let events = store
        .read_events_after(TENANT, None, 100)
        .await
        .expect("read events");
    assert_eq!(events.len(), 13, "every event committed");

    drop_pool(&pool, &name).await;
}
