//! The graph-backed planning port: compiled plans commit through `GraphTransaction`
//! (RUN-003, DOMAIN.md §4.5).
//!
//! The server-side planning module compiles a model proposal into a `CompiledPlan` and
//! hands it to the graph-backed `GraphPlanWorkspace` (`runtime::planning::graph_port`), which
//! applies it with `GraphTransaction::apply_plan_with` using RUN-005's capability algebra as
//! the final narrowing guard. This proves a compiled plan is committed as one
//! revision-checked, event-emitting transaction, and that a replay conflicts instead of
//! duplicating work.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

mod common;

/// Fixture identities (the graph suite's originals, kept so the assertions read the same).
const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const SEED_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

/// Migrate a fresh scratch database and seed the tenant.
async fn prepare(prefix: &str) -> Option<(String, sqlx::PgPool)> {
    let name = common::scratch_name(&format!("graph_{prefix}"));
    let pool = common::fresh_database(&name).await?;
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, SEED_NODE).await;
    Some((name, pool))
}

use common::{blocked_marker, drop_pool};

use std::sync::Arc;

use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::EventStore;
use quansio_graph::GraphStore;
use quansio_server::runtime::planning::{
    GraphPlanWorkspace, PlanError, PlanProposerProjection, PlanWorkspacePort, Planner,
};
use quansio_server::runtime::state_machine::RuntimeIdentity;
use serde_json::{json, Value};
use sqlx::PgPool;

/// The agent thread that proposes plans in these fixtures.
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

/// The proposer's capability projection: one filesystem write grant.
fn grants() -> Value {
    json!([{
        "effect_class": "fs.write.workspace",
        "resource": {"kind": "fs", "selector": "/work/**"},
        "constraints": {"max_tier": 1}
    }])
}

fn contract() -> Value {
    json!({
        "deterministic_checks": [{"kind": "artifact_exists", "artifact_role": "report"}],
        "semantic_verification": {"required": false},
        "human_signoff_required": false
    })
}

fn proposer_projection() -> PlanProposerProjection {
    PlanProposerProjection {
        agent_thread_id: Some(AGENT.to_string()),
        grants: grants(),
        created_by: json!({"kind": "agent", "id": AGENT}),
    }
}

fn correlation() -> CorrelationId {
    CorrelationId::generate(&mut UlidGenerator::new())
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity::agent(TENANT, AGENT, correlation())
}

/// A raw plan proposal that adds one task with its own CompletionContract.
fn raw_plan(base_revision: u64) -> Value {
    json!({
        "proposal_id": "prop_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        "run_id": "run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        "workspace_id": WORKSPACE,
        "base_revision": base_revision,
        "nodes_add": [{
            "id": "planned",
            "kind": "task",
            "title": "planned task",
            "status": "draft",
            "completion_contract": contract(),
            "capability_needs": [],
            "priority": 1
        }],
        "nodes_update": [],
        "edges_add": [],
        "edges_remove": [],
        "rationale": "add one planned task",
        "capability_needs": []
    })
}

/// A planner over the graph-backed port for a scratch database.
struct Fixture {
    name: String,
    pool: PgPool,
    events: EventStore,
    planner: Planner,
}

async fn setup(prefix: &str) -> Option<Fixture> {
    let (name, pool) = prepare(prefix).await?;
    let port: Arc<dyn PlanWorkspacePort> =
        Arc::new(GraphPlanWorkspace::new(pool.clone(), identity()));
    let planner = Planner::new(pool.clone(), port, identity()).expect("planner");
    Some(Fixture {
        name,
        pool: pool.clone(),
        events: EventStore::new(pool),
        planner,
    })
}

async fn event_types(events: &EventStore) -> Vec<String> {
    event_shape(events)
        .await
        .into_iter()
        .map(|(_, event_type)| event_type)
        .collect()
}

/// `(aggregate_type, event_type)` in tenant sequence order.
///
/// Generated `wn_…`/`we_…` aggregate ids are ULIDs (CORE-002), so determinism is asserted
/// on the aggregate *kind* and event type sequence; the plan events' aggregate id is the
/// workspace, which the accepted-plan test pins exactly.
async fn event_shape(events: &EventStore) -> Vec<(String, String)> {
    events
        .read_events_after(TENANT, None, 1000)
        .await
        .expect("read events")
        .into_iter()
        .map(|event| (event.aggregate_type, event.event_type.to_string()))
        .collect()
}

/// `(title, kind, parent_id)` for every node, sorted so generated ids do not matter.
async fn node_shape(pool: &PgPool) -> Vec<(String, String, Option<String>)> {
    let store = GraphStore::new(pool.clone(), TENANT).expect("store");
    let mut shape: Vec<(String, String, Option<String>)> = store
        .list_nodes(WORKSPACE)
        .await
        .expect("nodes")
        .into_iter()
        .map(|node| {
            (
                node.title,
                node.kind.as_db_str().to_string(),
                node.parent_id.map(|id| id.to_string()),
            )
        })
        .collect();
    shape.sort();
    shape
}

#[tokio::test]
async fn a_compiled_plan_commits_through_graph_transaction_with_one_revision_bump() {
    let Some(fixture) = setup("planning_apply").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(fixture.pool.clone(), TENANT).expect("store");
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let outcome = fixture
        .planner
        .propose(&raw_plan(base.get()), proposer_projection())
        .await
        .expect("the compiled plan applies");
    assert_eq!(outcome.revision, base.get() + 1);
    assert_eq!(outcome.nodes_added, 1);

    let types = event_types(&fixture.events).await;
    assert_eq!(
        types,
        vec![
            "work.plan_proposed",
            "work.node_created", // the planned node
            "work.plan_applied"
        ]
    );
    let events = fixture
        .events
        .read_events_after(TENANT, None, 100)
        .await
        .expect("events");
    let applied = events.last().expect("plan applied");
    assert_eq!(applied.aggregate_type, "work_graph");
    assert_eq!(applied.aggregate_id, WORKSPACE);
    assert_eq!(applied.aggregate_version, base.get() + 1);

    let shape = node_shape(&fixture.pool).await;
    assert_eq!(shape.len(), 2, "the seed node plus the planned task");
    assert_eq!(
        shape
            .iter()
            .filter(|(title, _, _)| title == "planned task")
            .count(),
        1,
        "the plan's node exists exactly once"
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn replaying_a_compiled_plan_conflicts_instead_of_duplicating_nodes() {
    let Some(fixture) = setup("planning_replay").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(fixture.pool.clone(), TENANT).expect("store");
    let base = store.graph_revision(WORKSPACE).await.expect("head");
    let plan = raw_plan(base.get());

    let first = fixture
        .planner
        .propose(&plan, proposer_projection())
        .await
        .expect("first application");
    assert_eq!(first.revision, base.get() + 1);

    let error = fixture
        .planner
        .propose(&plan, proposer_projection())
        .await
        .expect_err("a replayed plan is stale");
    assert_eq!(error.code(), "CONFLICT_REVISION");
    assert!(matches!(error, PlanError::StaleBaseRevision { .. }));

    let after = node_shape(&fixture.pool).await;
    assert_eq!(after.len(), 2, "the plan's node exists exactly once");
    assert_eq!(
        store.graph_revision(WORKSPACE).await.expect("head").get(),
        base.get() + 1,
        "the rejected replay did not bump the revision"
    );
    assert_eq!(
        event_types(&fixture.events)
            .await
            .last()
            .map(String::as_str),
        Some("work.plan_rejected")
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn the_same_plan_over_the_same_state_is_deterministic_across_runs_and_restart() {
    let plan_a = {
        let Some(fixture) = setup("planning_det_a").await else {
            blocked_marker();
            return;
        };
        let store = GraphStore::new(fixture.pool.clone(), TENANT).expect("store");
        let base = store.graph_revision(WORKSPACE).await.expect("head");
        let outcome = fixture
            .planner
            .propose(&raw_plan(base.get()), proposer_projection())
            .await
            .expect("apply");
        let snapshot = (
            outcome.revision,
            event_shape(&fixture.events).await,
            node_shape(&fixture.pool).await,
        );

        // A simulated restart: fresh store and event reader over the same durable state
        // see the same graph and the same event sequence.
        let restarted_store = GraphStore::new(fixture.pool.clone(), TENANT).expect("store");
        let restarted_events = EventStore::new(fixture.pool.clone());
        assert_eq!(
            restarted_store
                .graph_revision(WORKSPACE)
                .await
                .expect("head")
                .get(),
            outcome.revision
        );
        assert_eq!(event_shape(&restarted_events).await, snapshot.1);
        assert_eq!(node_shape(&fixture.pool).await, snapshot.2);

        drop_pool(&fixture.pool, &fixture.name).await;
        snapshot
    };

    let Some(fixture_b) = setup("planning_det_b").await else {
        blocked_marker();
        return;
    };
    let store_b = GraphStore::new(fixture_b.pool.clone(), TENANT).expect("store");
    let base_b = store_b.graph_revision(WORKSPACE).await.expect("head");
    let outcome_b = fixture_b
        .planner
        .propose(&raw_plan(base_b.get()), proposer_projection())
        .await
        .expect("apply");
    let snapshot_b = (
        outcome_b.revision,
        event_shape(&fixture_b.events).await,
        node_shape(&fixture_b.pool).await,
    );

    assert_eq!(plan_a.0, snapshot_b.0, "same resulting revision");
    assert_eq!(
        plan_a.1, snapshot_b.1,
        "same aggregate kind and event type sequence"
    );
    assert_eq!(plan_a.2, snapshot_b.2, "same node shape");

    drop_pool(&fixture_b.pool, &fixture_b.name).await;
}

#[tokio::test]
async fn the_seed_node_id_is_visible_to_the_compiler_snapshot() {
    let Some(fixture) = setup("planning_snapshot").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(fixture.pool.clone(), TENANT).expect("store");
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    // A node whose parent is the seed objective: the seed carries no contract, so the
    // plan is rejected with `VALIDATION_SCHEMA` before the graph is touched.
    let mut raw = raw_plan(base.get());
    raw["nodes_add"] = json!([{
        "id": "child",
        "kind": "subtask",
        "title": "child of seed",
        "status": "draft",
        "parent_id": SEED_NODE,
        "completion_contract": {},
        "capability_needs": []
    }]);
    raw["proposal_id"] = json!("prop_01J8Z3K6F1N8VQ2X5W9Y0DDDDD");
    let error = fixture
        .planner
        .propose(&raw, proposer_projection())
        .await
        .expect_err("the seed provides no contract to inherit");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert_eq!(
        store.graph_revision(WORKSPACE).await.expect("head"),
        base,
        "a rejected plan writes nothing"
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}
