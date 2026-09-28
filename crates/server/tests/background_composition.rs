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

// ---------------------------------------------------------------------------------------
// RUN-004: the orchestrator the served background pass runs
// ---------------------------------------------------------------------------------------

use quansio_core::{CanonicalId, CorrelationId, Prefix};
use quansio_graph::{
    GraphStore, NewWorkEdge, NewWorkNode, WorkEdgeKind, WorkNodeKind, WorkNodeStatus,
};
use quansio_server::runtime::state_machine::{
    NewRun, RunTriggerKind, RuntimeEngine, RuntimeIdentity,
};

fn graph(harness: &Harness) -> GraphStore {
    GraphStore::new(harness.admin.clone(), TENANT).expect("graph store")
}

async fn node(store: &GraphStore, title: &str, parent: Option<CanonicalId>) -> CanonicalId {
    let mut new = NewWorkNode::new(
        WORKSPACE,
        WorkNodeKind::Task,
        title,
        json!({ "kind": "system", "id": "orchestration-test" }),
    );
    new.parent_id = parent;
    store.create_node(new).await.expect("node").id
}

async fn depends_on(store: &GraphStore, dependent: CanonicalId, prerequisite: CanonicalId) {
    store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            dependent,
            prerequisite,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect("edge");
}

async fn agent_thread(harness: &Harness) -> CanonicalId {
    let id = CanonicalId::generate(Prefix::AgentThread, &mut UlidGenerator::new());
    let mut tx = harness.admin.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'worker', 1, 'ACTIVE')",
    )
    .bind(id.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    tx.commit().await.expect("commit");
    id
}

fn engine(harness: &Harness) -> RuntimeEngine {
    RuntimeEngine::new(
        harness.admin.clone(),
        RuntimeIdentity::system(
            TENANT,
            "orchestration-test",
            CorrelationId::generate(&mut UlidGenerator::new()),
        ),
    )
    .expect("engine")
}

/// A queued run for `node`, as delegation or a plan would leave it for orchestration.
async fn queued_run(harness: &Harness, node: CanonicalId, thread: CanonicalId) -> String {
    let engine = engine(harness);
    let run = engine
        .create_run(NewRun::new(WORKSPACE, node, thread, RunTriggerKind::Manual))
        .await
        .expect("run");
    engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue")
        .id
        .to_string()
}

async fn run_status(harness: &Harness, run_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&harness.admin)
        .await
        .expect("run")
}

async fn node_status(harness: &Harness, node: &CanonicalId) -> String {
    sqlx::query_scalar("SELECT status FROM work_nodes WHERE id = $1")
        .bind(node.to_string())
        .fetch_one(&harness.admin)
        .await
        .expect("node")
}

/// Advance a node to `done` through the only legal path.
async fn complete(store: &GraphStore, node_id: &CanonicalId) {
    let mut node = store.get_node(node_id).await.expect("node");
    if node.status == WorkNodeStatus::Draft {
        node = store
            .transition_node(node_id, node.revision, WorkNodeStatus::Ready)
            .await
            .expect("ready");
    }
    if node.status == WorkNodeStatus::Ready {
        node = store
            .transition_node(node_id, node.revision, WorkNodeStatus::InProgress)
            .await
            .expect("in progress");
    }
    let node = store
        .transition_node(node_id, node.revision, WorkNodeStatus::Verifying)
        .await
        .expect("verifying");
    store
        .mark_verification_passed(node_id, node.revision)
        .await
        .expect("done");
}

/// RUN-004 composed: the served background pass releases graph work in dependency order —
/// a dependent's queued run starts only once its prerequisite is `done`, and neither the
/// orchestrator nor the scheduler beside it starts it earlier.
#[tokio::test]
async fn the_background_pass_starts_dependent_work_only_after_its_prerequisite() {
    let Some(harness) = prepare("bg_orchestration_dag").await else {
        common::blocked_marker();
        return;
    };
    let store = graph(&harness);
    let thread = agent_thread(&harness).await;
    let first = node(&store, "gather", None).await;
    let second = node(&store, "summarise", None).await;
    depends_on(&store, second, first).await;
    let first_run = queued_run(&harness, first, thread).await;
    let second_run = queued_run(&harness, second, thread).await;

    background_pass(&harness).await;
    assert_eq!(node_status(&harness, &first).await, "ready", "released");
    assert_eq!(
        run_status(&harness, &first_run).await,
        "RUNNING",
        "dispatched"
    );
    assert_eq!(node_status(&harness, &second).await, "draft");
    assert_eq!(
        run_status(&harness, &second_run).await,
        "QUEUED",
        "no dependent work starts before its prerequisite"
    );

    // Another pass changes nothing while the prerequisite is unfinished.
    background_pass(&harness).await;
    assert_eq!(run_status(&harness, &second_run).await, "QUEUED");

    complete(&store, &first).await;
    background_pass(&harness).await;
    assert_eq!(node_status(&harness, &second).await, "ready");
    assert_eq!(run_status(&harness, &second_run).await, "RUNNING");
    finish(harness).await;
}

/// The workspace's `concurrency` budget bounds how much ready work the pass starts.
#[tokio::test]
async fn the_background_pass_admits_ready_work_within_the_concurrency_budget() {
    let Some(harness) = prepare("bg_orchestration_capacity").await else {
        common::blocked_marker();
        return;
    };
    command(
        &harness.api,
        "SetBudget",
        json!({ "scope": "workspace", "workspace_id": WORKSPACE, "limits": { "concurrency": 1 } }),
    )
    .await;
    let store = graph(&harness);
    let thread = agent_thread(&harness).await;
    let mut runs = Vec::new();
    for title in ["one", "two", "three"] {
        let id = node(&store, title, None).await;
        runs.push(queued_run(&harness, id, thread).await);
    }
    background_pass(&harness).await;
    let mut running = 0;
    for run in &runs {
        if run_status(&harness, run).await == "RUNNING" {
            running += 1;
        }
    }
    assert_eq!(
        running, 1,
        "one slot, one running run; the rest stay queued"
    );
    background_pass(&harness).await;
    let mut still_running = 0;
    for run in &runs {
        if run_status(&harness, run).await == "RUNNING" {
            still_running += 1;
        }
    }
    assert_eq!(still_running, 1, "the bound holds across passes");
    finish(harness).await;
}

/// A node cancelled by any writer reaches its subtree: the child's running run is cancelled
/// once, the child node is cancelled, and a dependent is blocked; a later pass repeats none
/// of it.
#[tokio::test]
async fn the_background_pass_carries_a_cancellation_to_its_subtree_once() {
    let Some(harness) = prepare("bg_orchestration_cancel").await else {
        common::blocked_marker();
        return;
    };
    let store = graph(&harness);
    let thread = agent_thread(&harness).await;
    let parent = node(&store, "objective", None).await;
    let child = node(&store, "step", Some(parent)).await;
    let dependent = node(&store, "follow-up", None).await;
    depends_on(&store, dependent, child).await;
    let child_run = queued_run(&harness, child, thread).await;
    background_pass(&harness).await;
    assert_eq!(run_status(&harness, &child_run).await, "RUNNING");

    // Another authority cancels the parent node.
    let current = store.get_node(&parent).await.expect("parent");
    store
        .transition_node(&parent, current.revision, WorkNodeStatus::Cancelled)
        .await
        .expect("cancel the parent");
    background_pass(&harness).await;
    assert_eq!(run_status(&harness, &child_run).await, "CANCELLED");
    assert_eq!(node_status(&harness, &child).await, "cancelled");
    assert_eq!(node_status(&harness, &dependent).await, "blocked");

    let cancellations = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = 'run.cancelled'",
        )
        .bind(TENANT)
        .fetch_one(&harness.admin)
        .await
        .expect("events")
    };
    assert_eq!(cancellations().await, 1);
    background_pass(&harness).await;
    assert_eq!(cancellations().await, 1, "propagation is idempotent");
    finish(harness).await;
}
