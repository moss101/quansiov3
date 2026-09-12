//! Projection determinism and idempotency tests (CORE-009).
//!
//! A projection is a pure function of the RuntimeEvent stream: incremental application
//! and a rebuild from zero must produce identical bytes, and re-applying an applied
//! event must not double-count or regress state.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use quansio_core::Sequence;
use quansio_events::{
    ApplyOutcome, EventStore, ProjectionRunner, RunStatusProjection, WorkNodeStatusProjection,
};
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, commit_event, drop_pool, fresh_migrated_database, scratch_name,
    seed_tenant, SeedEvent,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0P0001";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0P0001";
const RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0P0001";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0P0001";
const AGENT_THREAD: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0P0001";

async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_migrated_database(&name).await?;
    seed_tenant(&pool, TENANT, WORKSPACE).await;
    Some((name, pool))
}

fn seed_event<'a>(
    aggregate_type: &'a str,
    aggregate_id: &'a str,
    version: u64,
    event_type: &'a str,
    payload: serde_json::Value,
) -> SeedEvent<'a> {
    SeedEvent {
        aggregate_type,
        aggregate_id,
        aggregate_version: version,
        event_type,
        workspace_id: Some(WORKSPACE),
        payload,
    }
}

/// Nine events: a run lifecycle and a work-node status change, interleaved.
async fn seed_stream(store: &EventStore) {
    commit_event(
        store,
        TENANT,
        seed_event(
            "work",
            NODE,
            1,
            "work.node_created",
            serde_json::json!({"kind": "objective", "title": "Ship V8.1", "status": "ready"}),
        ),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event(
            "run",
            RUN,
            1,
            "run.created",
            serde_json::json!({"work_node_id": NODE, "agent_thread_id": AGENT_THREAD}),
        ),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event("run", RUN, 2, "run.queued", serde_json::json!({})),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event("run", RUN, 3, "run.started", serde_json::json!({})),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event(
            "work",
            NODE,
            2,
            "work.node_status_changed",
            serde_json::json!({"status": "in_progress"}),
        ),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event(
            "run",
            RUN,
            4,
            "run.waiting",
            serde_json::json!({"status": "WAITING_APPROVAL"}),
        ),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event("run", RUN, 5, "run.resumed", serde_json::json!({})),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event(
            "work",
            NODE,
            3,
            "work.node_status_changed",
            serde_json::json!({"status": "done"}),
        ),
    )
    .await;
    commit_event(
        store,
        TENANT,
        seed_event(
            "run",
            RUN,
            6,
            "run.succeeded",
            serde_json::json!({"terminal_reason": "completed"}),
        ),
    )
    .await;
}

#[tokio::test]
async fn rebuild_is_byte_identical_to_incremental_application() {
    let Some((name, pool)) = prepare("projdet").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    seed_stream(&store).await;

    let runs = ProjectionRunner::new(store.clone(), RunStatusProjection::new());
    let nodes = ProjectionRunner::new(store.clone(), WorkNodeStatusProjection::new());

    let incremental_runs = runs.catch_up_all(TENANT, 2).await.expect("catch up runs");
    let incremental_nodes = nodes.catch_up_all(TENANT, 3).await.expect("catch up nodes");
    let total = store
        .read_events_after(TENANT, None, 100)
        .await
        .expect("read events")
        .len();
    assert_eq!(total, 9);
    assert_eq!(
        incremental_runs.consumed.len(),
        total,
        "the projection consumes every event in sequence order"
    );
    assert_eq!(incremental_nodes.consumed.len(), total);
    assert_eq!(
        runs.checkpoint(TENANT).await.expect("checkpoint"),
        Some(Sequence::new(9).expect("valid"))
    );
    assert_eq!(incremental_runs.applied, 6, "six run lifecycle events");

    let incremental_run_bytes = runs.snapshot(TENANT).await.expect("run snapshot");
    let incremental_node_bytes = nodes.snapshot(TENANT).await.expect("node snapshot");
    assert_ne!(incremental_run_bytes, b"[]", "the read model has rows");
    assert_ne!(incremental_node_bytes, b"[]");

    // Rebuild from zero and compare the state byte for byte.
    runs.rebuild(TENANT, 4).await.expect("rebuild runs");
    nodes.rebuild(TENANT, 4).await.expect("rebuild nodes");
    assert_eq!(
        runs.snapshot(TENANT).await.expect("run snapshot"),
        incremental_run_bytes,
        "run status projection is deterministic"
    );
    assert_eq!(
        nodes.snapshot(TENANT).await.expect("node snapshot"),
        incremental_node_bytes,
        "work node status projection is deterministic"
    );

    // The read models actually describe the stream.
    let (status, event_count, last_sequence, terminal_reason): (String, i64, i64, Option<String>) =
        sqlx::query_as(
            "SELECT status, event_count, last_sequence, terminal_reason \
             FROM run_status_projection WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(TENANT)
        .bind(RUN)
        .fetch_one(&pool)
        .await
        .expect("run row");
    assert_eq!(status, "SUCCEEDED");
    assert_eq!(event_count, 6);
    assert_eq!(last_sequence, 9);
    assert_eq!(terminal_reason.as_deref(), Some("completed"));

    let (node_status, node_events, node_sequence): (String, i64, i64) = sqlx::query_as(
        "SELECT status, event_count, last_sequence FROM work_node_status_projection \
         WHERE tenant_id = $1 AND work_node_id = $2",
    )
    .bind(TENANT)
    .bind(NODE)
    .fetch_one(&pool)
    .await
    .expect("node row");
    assert_eq!(node_status, "done");
    assert_eq!(node_events, 3);
    assert_eq!(node_sequence, 8);

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn reapplying_an_applied_event_neither_double_counts_nor_regresses() {
    let Some((name, pool)) = prepare("projidem").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    seed_stream(&store).await;
    let runs = ProjectionRunner::new(store.clone(), RunStatusProjection::new());
    runs.catch_up_all(TENANT, 4).await.expect("catch up");
    let before = runs.snapshot(TENANT).await.expect("snapshot");

    // Re-applying an event below the checkpoint is refused by the runner itself.
    let events = store
        .read_events_after(TENANT, None, 100)
        .await
        .expect("read events");
    for event in &events {
        let outcome = runs.apply_event(TENANT, event).await.expect("re-apply");
        assert_eq!(
            outcome,
            ApplyOutcome::Duplicate,
            "sequence {} was already applied",
            event.sequence
        );
    }

    // Even with the checkpoint removed, the guarded upsert refuses to double-count or
    // to move the row backwards: every re-applied event is a no-op.
    sqlx::query(
        "DELETE FROM event_cursors WHERE tenant_id = $1 AND stream_id = $2 AND consumer = $3",
    )
    .bind(TENANT)
    .bind(runs.stream_id())
    .bind(quansio_events::PROJECTION_CONSUMER)
    .execute(&pool)
    .await
    .expect("clear checkpoint");
    let replay = runs.catch_up_all(TENANT, 3).await.expect("replay");
    assert_eq!(replay.applied, 0, "a re-applied event writes no row again");
    assert_eq!(replay.unchanged, 9);
    assert_eq!(
        runs.snapshot(TENANT).await.expect("snapshot"),
        before,
        "re-application is idempotent"
    );
    let (status, event_count): (String, i64) = sqlx::query_as(
        "SELECT status, event_count FROM run_status_projection \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(RUN)
    .fetch_one(&pool)
    .await
    .expect("run row");
    assert_eq!(status, "SUCCEEDED", "no regression to an earlier state");
    assert_eq!(event_count, 6, "no double counting");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn projections_do_not_write_rows_for_unrelated_events() {
    let Some((name, pool)) = prepare("projscope").await else {
        blocked_marker();
        return;
    };
    let store = EventStore::new(pool.clone());
    commit_event(
        &store,
        TENANT,
        SeedEvent {
            aggregate_type: "thread",
            aggregate_id: "thr_01J8Z3K6F1N8VQ2X5W9Y0P0001",
            aggregate_version: 1,
            event_type: "thread.created",
            workspace_id: Some(WORKSPACE),
            payload: serde_json::json!({}),
        },
    )
    .await;

    let runs = ProjectionRunner::new(store.clone(), RunStatusProjection::new());
    let report = runs.catch_up_all(TENANT, 10).await.expect("catch up");
    assert_eq!(report.consumed.len(), 1, "the event is consumed");
    assert_eq!(report.unchanged, 1);
    assert_eq!(
        runs.snapshot(TENANT).await.expect("snapshot"),
        b"[]",
        "no run row is invented from a thread event"
    );

    drop_pool(&pool, &name).await;
}
