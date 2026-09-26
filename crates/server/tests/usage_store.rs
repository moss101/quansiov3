//! OPS-004: the persisted usage projection (`usage_records`) answers exactly what a rebuild from the
//! tenant's events answers, wherever the projection's cursor stands, and replaying the events meters
//! nothing twice (DOMAIN.md §13.4).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (the dev stack). Absent → `BLOCKED_EXTERNAL`.

use std::collections::BTreeMap;

use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventStore, EventType};
use quansio_server::control::schema;
use quansio_server::usage::{UsageMeter, UsageProjection, UsageStore, UsageSummary};
use serde_json::{json, Value};

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0SAGE1";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0SAGE1";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0SAGE1";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0SAGE1";

async fn emit(
    pool: &sqlx::PgPool,
    workspace: Option<&str>,
    aggregate_id: &str,
    event_type: &str,
    payload: Value,
) {
    let aggregate_id = aggregate_id.to_string();
    let workspace = workspace.map(str::to_string);
    let event_type = event_type.to_string();
    EventStore::new(pool.clone())
        .commit_mutation(TENANT, move |conn, batch| {
            Box::pin(async move {
                let version: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(aggregate_version), 0) + 1 FROM runtime_events \
                     WHERE aggregate_type = 'usage_test' AND aggregate_id = $1",
                )
                .bind(&aggregate_id)
                .fetch_one(&mut *conn)
                .await?;
                let mut draft = EventDraft::new(
                    "usage_test",
                    aggregate_id,
                    u64::try_from(version).unwrap_or(1),
                    EventType::parse(&event_type).expect("event type"),
                    CorrelationId::generate(&mut UlidGenerator::new()),
                    Actor::system("test"),
                )
                .with_payload(payload);
                if let Some(workspace) = workspace {
                    draft = draft.with_workspace(workspace);
                }
                batch.emit(draft);
                Ok(())
            })
        })
        .await
        .expect("event");
}

/// `count` events that meter something (and some that meter nothing), alternating workspaces.
async fn emit_usage(pool: &sqlx::PgPool, from: usize, count: usize) {
    for index in from..from + count {
        let workspace = (index % 3 != 0).then_some(WORKSPACE);
        let quantity = i64::try_from(index).unwrap_or(0) + 1;
        match index % 4 {
            0 => emit(pool, workspace, &format!("mc{index}"), "model.usage_recorded",
                json!({ "input_tokens": quantity * 10, "output_tokens": quantity, "cache_tokens": 0 })).await,
            1 => emit(pool, workspace, &format!("av{index}"), "artifact.version_added",
                json!({ "size_bytes": quantity * 100 })).await,
            2 => emit(pool, workspace, &format!("th{index}"), "thread.created", json!({})).await,
            _ => emit(pool, workspace, &format!("ts{index}"), "target.stopped",
                json!({ "machine_seconds": quantity })).await,
        }
    }
}

/// What a rebuild from every event says, per meter, optionally for one workspace.
async fn rebuilt(
    pool: &sqlx::PgPool,
    workspace: Option<&str>,
) -> (BTreeMap<&'static str, f64>, usize) {
    let events = EventStore::new(pool.clone())
        .read_events_after(TENANT, None, 1_000_000)
        .await
        .expect("events");
    let projection = UsageProjection::rebuild(&events);
    let records: Vec<_> = projection
        .records()
        .iter()
        .filter(|record| workspace.is_none_or(|ws| record.workspace_id.as_deref() == Some(ws)))
        .collect();
    let mut totals = BTreeMap::new();
    for meter in UsageMeter::ALL {
        totals.insert(
            meter.as_str(),
            records
                .iter()
                .filter(|record| record.meter == meter)
                .map(|record| record.quantity)
                .sum(),
        );
    }
    (totals, records.len())
}

fn totals(summary: &UsageSummary) -> BTreeMap<&'static str, f64> {
    summary
        .meters
        .iter()
        .map(|meter| (meter.meter.as_str(), meter.total))
        .collect()
}

async fn assert_matches_rebuild(pool: &sqlx::PgPool, store: &UsageStore, when: &str) {
    for workspace in [None, Some(WORKSPACE)] {
        let summary = store.summary(workspace, 500).await.expect("summary");
        let (expected, records) = rebuilt(pool, workspace).await;
        assert_eq!(totals(&summary), expected, "{when} ({workspace:?})");
        assert_eq!(
            summary.record_count, records as u64,
            "{when} ({workspace:?})"
        );
        assert!(summary.complete, "{when}");
    }
}

async fn stored_rows(pool: &sqlx::PgPool) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_records WHERE tenant_id = $1")
        .bind(TENANT)
        .fetch_one(&mut *tx)
        .await
        .expect("rows");
    tx.commit().await.expect("commit");
    rows
}

#[tokio::test]
async fn a_usage_read_is_the_whole_history_wherever_the_projection_stands() {
    let Some(name) = common::admin_url().map(|_| common::scratch_name("usage_store")) else {
        common::blocked_marker();
        return;
    };
    let pool = common::fresh_database(&name)
        .await
        .expect("scratch database");
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    let store = UsageStore::new(pool.clone(), TENANT);

    emit_usage(&pool, 0, 40).await;
    // Nothing materialized yet: the read projects every event itself.
    assert_matches_rebuild(&pool, &store, "before any materialization").await;
    // Part of the history materialized, the rest read on the fly.
    assert_eq!(store.materialize(13).await.expect("materialize"), 13);
    assert_matches_rebuild(&pool, &store, "cursor mid-history").await;
    // All of it materialized.
    store.catch_up(4).await.expect("catch up");
    assert_matches_rebuild(&pool, &store, "cursor at the end").await;
    // New events after the cursor.
    emit_usage(&pool, 40, 25).await;
    assert_matches_rebuild(&pool, &store, "new events after the cursor").await;
    store.catch_up(4).await.expect("catch up");
    let (_, records) = rebuilt(&pool, None).await;
    assert_eq!(
        stored_rows(&pool).await,
        i64::try_from(records).unwrap_or(0)
    );

    // Replaying the whole stream (a lost cursor) meters nothing twice.
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("DELETE FROM event_cursors WHERE tenant_id = $1 AND consumer = 'usage-projection'")
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("drop the cursor");
    tx.commit().await.expect("commit");
    store.catch_up(4).await.expect("replay");
    assert_eq!(
        stored_rows(&pool).await,
        i64::try_from(records).unwrap_or(0)
    );
    assert_matches_rebuild(&pool, &store, "after a replay").await;

    // The listing is bounded; the count and the totals are not.
    let summary = store.summary(None, 5).await.expect("summary");
    assert_eq!(summary.recent.len(), 5);
    assert_eq!(summary.record_count, records as u64);
    assert!(summary
        .recent
        .windows(2)
        .all(|pair| pair[0].occurred_at >= pair[1].occurred_at));

    common::drop_pool(&pool, &name).await;
}
