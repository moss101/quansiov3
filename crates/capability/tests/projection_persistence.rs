//! Persistence/projection test (RUN-005, DOMAIN.md §6.2, §7.2, §9.2).
//!
//! The stored projection round-trips with its `inputs[]` digests, and the emitted
//! `capability.projected` event carries the same `inputs_digest`. The row and the event
//! are written on one canonical `EventStore` transaction, so a projection that
//! authorizes an effect is reconstructable.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

use chrono::{DateTime, Utc};
use quansio_capability::{
    assemble, stage_projected_event, Approval, Layer, LayerInput, ProjectionArgument,
    ProjectionRequest, ProjectionRow, ProjectionSubject, SubjectKind,
};
use quansio_core::{CorrelationId, Digest, UlidGenerator};
use quansio_events::{Actor, EventStore};
use sqlx::{PgPool, Row};

mod common;
use common::{
    admin_url, at, blocked_marker, constraints, drop_pool, fs, grant_with, scratch_name,
    seed_tenant, source_with_platform,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

fn subject() -> ProjectionSubject {
    ProjectionSubject::run("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
}

fn parse_subject_kind(value: &str) -> SubjectKind {
    match value {
        "run" => SubjectKind::Run,
        "agent_thread" => SubjectKind::AgentThread,
        "tool_call" => SubjectKind::ToolCall,
        other => panic!("unknown subject kind {other}"),
    }
}

async fn read_row(pool: &PgPool, id: &str) -> ProjectionRow {
    let mut tx = pool.begin().await.expect("begin read transaction");
    sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("tenant context");
    let row = sqlx::query(ProjectionRow::SELECT_SQL)
        .bind(id)
        .bind(TENANT)
        .fetch_one(&mut *tx)
        .await
        .expect("read projection row");
    tx.commit().await.expect("commit read transaction");

    ProjectionRow {
        id: row.try_get("id").expect("id"),
        tenant_id: row.try_get("tenant_id").expect("tenant_id"),
        workspace_id: row.try_get("workspace_id").expect("workspace_id"),
        subject_kind: parse_subject_kind(&row.try_get::<String, _>("subject_kind").expect("kind")),
        subject_id: row.try_get("subject_id").expect("subject_id"),
        inputs: serde_json::from_value(row.try_get("inputs").expect("inputs"))
            .expect("inputs json"),
        grants: serde_json::from_value(row.try_get("grants").expect("grants"))
            .expect("grants json"),
        inputs_digest: row
            .try_get::<String, _>("inputs_digest")
            .expect("inputs_digest")
            .parse::<Digest>()
            .expect("digest"),
        computed_at: row
            .try_get::<DateTime<Utc>, _>("computed_at")
            .expect("computed_at"),
        expires_at: row
            .try_get::<Option<DateTime<Utc>>, _>("expires_at")
            .expect("expires_at"),
    }
}

#[tokio::test]
async fn stored_projection_round_trips_and_event_carries_the_inputs_digest() {
    if admin_url().is_none() {
        blocked_marker();
        return;
    }
    let name = scratch_name("persist");
    let Some(pool) = common::fresh_migrated_database(&name).await else {
        blocked_marker();
        return;
    };
    seed_tenant(&pool, TENANT, WORKSPACE).await;

    let now = at("2026-06-01T00:00:00Z");
    let source = source_with_platform(
        vec![grant_with(
            "read.internal",
            fs("/root/**"),
            constraints(None, Approval::Always, None, None),
        )],
        &[("read.internal", 0)],
    )
    .with(Layer::ActiveSkills, LayerInput::empty("skill/none"));
    let assembly = assemble(&source, &ProjectionRequest::at(subject(), now)).expect("projection");
    let projection = assembly.projection.clone();
    let row = ProjectionRow::from_projection(&projection, TENANT, Some(WORKSPACE.to_string()));

    let store = EventStore::new(pool.clone());
    let row_to_write = row.clone();
    let mut generator = UlidGenerator::new();
    let correlation_id = CorrelationId::generate(&mut generator);
    let actor = Actor::system("capability_persistence_test");
    store
        .commit_mutation_tx(TENANT, move |tx, batch| {
            Box::pin(async move {
                let mut query = sqlx::query(ProjectionRow::INSERT_SQL);
                for argument in row_to_write.arguments() {
                    query = match argument {
                        ProjectionArgument::Text(value) => query.bind(value),
                        ProjectionArgument::OptionalText(value) => query.bind(value),
                        ProjectionArgument::Json(value) => query.bind(value),
                        ProjectionArgument::Timestamptz(value) => query.bind(value),
                        ProjectionArgument::OptionalTimestamptz(value) => query.bind(value),
                    };
                }
                query.execute(&mut **tx).await?;
                stage_projected_event(batch, &row_to_write, correlation_id, actor);
                Ok(())
            })
        })
        .await
        .expect("commit projection row and event");

    let restored = read_row(&pool, &row.id).await;
    assert_eq!(
        restored, row,
        "the stored projection must round-trip with its input digests"
    );
    let rebuilt = restored
        .projection()
        .expect("row reconstructs a projection");
    assert_eq!(rebuilt, projection);
    assert_eq!(rebuilt.inputs_digest, projection.inputs_digest);

    let events = store
        .read_events_after(TENANT, None, 50)
        .await
        .expect("read events");
    let projected: Vec<_> = events
        .iter()
        .filter(|event| event.event_type.to_string() == "capability.projected")
        .collect();
    assert_eq!(projected.len(), 1, "exactly one capability.projected event");
    let payload = &projected[0].payload;
    assert_eq!(
        payload["inputs_digest"],
        projection.inputs_digest.to_string()
    );
    assert_eq!(payload["projection_id"], row.id);
    assert_eq!(
        payload["inputs"],
        serde_json::to_value(&row.inputs).expect("inputs json")
    );

    drop_pool(&pool, &name).await;
}
