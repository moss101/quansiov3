//! Projection crash/resume test (CORE-009).
//!
//! A projection checkpoint is a cursor over the tenant RuntimeEvent stream, distinct
//! from the authoritative `sequence`. After a crash the projection must resume exactly
//! where it stopped: only the missed events are applied, in order, once each.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use quansio_core::Sequence;
use quansio_events::{EventStore, ProjectionRunner, RunStatusProjection};
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, commit_event, connect_database, drop_pool, fresh_migrated_database,
    scratch_name, seed_tenant, SeedEvent,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0R0001";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0R0001";
const RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0R0001";

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_migrated_database(&name).await?;
    seed_tenant(&pool, TENANT, WORKSPACE).await;
    Some((name, pool))
}

async fn commit_run(
    store: &EventStore,
    version: u64,
    event_type: &str,
    payload: serde_json::Value,
) {
    commit_event(
        store,
        TENANT,
        SeedEvent {
            aggregate_type: "run",
            aggregate_id: RUN,
            aggregate_version: version,
            event_type,
            workspace_id: Some(WORKSPACE),
            payload,
        },
    )
    .await;
}

#[tokio::test]
async fn checkpoint_resumes_after_a_crash_without_gaps_or_duplicates() {
    let Some((name, pool)) = prepare("projresume").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    commit_run(&store, 1, "run.created", serde_json::json!({})).await;
    commit_run(&store, 2, "run.queued", serde_json::json!({})).await;
    commit_run(&store, 3, "run.started", serde_json::json!({})).await;

    let runs = ProjectionRunner::new(store.clone(), RunStatusProjection::new());
    let first = runs.catch_up_all(TENANT, 2).await.expect("catch up");
    assert_eq!(
        first.consumed,
        vec![
            Sequence::new(1).expect("valid"),
            Sequence::new(2).expect("valid"),
            Sequence::new(3).expect("valid"),
        ]
    );
    assert_eq!(
        runs.checkpoint(TENANT).await.expect("checkpoint"),
        Some(Sequence::new(3).expect("valid"))
    );
    let (status_before, count_before): (String, i64) = sqlx::query_as(
        "SELECT status, event_count FROM run_status_projection WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(RUN)
    .fetch_one(&pool)
    .await
    .expect("run row");
    assert_eq!((status_before.as_str(), count_before), ("RUNNING", 3));

    // The process dies: every pooled connection is closed. The checkpoint lives in
    // PostgreSQL (event_cursors), not in the process.
    pool.close().await;

    let pool = connect_database(&name).await.expect("reconnect");
    let store = EventStore::new(pool.clone());
    let runs = ProjectionRunner::new(store.clone(), RunStatusProjection::new());
    assert_eq!(
        runs.checkpoint(TENANT).await.expect("checkpoint"),
        Some(Sequence::new(3).expect("valid")),
        "the checkpoint survived the restart"
    );

    // Events that arrived while the projection was down.
    commit_run(
        &store,
        4,
        "run.waiting",
        serde_json::json!({"status": "WAITING_QUESTION"}),
    )
    .await;
    commit_run(&store, 5, "run.resumed", serde_json::json!({})).await;

    let resumed = runs.catch_up_all(TENANT, 1).await.expect("resume");
    assert_eq!(
        resumed.consumed,
        vec![
            Sequence::new(4).expect("valid"),
            Sequence::new(5).expect("valid"),
        ],
        "only the missed events, in order, with no gap and no duplicate"
    );
    assert_eq!(resumed.applied, 2);
    assert_eq!(resumed.duplicates, 0);
    assert_eq!(
        runs.checkpoint(TENANT).await.expect("checkpoint"),
        Some(Sequence::new(5).expect("valid"))
    );

    let (status, count, last_sequence): (String, i64, i64) = sqlx::query_as(
        "SELECT status, event_count, last_sequence FROM run_status_projection \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(RUN)
    .fetch_one(&pool)
    .await
    .expect("run row");
    assert_eq!(status, "RUNNING");
    assert_eq!(count, 5, "state continued instead of restarting");
    assert_eq!(last_sequence, 5);

    drop_pool(&pool, &name).await;
}
