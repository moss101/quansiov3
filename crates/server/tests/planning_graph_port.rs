//! RUN-003 coverage: the graph-backed plan port applies node transitions, every edge kind and
//! edge removals as one `GraphTransaction`, attributes them to a user, and is itself the final
//! guard for a compiled plan that reaches it directly (DOMAIN.md §4.1, §4.2, §4.5).
//!
//! Everything is real: the planner, the graph store, the graph transaction and the event log.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`;
//! absent, every test prints the `BLOCKED_EXTERNAL` marker and returns.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use quansio_core::{CausationId, CommandId, CorrelationId, Revision, UlidGenerator};
use quansio_events::{Actor, ActorKind, EventStore};
use quansio_graph::{GraphStore, WorkNodeStatus};
use quansio_server::control::schema;
use quansio_server::runtime::planning::{
    CompiledEdge, CompiledEdgeRemoval, CompiledNode, CompiledNodeUpdate, CompiledPlan,
    ContractSource, EdgeKind, GraphPlanWorkspace, NodeKind, NodeStatus, PlanCommitOutcome,
    PlanError, PlanGraphSnapshot, PlanProposerProjection, PlanWorkspacePort, Planner,
};
use quansio_server::runtime::state_machine::RuntimeIdentity;
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA0G1";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA0G1";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0G1";
const SEED_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0G1";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0G2";
const OTHER_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0G2";
const COMMAND: &str = "cmd_01J8Z3K6F1N8VQ2X5W9Y0QA0G1";

struct Fixture {
    name: String,
    pool: PgPool,
    port: Arc<GraphPlanWorkspace>,
    planner: Planner,
}

/// A user acting through a client command, as the WorkGraph surface would.
fn user_identity() -> RuntimeIdentity {
    let command = CommandId::from_str(COMMAND).expect("command id");
    RuntimeIdentity {
        tenant_id: TENANT.to_string(),
        actor: Actor::user(USER),
        correlation_id: CorrelationId::generate(&mut UlidGenerator::new()),
        causation_id: Some(CausationId::Command(command)),
        command_id: Some(command),
    }
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, SEED_NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'other')")
        .bind(OTHER_WORKSPACE)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("other workspace");
    sqlx::query(
        "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) \
         VALUES ($1, $2, $3, 'objective', 'other node', $4)",
    )
    .bind(OTHER_NODE)
    .bind(TENANT)
    .bind(OTHER_WORKSPACE)
    .bind(json!({ "kind": "user", "id": USER }))
    .execute(&mut *tx)
    .await
    .expect("other node");
    tx.commit().await.expect("commit");

    let port = Arc::new(GraphPlanWorkspace::new(pool.clone(), user_identity()));
    let planner = Planner::new(
        pool.clone(),
        Arc::clone(&port) as Arc<dyn PlanWorkspacePort>,
        user_identity(),
    )
    .expect("planner");
    Some(Fixture {
        name,
        pool,
        port,
        planner,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

fn contract() -> Value {
    json!({ "deterministic_checks": [{ "kind": "artifact_exists", "artifact_role": "report" }] })
}

fn grants() -> Value {
    json!([{
        "effect_class": "fs.write.workspace",
        "resource": { "kind": "fs", "selector": "/work/**" },
        "constraints": { "max_tier": 1 }
    }])
}

fn proposer() -> PlanProposerProjection {
    PlanProposerProjection {
        agent_thread_id: None,
        grants: grants(),
        created_by: json!({ "kind": "user", "id": USER }),
    }
}

async fn revision(fixture: &Fixture) -> u64 {
    fixture
        .port
        .snapshot(WORKSPACE)
        .await
        .expect("snapshot")
        .revision
        .get()
}

fn raw(base_revision: u64, changes: Value) -> Value {
    let mut raw = json!({
        "proposal_id": format!("prop_{base_revision}"),
        "workspace_id": WORKSPACE,
        "base_revision": base_revision,
        "rationale": "user edit",
        "capability_needs": []
    });
    for (key, value) in changes.as_object().expect("changes").clone() {
        raw[key] = value;
    }
    raw
}

/// The id of the node with `title`.
async fn node_id(fixture: &Fixture, title: &str) -> String {
    GraphStore::new(fixture.pool.clone(), TENANT)
        .expect("store")
        .list_nodes(WORKSPACE)
        .await
        .expect("nodes")
        .into_iter()
        .find(|node| node.title == title)
        .map(|node| node.id.to_string())
        .expect("node")
}

/// A compiled plan with no changes at the current revision, to be filled per case.
fn plan(base: u64) -> CompiledPlan {
    CompiledPlan {
        proposal_id: format!("prop_direct_{base}"),
        run_id: None,
        workspace_id: WORKSPACE.to_string(),
        base_revision: Revision::new(base),
        rationale: "direct".to_string(),
        nodes_add: Vec::new(),
        nodes_update: Vec::new(),
        edges_add: Vec::new(),
        edges_remove: Vec::new(),
        capability_needs: json!([]),
        proposer_agent_thread_id: None,
        proposer_grants: grants(),
        created_by: json!({ "kind": "user", "id": USER }),
    }
}

fn compiled_node(title: &str, parent: Option<&str>, completion_contract: Value) -> CompiledNode {
    CompiledNode {
        draft_id: title.to_string(),
        kind: NodeKind::Task,
        title: title.to_string(),
        description: None,
        parent_id: parent.map(str::to_string),
        status: NodeStatus::Draft,
        completion_contract,
        contract_source: ContractSource::Own,
        capability_needs: json!([]),
        priority: 1,
        budget_id: None,
        thread_id: None,
    }
}

#[tokio::test]
async fn a_user_edit_transitions_nodes_and_adds_and_removes_every_edge_kind_in_one_transaction() {
    let Some(fixture) = prepare("qa001_gp_edit").await else {
        blocked_marker();
        return;
    };
    let base = revision(&fixture).await;
    let task = |title: &str| {
        json!({ "id": title, "kind": "task", "title": title, "parent_id": SEED_NODE,
                "completion_contract": contract() })
    };
    fixture
        .planner
        .propose(
            &raw(
                base,
                json!({ "nodes_add": [task("draft report"), task("review")] }),
            ),
            proposer(),
        )
        .await
        .expect("two tasks");
    let report = node_id(&fixture, "draft report").await;
    let review = node_id(&fixture, "review").await;

    let snapshot = fixture.port.snapshot(WORKSPACE).await.expect("snapshot");
    let report_revision = snapshot
        .nodes
        .iter()
        .find(|node| node.id == report)
        .expect("report")
        .revision;
    let edge = |id: &str, from: &str, to: &str, kind: &str| json!({ "id": id, "from": from, "to": to, "kind": kind });
    let outcome = fixture
        .planner
        .propose(
            &raw(
                snapshot.revision.get(),
                json!({
                    "nodes_update": [{ "node_id": report, "expected_revision": report_revision, "to": "ready" }],
                    "edges_add": [
                        edge("e1", &report, &review, "produces_artifact"),
                        edge("e2", &report, &review, "verified_by"),
                        edge("e3", &review, SEED_NODE, "blocked_by"),
                        edge("e4", &report, &review, "parent_of")
                    ]
                }),
            ),
            proposer(),
        )
        .await
        .expect("the edit applies");
    assert_eq!(outcome.nodes_updated, 1);
    assert_eq!(outcome.edges_added, 4);
    assert_eq!(
        outcome.revision,
        snapshot.revision.get() + 1,
        "one revision bump"
    );

    let store = GraphStore::new(fixture.pool.clone(), TENANT).expect("store");
    let transitioned = store
        .list_nodes(WORKSPACE)
        .await
        .expect("nodes")
        .into_iter()
        .find(|node| node.id.to_string() == report)
        .expect("report");
    assert_eq!(transitioned.status, WorkNodeStatus::Ready);

    let after = fixture.port.snapshot(WORKSPACE).await.expect("snapshot");
    let mut kinds: Vec<EdgeKind> = after.edges.iter().map(|edge| edge.kind).collect();
    kinds.sort_by_key(|kind| kind.as_str());
    assert_eq!(
        kinds,
        vec![
            EdgeKind::BlockedBy,
            EdgeKind::ParentOf,
            EdgeKind::ProducesArtifact,
            EdgeKind::VerifiedBy
        ],
        "every stored edge kind reads back as itself"
    );

    // Removing one edge at its observed revision leaves the others.
    let verified = after
        .edges
        .iter()
        .find(|edge| edge.kind == EdgeKind::VerifiedBy)
        .expect("verified_by edge");
    let removed = fixture
        .planner
        .propose(
            &raw(
                after.revision.get(),
                json!({ "edges_remove": [{ "edge_id": verified.id, "expected_revision": verified.revision }] }),
            ),
            proposer(),
        )
        .await
        .expect("the removal applies");
    assert_eq!(removed.edges_removed, 1);
    let last = fixture.port.snapshot(WORKSPACE).await.expect("snapshot");
    assert_eq!(last.edges.len(), 3);
    assert!(last.edges.iter().all(|edge| edge.id != verified.id));

    // The edit is attributed to the user and to the command that carried it.
    let events = EventStore::new(fixture.pool.clone())
        .read_events_after(TENANT, None, 1_000)
        .await
        .expect("events");
    let applied: Vec<_> = events
        .iter()
        .filter(|event| event.event_type.to_string() == "work.plan_applied")
        .collect();
    assert_eq!(applied.len(), 3);
    for event in &applied {
        assert_eq!(event.actor.kind, ActorKind::User);
        assert_eq!(event.actor.id, USER);
    }
    let proposed: Vec<_> = events
        .iter()
        .filter(|event| event.event_type.to_string() == "work.plan_proposed")
        .collect();
    assert_eq!(proposed.len(), 3);
    for event in &proposed {
        assert_eq!(
            event
                .causation_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some(COMMAND)
        );
        assert_eq!(
            event.command_id.map(|id| id.to_string()).as_deref(),
            Some(COMMAND)
        );
    }
    finish(fixture).await;
}

#[tokio::test]
async fn the_graph_transaction_refuses_a_compiled_plan_that_breaks_a_rule_and_writes_nothing() {
    let Some(fixture) = prepare("qa001_gp_guard").await else {
        blocked_marker();
        return;
    };
    let base = revision(&fixture).await;
    let seed_revision = fixture
        .port
        .snapshot(WORKSPACE)
        .await
        .expect("snapshot")
        .nodes
        .iter()
        .find(|node| node.id == SEED_NODE)
        .expect("seed")
        .revision;
    let apply = |plan: CompiledPlan| {
        let port = Arc::clone(&fixture.port);
        async move { port.apply(plan).await.expect_err("the plan is refused") }
    };

    assert_eq!(apply(plan(base)).await, PlanError::EmptyPlan);

    let mut uncontracted = plan(base);
    uncontracted.nodes_add = vec![compiled_node("bare", None, json!({}))];
    assert_eq!(
        apply(uncontracted).await,
        PlanError::MissingCompletionContract {
            node: "bare".to_string()
        }
    );

    // The final narrowing guard: a need wider than the proposer's grant (given as a single
    // object rather than a list) is refused.
    let mut widening = plan(base);
    widening.proposer_grants = grants()[0].clone();
    let mut wide = compiled_node("wide", Some(SEED_NODE), contract());
    wide.capability_needs = json!([{
        "effect_class": "fs.write.workspace",
        "resource": { "kind": "fs", "selector": "/etc/**" },
        "constraints": {}
    }]);
    widening.nodes_add = vec![wide];
    let error = apply(widening).await;
    assert_eq!(error.code(), "CAPABILITY_DENIED", "{error}");

    let mut stale = plan(base + 3);
    stale.nodes_add = vec![compiled_node("late", Some(SEED_NODE), contract())];
    assert_eq!(
        apply(stale).await,
        PlanError::StaleBaseRevision {
            expected: base + 3,
            current: base
        }
    );

    let mut foreign = plan(base);
    foreign.nodes_update = vec![CompiledNodeUpdate {
        node_id: OTHER_NODE.to_string(),
        expected_revision: 1,
        to: NodeStatus::Ready,
    }];
    let error = apply(foreign).await;
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert!(error.to_string().contains("different workspace"), "{error}");

    let mut racing = plan(base);
    racing.nodes_update = vec![CompiledNodeUpdate {
        node_id: SEED_NODE.to_string(),
        expected_revision: seed_revision + 7,
        to: NodeStatus::Ready,
    }];
    let error = apply(racing).await;
    assert_eq!(error.code(), "CONFLICT_REVISION", "{error}");

    let mut illegal = plan(base);
    illegal.nodes_update = vec![CompiledNodeUpdate {
        node_id: SEED_NODE.to_string(),
        expected_revision: seed_revision,
        to: NodeStatus::Done,
    }];
    // Completion is never self-certified through a plan: the store refuses the transition.
    // (Its typed CONFLICT_STATE is flattened by the port; see the QA-001 finding.)
    let error = apply(illegal).await;
    assert!(error.to_string().contains("done"), "{error}");

    let mut malformed = plan(base);
    malformed.nodes_update = vec![CompiledNodeUpdate {
        node_id: "draft-node".to_string(),
        expected_revision: 1,
        to: NodeStatus::Ready,
    }];
    assert_eq!(apply(malformed).await.code(), "VALIDATION_SCHEMA");
    let mut malformed = plan(base);
    malformed.edges_remove = vec![CompiledEdgeRemoval {
        edge_id: "edge-1".to_string(),
        expected_revision: 1,
    }];
    assert_eq!(apply(malformed).await.code(), "VALIDATION_SCHEMA");

    // Nothing above reached the graph.
    assert_eq!(revision(&fixture).await, base);
    assert_eq!(
        fixture
            .port
            .snapshot(WORKSPACE)
            .await
            .expect("snapshot")
            .nodes
            .len(),
        1,
        "only the seed node exists"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn the_graph_transaction_refuses_a_cycle_and_a_plan_above_the_policy_bound() {
    let Some(fixture) = prepare("qa001_gp_bounds").await else {
        blocked_marker();
        return;
    };
    let base = revision(&fixture).await;
    let mut child = plan(base);
    child.nodes_add = vec![compiled_node("child", Some(SEED_NODE), contract())];
    fixture.port.apply(child).await.expect("the child applies");
    let child_id = node_id(&fixture, "child").await;
    let base = revision(&fixture).await;

    // The child already hangs under the seed, so the seed depending on the child is a cycle.
    let mut cyclic = plan(base);
    cyclic.edges_add = vec![CompiledEdge {
        from_node_id: SEED_NODE.to_string(),
        to_node_id: child_id.clone(),
        kind: EdgeKind::DependsOn,
    }];
    let error = fixture.port.apply(cyclic).await.expect_err("a cycle");
    assert!(matches!(error, PlanError::Cycle { .. }), "{error}");

    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules, max_plan_nodes) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0QA0G1', $1, NULL, 'tenant', '[]'::jsonb, 1)",
    )
    .bind(TENANT)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    let mut large = plan(base);
    large.nodes_add = vec![
        compiled_node("one", Some(SEED_NODE), contract()),
        compiled_node("two", Some(SEED_NODE), contract()),
    ];
    assert_eq!(
        fixture.port.apply(large).await.expect_err("too many"),
        PlanError::Bounds {
            requested: 2,
            limit: 1
        }
    );
    assert_eq!(
        revision(&fixture).await,
        base,
        "no refused plan was applied"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_port_for_a_malformed_tenant_reads_nothing() {
    let Some(fixture) = prepare("qa001_gp_tenant").await else {
        blocked_marker();
        return;
    };
    let identity = RuntimeIdentity::system(
        "tenant-without-prefix",
        "graph-port-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let port = GraphPlanWorkspace::new(fixture.pool.clone(), identity);
    let error = port.snapshot(WORKSPACE).await.expect_err("no tenant scope");
    assert_eq!(error.code(), "INTERNAL");
    assert!(
        !error.is_rejection(),
        "an infrastructure fault is not a plan verdict"
    );
    finish(fixture).await;
}

/// The graph-backed port with another writer committing between the planner's snapshot and
/// its commit, once.
struct RacingPort {
    inner: Arc<GraphPlanWorkspace>,
    raced: AtomicBool,
}

#[async_trait]
impl PlanWorkspacePort for RacingPort {
    async fn snapshot(&self, workspace_id: &str) -> Result<PlanGraphSnapshot, PlanError> {
        self.inner.snapshot(workspace_id).await
    }

    async fn apply(&self, compiled: CompiledPlan) -> Result<PlanCommitOutcome, PlanError> {
        if !self.raced.swap(true, Ordering::SeqCst) {
            let mut other = plan(compiled.base_revision.get());
            other.proposal_id = "prop_racing_writer".to_string();
            other.nodes_add = vec![compiled_node("racing writer", Some(SEED_NODE), contract())];
            self.inner
                .apply(other)
                .await
                .expect("the racing writer commits");
        }
        self.inner.apply(compiled).await
    }
}

#[tokio::test]
async fn a_writer_racing_the_planner_between_compile_and_commit_is_recorded_as_a_revision_conflict()
{
    let Some(fixture) = prepare("qa001_gp_race").await else {
        blocked_marker();
        return;
    };
    let racing = Arc::new(RacingPort {
        inner: Arc::clone(&fixture.port),
        raced: AtomicBool::new(false),
    });
    let planner = Planner::new(
        fixture.pool.clone(),
        racing as Arc<dyn PlanWorkspacePort>,
        user_identity(),
    )
    .expect("planner");
    let base = revision(&fixture).await;

    let error = planner
        .propose(
            &raw(
                base,
                json!({ "nodes_add": [{ "id": "mine", "kind": "task", "title": "mine",
                                         "parent_id": SEED_NODE, "completion_contract": contract() }] }),
            ),
            proposer(),
        )
        .await
        .expect_err("the compare-and-set refuses the stale plan");
    assert_eq!(
        error,
        PlanError::StaleBaseRevision {
            expected: base,
            current: base + 1
        }
    );

    let events = planner
        .events()
        .read_events_after(TENANT, None, 1_000)
        .await
        .expect("events");
    let rejected: Vec<_> = events
        .iter()
        .filter(|event| event.event_type.to_string() == "work.plan_rejected")
        .collect();
    assert_eq!(rejected.len(), 1, "the refusal is audited once");
    assert_eq!(rejected[0].payload["code"], json!("CONFLICT_REVISION"));
    assert_eq!(
        rejected[0].payload["proposal_id"],
        json!(format!("prop_{base}"))
    );
    let titles: Vec<String> = GraphStore::new(fixture.pool.clone(), TENANT)
        .expect("store")
        .list_nodes(WORKSPACE)
        .await
        .expect("nodes")
        .into_iter()
        .map(|node| node.title)
        .collect();
    assert!(titles.iter().any(|title| title == "racing writer"));
    assert!(
        !titles.iter().any(|title| title == "mine"),
        "the stale plan wrote nothing"
    );
    finish(fixture).await;
}
