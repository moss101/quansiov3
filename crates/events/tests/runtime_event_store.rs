//! Transactional store, concurrency and cursor tests (CORE-003).
//!
//! These run against real PostgreSQL: the atomicity of state + event + outbox, the
//! race-freedom of the tenant sequence counter and the resumability of consumer cursors
//! are database properties and are proven at that boundary.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use quansio_core::{CorrelationId, Cursor, EventId, Sequence, TypedId, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventError, EventStore, EventType};
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, drop_pool, fresh_migrated_database, scratch_name, seed_tenant,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const ROLLBACK_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const COMMITTED_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

fn event_type(value: &str) -> EventType {
    EventType::parse(value).expect("canonical event type")
}

fn draft(version: u64, payload: serde_json::Value) -> EventDraft {
    let mut generator = UlidGenerator::new();
    let correlation_id = CorrelationId::generate(&mut generator);
    EventDraft::new(
        "run",
        NODE,
        version,
        event_type("run.created"),
        correlation_id,
        Actor::system("runtime_event_store_test"),
    )
    .with_payload(payload)
}

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_migrated_database(&name).await?;
    seed_tenant(&pool, TENANT, WORKSPACE).await;
    Some((name, pool))
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .expect("count")
}

#[tokio::test]
async fn failing_mutation_rolls_back_state_event_and_outbox() {
    let Some((name, pool)) = prepare("rollback").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());

    let result: Result<(), EventError> = store
        .commit_mutation(TENANT, move |conn, batch| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'rollback me')",
                )
                .bind(ROLLBACK_WORKSPACE)
                .bind(TENANT)
                .execute(&mut *conn)
                .await?;
                batch.emit(draft(1, serde_json::json!({"kind": "rollback"})));
                // A later statement in the same mutation fails: the whole transaction,
                // including the staged event and its outbox row, must disappear.
                sqlx::query("INSERT INTO table_that_does_not_exist (x) VALUES (1)")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await;
    assert!(matches!(result, Err(EventError::Database(_))));

    assert_eq!(
        count(
            &pool,
            &format!("SELECT count(*) FROM workspaces WHERE id = '{ROLLBACK_WORKSPACE}'")
        )
        .await,
        0,
        "the state row must be rolled back"
    );
    assert_eq!(
        count(
            &pool,
            &format!("SELECT count(*) FROM runtime_events WHERE tenant_id = '{TENANT}'")
        )
        .await,
        0,
        "no event may survive a rolled-back mutation"
    );
    assert_eq!(
        count(
            &pool,
            &format!("SELECT count(*) FROM event_outbox WHERE tenant_id = '{TENANT}'")
        )
        .await,
        0,
        "no outbox row may survive a rolled-back mutation"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn committed_mutation_writes_state_event_and_outbox_together() {
    let Some((name, pool)) = prepare("commit").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    let staged = draft(1, serde_json::json!({"kind": "commit"}));
    let event_id = staged.event_id;

    let committed_id: EventId = store
        .commit_mutation(TENANT, move |conn, batch| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'committed')",
                )
                .bind(COMMITTED_WORKSPACE)
                .bind(TENANT)
                .execute(&mut *conn)
                .await?;
                batch.emit(staged);
                Ok(event_id)
            })
        })
        .await
        .expect("commit");
    assert_eq!(committed_id, event_id);

    let state = count(
        &pool,
        &format!("SELECT count(*) FROM workspaces WHERE id = '{COMMITTED_WORKSPACE}'"),
    )
    .await;
    assert_eq!(state, 1, "the state mutation is committed");

    let (stored_id, sequence, stored_type): (String, i64, String) =
        sqlx::query_as("SELECT id, sequence, type FROM runtime_events WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_one(&pool)
            .await
            .expect("event row");
    assert_eq!(stored_id, event_id.to_string());
    assert_eq!(sequence, 1, "first event in the tenant stream");
    assert_eq!(stored_type, "run.created");

    let (subject, published): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT subject, published_at FROM event_outbox WHERE event_id = $1")
            .bind(event_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("outbox row");
    assert_eq!(subject, format!("q.{TENANT}.run.run.created"));
    assert!(published.is_none(), "the publisher has not run yet");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn committing_without_an_event_is_refused() {
    let Some((name, pool)) = prepare("noevent").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());

    let result: Result<(), EventError> = store
        .commit_mutation(TENANT, |conn, _batch| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'eventless')",
                )
                .bind(ROLLBACK_WORKSPACE)
                .bind(TENANT)
                .execute(&mut *conn)
                .await?;
                Ok(())
            })
        })
        .await;
    assert!(matches!(result, Err(EventError::NoEventStaged)));

    assert_eq!(
        count(
            &pool,
            &format!("SELECT count(*) FROM workspaces WHERE id = '{ROLLBACK_WORKSPACE}'")
        )
        .await,
        0,
        "state must not commit without its event"
    );
    assert_eq!(
        count(
            &pool,
            &format!("SELECT count(*) FROM runtime_events WHERE tenant_id = '{TENANT}'")
        )
        .await,
        0
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn concurrent_writers_get_distinct_gap_free_sequences() {
    let Some((name, pool)) = prepare("concurrent").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());

    let workers = 4u64;
    let per_worker = 4u64;
    let mut handles = Vec::new();
    for worker in 0..workers {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let mut event_ids = Vec::new();
            for index in 0..per_worker {
                let staged = draft(
                    index + 1,
                    serde_json::json!({"worker": worker, "index": index}),
                );
                let event_id = staged.event_id;
                store
                    .commit_mutation(TENANT, move |conn, batch| {
                        Box::pin(async move {
                            // A real state statement per event; the tenant row is updated
                            // under the same transaction as the event.
                            sqlx::query("UPDATE tenants SET name = $2 WHERE id = $1")
                                .bind(TENANT)
                                .bind(format!("worker-{worker}-{index}"))
                                .execute(&mut *conn)
                                .await?;
                            batch.emit(staged);
                            Ok(())
                        })
                    })
                    .await
                    .expect("concurrent commit");
                event_ids.push(event_id);
            }
            event_ids
        }));
    }

    let mut all_ids = Vec::new();
    for handle in handles {
        all_ids.extend(handle.await.expect("worker task"));
    }

    let events = store
        .read_events_after(TENANT, None, 100)
        .await
        .expect("read events");
    let total = workers * per_worker;
    let sequences: Vec<i64> = events.iter().map(|event| event.sequence.get()).collect();
    assert_eq!(
        sequences,
        (1..=total as i64).collect::<Vec<i64>>(),
        "gap-free, monotonic 1..={total}"
    );
    assert_eq!(
        all_ids.len(),
        total as usize,
        "every concurrent commit wrote its event"
    );
    let mut ids: Vec<String> = events
        .iter()
        .map(|event| event.event_id.to_string())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total as usize, "distinct event identities");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_rolled_back_commit_releases_its_sequence_instead_of_burning_it() {
    let Some((name, pool)) = prepare("seqrollback").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());

    // Two staged events that share an event id: the second insert violates the primary
    // key after both sequence values were assigned, so the transaction fails and both
    // increments roll back with it.
    let mut generator = UlidGenerator::new();
    let duplicate_id = EventId::generate(&mut generator);
    let first = draft(1, serde_json::json!({"kind": "duplicate"})).with_event_id(duplicate_id);
    let second = draft(2, serde_json::json!({"kind": "duplicate"})).with_event_id(duplicate_id);
    let result: Result<(), EventError> = store
        .commit_mutation(TENANT, move |conn, batch| {
            Box::pin(async move {
                sqlx::query("UPDATE tenants SET name = $2 WHERE id = $1")
                    .bind(TENANT)
                    .bind("sequence-rollback")
                    .execute(&mut *conn)
                    .await?;
                batch.emit(first);
                batch.emit(second);
                Ok(())
            })
        })
        .await;
    assert!(matches!(result, Err(EventError::Database(_))));
    assert_eq!(
        count(
            &pool,
            &format!("SELECT count(*) FROM runtime_events WHERE tenant_id = '{TENANT}'")
        )
        .await,
        0,
        "a failed commit writes no event"
    );

    // The next successful commit starts at sequence 1: the failed attempt did not burn
    // a value, so the tenant stream stays gap-free.
    let staged = draft(1, serde_json::json!({"kind": "after-rollback"}));
    store
        .commit_mutation(TENANT, move |conn, batch| {
            Box::pin(async move {
                sqlx::query("UPDATE tenants SET name = $2 WHERE id = $1")
                    .bind(TENANT)
                    .bind("after-rollback")
                    .execute(&mut *conn)
                    .await?;
                batch.emit(staged);
                Ok(())
            })
        })
        .await
        .expect("commit after rollback");
    let events = store
        .read_events_after(TENANT, None, 10)
        .await
        .expect("read events");
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence.get())
            .collect::<Vec<_>>(),
        vec![1],
        "the rolled-back sequence is reused, not skipped"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn consumer_resumes_exactly_after_the_stored_sequence() {
    let Some((name, pool)) = prepare("cursor").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    for version in 1..=5u64 {
        let staged = draft(version, serde_json::json!({"index": version}));
        store
            .commit_mutation(TENANT, move |conn, batch| {
                Box::pin(async move {
                    sqlx::query("UPDATE tenants SET name = $2 WHERE id = $1")
                        .bind(TENANT)
                        .bind(format!("cursor-{version}"))
                        .execute(&mut *conn)
                        .await?;
                    batch.emit(staged);
                    Ok(())
                })
            })
            .await
            .expect("seed event");
    }

    let stream = "workspace:ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
    let consumer = "test-consumer";

    // First delivery: read the head of the stream and persist the cursor.
    let first = store
        .resume(TENANT, stream, consumer, None, 2)
        .await
        .expect("first resume");
    assert_eq!(
        first
            .events
            .iter()
            .map(|event| event.sequence.get())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let cursor = first.cursor.clone().expect("cursor after delivery");
    assert_eq!(cursor.sequence(), Sequence::new(2).expect("valid"));
    store
        .save_cursor(TENANT, stream, consumer, &cursor)
        .await
        .expect("persist cursor");

    // The persisted position round-trips through the opaque encoding.
    let loaded = store
        .load_cursor(TENANT, stream, consumer)
        .await
        .expect("load cursor")
        .expect("cursor exists");
    assert_eq!(loaded, cursor);
    assert_eq!(Cursor::decode(&loaded.encode()).expect("decode"), cursor);

    // Resuming with the client token returns exactly the missed events, in order.
    let resumed = store
        .resume(TENANT, stream, consumer, Some(&cursor.encode()), 100)
        .await
        .expect("resume from token");
    assert_eq!(
        resumed
            .events
            .iter()
            .map(|event| event.sequence.get())
            .collect::<Vec<_>>(),
        vec![3, 4, 5],
        "exactly the events after the stored sequence"
    );
    assert!(resumed
        .events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    let tail = resumed.cursor.expect("tail cursor");
    assert_eq!(tail.sequence(), Sequence::new(5).expect("valid"));
    store
        .save_cursor(TENANT, stream, consumer, &tail)
        .await
        .expect("persist tail cursor");

    // Resuming from the persisted cursor (no token) agrees with the token path.
    let by_state = store
        .resume(TENANT, stream, consumer, None, 100)
        .await
        .expect("resume from state");
    assert!(by_state.events.is_empty(), "already at the tail");

    // A cursor from another stream is rejected rather than silently accepted.
    let other = Cursor::new(
        "thread:thr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        Sequence::new(2).unwrap(),
    );
    let mismatch = store
        .resume(TENANT, stream, consumer, Some(&other.encode()), 100)
        .await
        .expect_err("stream mismatch");
    assert!(matches!(mismatch, EventError::CursorStreamMismatch { .. }));

    drop_pool(&pool, &name).await;
}
