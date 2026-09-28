//! The served background pass composes the authorities QA-002 found built but never run
//! (CORE-009 client projections, RUN-004 orchestration, CORE-003 outbox publishing).
//!
//! Every test drives the deployable's own `composition::Background` over a pool that acts as
//! `quansio_app`, the least-privilege role a deployment serves as, so row-level security
//! applies exactly as in production.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (the dev stack). Absent → `BLOCKED_EXTERNAL`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::Request;
use quansio_core::UlidGenerator;
use quansio_server::api::ApiState;
use quansio_server::composition::Background;
use quansio_server::control::schema;
use quansio_server::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, RuntimeError,
};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0BGCMP";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0BGCMP";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0BGCMP";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0BGCMP";

/// A model that answers every turn.
#[derive(Default)]
struct AnsweringModel {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ModelProposalSource for AnsweringModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelProposal {
            assistant_text: Some("noted".to_string()),
            ..ModelProposal::default()
        })
    }
}

struct Harness {
    name: String,
    /// The schema owner, for assertions only.
    admin: sqlx::PgPool,
    /// What the deployable serves over.
    app: sqlx::PgPool,
    api: ApiState,
}

/// A pool whose every connection acts as `quansio_app`.
async fn app_role_pool(url: &str) -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET ROLE quansio_app").execute(conn).await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .expect("app-role pool")
}

async fn prepare(prefix: &str) -> Option<Harness> {
    let name = common::scratch_name(prefix);
    let admin = common::fresh_database(&name).await?;
    schema::migrate(&admin).await.expect("migrate");
    common::seed_tenant(&admin, TENANT, USER, WORKSPACE, NODE).await;
    let app = app_role_pool(&common::scratch_url(&name)).await;
    let api = ApiState::new(app.clone())
        .expect("api")
        .with_model_source(Arc::new(AnsweringModel::default()));
    Some(Harness {
        name,
        admin,
        app,
        api,
    })
}

async fn finish(harness: Harness) {
    harness.app.close().await;
    common::drop_pool(&harness.admin, &harness.name).await;
}

async fn command(api: &ApiState, name: &str, params: Value) -> Value {
    let router = quansio_server::api::router(api.clone());
    let request = Request::builder()
        .method("POST")
        .uri(format!("/v1/commands/{name}"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "command_id": format!("cmd_{}", UlidGenerator::new().generate().to_base32()),
                "params": params,
            })
            .to_string(),
        ))
        .expect("request");
    let response = router.oneshot(request).await.expect("response");
    assert!(
        response.status().is_success(),
        "{name}: {}",
        response.status()
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

async fn background_pass(harness: &Harness) {
    Background::new(harness.app.clone(), harness.api.clone())
        .pass()
        .await
        .expect("background pass");
}

/// CORE-009 composed: the served background pass keeps the run-status and work-node-status
/// projections up with the tenant's events, as the least-privilege role, and a pass over an
/// unchanged stream changes nothing.
#[tokio::test]
async fn the_background_pass_keeps_the_client_projections_current() {
    let Some(harness) = prepare("bg_projections").await else {
        common::blocked_marker();
        return;
    };
    let posted = command(
        &harness.api,
        "PostMessage",
        json!({ "workspace_id": WORKSPACE, "content": "hello" }),
    )
    .await;
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let objective = command(
        &harness.api,
        "CreateObjective",
        json!({ "workspace_id": WORKSPACE, "title": "Ship the report" }),
    )
    .await;

    background_pass(&harness).await;

    let (status,): (String,) = sqlx::query_as("SELECT status FROM runs WHERE id = $1")
        .bind(&run_id)
        .fetch_one(&harness.admin)
        .await
        .expect("run");
    let projected: Option<(String, i64)> = sqlx::query_as(
        "SELECT status, last_sequence FROM run_status_projection WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(TENANT)
    .bind(&run_id)
    .fetch_optional(&harness.admin)
    .await
    .expect("projection");
    let (projected_status, _) = projected.expect("the run is projected");
    assert_eq!(projected_status, status, "the read model follows the run");
    let nodes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM work_node_status_projection WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_one(&harness.admin)
            .await
            .expect("nodes");
    assert!(nodes >= 1, "work-node status is projected");
    // The objective's node, emitted by the graph transaction on the `work_node` aggregate, is
    // projected with its canonical status.
    let node_id: String = sqlx::query_scalar(
        "SELECT id FROM work_nodes WHERE tenant_id = $1 AND title = 'Ship the report'",
    )
    .bind(TENANT)
    .fetch_one(&harness.admin)
    .await
    .unwrap_or_else(|error| panic!("the objective's node ({objective}): {error}"));
    let (canonical,): (String,) = sqlx::query_as("SELECT status FROM work_nodes WHERE id = $1")
        .bind(&node_id)
        .fetch_one(&harness.admin)
        .await
        .expect("node");
    let projected_node: Option<(String, String)> = sqlx::query_as(
        "SELECT status, title FROM work_node_status_projection WHERE tenant_id = $1 AND work_node_id = $2",
    )
    .bind(TENANT)
    .bind(&node_id)
    .fetch_optional(&harness.admin)
    .await
    .expect("node projection");
    assert_eq!(
        projected_node,
        Some((canonical, "Ship the report".to_string())),
        "the objective's node is projected"
    );
    let checkpoint = |stream: &'static str| {
        let admin = harness.admin.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT sequence FROM event_cursors WHERE tenant_id = $1 AND stream_id = $2",
            )
            .bind(TENANT)
            .bind(stream)
            .fetch_optional(&admin)
            .await
            .expect("cursor")
        }
    };
    let head: i64 =
        sqlx::query_scalar("SELECT max(sequence) FROM runtime_events WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_one(&harness.admin)
            .await
            .expect("head");
    let run_cursor = checkpoint("projection:run_status").await;
    assert_eq!(
        run_cursor,
        Some(head),
        "the checkpoint reached the stream head"
    );

    // An idle pass is a no-op: same rows, same checkpoint.
    let before: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT run_id, status, event_count FROM run_status_projection WHERE tenant_id = $1 ORDER BY run_id",
    )
    .bind(TENANT)
    .fetch_all(&harness.admin)
    .await
    .expect("rows");
    background_pass(&harness).await;
    let after: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT run_id, status, event_count FROM run_status_projection WHERE tenant_id = $1 ORDER BY run_id",
    )
    .bind(TENANT)
    .fetch_all(&harness.admin)
    .await
    .expect("rows");
    assert_eq!(before, after);
    assert_eq!(checkpoint("projection:run_status").await, run_cursor);
    finish(harness).await;
}

/// Apply graph changes the way production does: one `GraphTransaction`, which writes the
/// rows and their `work.*` events together.
async fn transact(harness: &Harness, changes: Vec<quansio_graph::GraphChange>) {
    let store = quansio_graph::GraphStore::new(harness.admin.clone(), TENANT).expect("graph");
    let revision = store.graph_revision(WORKSPACE).await.expect("revision");
    let mut batch = quansio_graph::GraphBatch::new(WORKSPACE, revision);
    for change in changes {
        batch.push(change);
    }
    quansio_graph::transaction::GraphTransaction::new(
        store,
        quansio_events::EventStore::new(harness.admin.clone()),
        quansio_graph::transaction::TransactionContext::system(
            "projection-test",
            quansio_core::CorrelationId::generate(&mut UlidGenerator::new()),
        ),
    )
    .apply(batch)
    .await
    .expect("graph transaction");
}

/// A node's later status changes reach the work-node projection: CORE-005 states a
/// transition as `from`/`to` on `work.node_status_changed`, and the projection follows `to`.
#[tokio::test]
async fn the_work_node_projection_follows_status_changes() {
    let Some(harness) = prepare("bg_projection_status").await else {
        common::blocked_marker();
        return;
    };
    transact(
        &harness,
        vec![quansio_graph::GraphChange::create_work_node(
            quansio_graph::NewWorkNode::new(
                WORKSPACE,
                quansio_graph::WorkNodeKind::Task,
                "draft a plan",
                json!({ "kind": "system", "id": "projection-test" }),
            ),
        )],
    )
    .await;
    let store = quansio_graph::GraphStore::new(harness.admin.clone(), TENANT).expect("graph");
    let id: String = sqlx::query_scalar("SELECT id FROM work_nodes WHERE title = 'draft a plan'")
        .fetch_one(&harness.admin)
        .await
        .expect("node id");
    let id = quansio_core::CanonicalId::parse_typed(&id, quansio_core::Prefix::WorkNode)
        .expect("canonical");
    for to in [
        quansio_graph::WorkNodeStatus::Ready,
        quansio_graph::WorkNodeStatus::Cancelled,
    ] {
        let node = store.get_node(&id).await.expect("node");
        transact(
            &harness,
            vec![quansio_graph::GraphChange::TransitionWorkNode {
                node_id: id,
                expected: node.revision,
                to,
            }],
        )
        .await;
    }
    background_pass(&harness).await;
    let projected: (String, String, i64) = sqlx::query_as(
        "SELECT status, last_event_type, event_count FROM work_node_status_projection \
         WHERE tenant_id = $1 AND work_node_id = $2",
    )
    .bind(TENANT)
    .bind(id.to_string())
    .fetch_one(&harness.admin)
    .await
    .expect("projected node");
    assert_eq!(
        projected,
        (
            "cancelled".to_string(),
            "work.node_status_changed".to_string(),
            3
        )
    );
    finish(harness).await;
}
