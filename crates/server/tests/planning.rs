//! PlanProposal ingestion, validation and commit tests (RUN-003, DOMAIN.md §4.5).
//!
//! These tests drive the server-side planning module against a real scratch PostgreSQL
//! database for the audit trail (`work.plan_proposed`/`work.plan_rejected` through the
//! canonical `EventStore`) and a recording [`PlanWorkspacePort`] for the WorkGraph.
//! `crates/server` cannot depend on `quansio-graph` (the graph store depends on this crate
//! for `control::schema`, and the workspace gate forbids the package cycle), so the real
//! graph-backed commit through `GraphTransaction` is proven in
//! `crates/graph/tests/planning.rs`.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};
use quansio_core::{CanonicalId, CorrelationId, EventId, Prefix, Revision, UlidGenerator};
use quansio_events::{EventDraft, EventStore, EventType, RuntimeEvent};
use quansio_server::control::schema;
use quansio_server::runtime::planning::{
    CompiledPlan, ContractSource, EdgeKind, PlanCommitOutcome, PlanError, PlanGraphSnapshot,
    PlanProposerProjection, PlanWorkspacePort, Planner, SnapshotEdge, SnapshotNode,
};
use quansio_server::runtime::state_machine::RuntimeIdentity;
use serde_json::{json, Value};
use sqlx::PgPool;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const SEED_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const OBJECTIVE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";

/// The CompletionContract a node can carry (DOMAIN.md §4.4).
fn contract() -> Value {
    json!({
        "deterministic_checks": [{"kind": "artifact_exists", "artifact_role": "report"}],
        "semantic_verification": {"required": false},
        "human_signoff_required": false
    })
}

/// The proposer's capability projection: one filesystem write grant.
fn grants() -> Value {
    json!([{
        "effect_class": "fs.write.workspace",
        "resource": {"kind": "fs", "selector": "/work/**"},
        "constraints": {"max_tier": 1}
    }])
}

fn proposer() -> PlanProposerProjection {
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

fn snapshot(nodes: Vec<SnapshotNode>, edges: Vec<SnapshotEdge>) -> PlanGraphSnapshot {
    PlanGraphSnapshot {
        workspace_id: WORKSPACE.to_string(),
        revision: Revision::new(1),
        nodes,
        edges,
    }
}

fn snapshot_node(id: &str, parent: Option<&str>, contract: Value) -> SnapshotNode {
    SnapshotNode {
        id: id.to_string(),
        parent_id: parent.map(ToString::to_string),
        revision: 1,
        completion_contract: contract,
    }
}

/// A model proposal node with its own CompletionContract.
fn node(id: &str, kind: &str, contract: Value) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "title": format!("node {id}"),
        "status": "draft",
        "completion_contract": contract,
        "capability_needs": [],
        "priority": 1
    })
}

fn proposal(base_revision: u64, nodes_add: Vec<Value>) -> Value {
    json!({
        "proposal_id": "prop_01J8Z3K6F1N8VQ2X5W9Y0GGGGG",
        "run_id": "run_01J8Z3K6F1N8VQ2X5W9Y0GGGGG",
        "workspace_id": WORKSPACE,
        "base_revision": base_revision,
        "nodes_add": nodes_add,
        "nodes_update": [],
        "edges_add": [],
        "edges_remove": [],
        "rationale": "bounded plan",
        "capability_needs": []
    })
}

/// An in-memory WorkGraph that records the compiled plan and emits `work.plan_applied`.
///
/// Compilation is fully validated before this port is reached, so the recording
/// implementation only has to honour the base-revision compare-and-set, materialise the
/// mutation and emit the applied event through the canonical event store.
struct RecordingWorkspace {
    events: EventStore,
    identity: RuntimeIdentity,
    workspace: String,
    state: Mutex<PlanGraphSnapshot>,
}

impl RecordingWorkspace {
    fn new(events: EventStore, identity: RuntimeIdentity, state: PlanGraphSnapshot) -> Self {
        let workspace = state.workspace_id.clone();
        Self {
            events,
            identity,
            workspace,
            state: Mutex::new(state),
        }
    }

    fn node_count(&self) -> usize {
        self.state.lock().expect("state").nodes.len()
    }

    fn revision(&self) -> u64 {
        self.state.lock().expect("state").revision.get()
    }

    async fn emit_applied(&self, plan: &CompiledPlan, revision: u64) -> Result<EventId, PlanError> {
        let tenant_id = self.identity.tenant_id.clone();
        let actor = self.identity.actor.clone();
        let correlation_id = self.identity.correlation_id;
        let workspace = self.workspace.clone();
        let payload = json!({
            "proposal_id": plan.proposal_id,
            "base_revision": plan.base_revision.get(),
            "revision": revision,
            "nodes_added": plan.nodes_add.len(),
            "nodes_updated": plan.nodes_update.len(),
            "edges_added": plan.edges_add.len(),
            "edges_removed": plan.edges_remove.len(),
            "rationale": plan.rationale,
        });
        self.events
            .commit_mutation(&tenant_id, move |_conn, batch| {
                Box::pin(async move {
                    let event_type =
                        EventType::parse("work.plan_applied").expect("canonical event type");
                    let draft = EventDraft::new(
                        "work_graph",
                        workspace.clone(),
                        revision,
                        event_type,
                        correlation_id,
                        actor,
                    )
                    .with_workspace(workspace)
                    .with_payload(payload);
                    let event_id = draft.event_id;
                    batch.emit(draft);
                    Ok(event_id)
                })
            })
            .await
            .map_err(|error| PlanError::Workspace {
                detail: error.to_string(),
            })
    }
}

#[async_trait]
impl PlanWorkspacePort for RecordingWorkspace {
    async fn snapshot(&self, workspace_id: &str) -> Result<PlanGraphSnapshot, PlanError> {
        if workspace_id != self.workspace {
            return Err(PlanError::Workspace {
                detail: format!("workspace {workspace_id} is not the port's workspace"),
            });
        }
        Ok(self.state.lock().expect("state").clone())
    }

    async fn apply(&self, plan: CompiledPlan) -> Result<PlanCommitOutcome, PlanError> {
        let applied = plan.change_count();
        let revision;
        {
            let mut state = self.state.lock().expect("state");
            if plan.base_revision != state.revision {
                return Err(PlanError::StaleBaseRevision {
                    expected: plan.base_revision.get(),
                    current: state.revision.get(),
                });
            }
            for node in &plan.nodes_add {
                let id = CanonicalId::generate(Prefix::WorkNode, &mut UlidGenerator::new());
                state.nodes.push(SnapshotNode {
                    id: id.to_string(),
                    parent_id: node.parent_id.clone(),
                    revision: 1,
                    completion_contract: node.completion_contract.clone(),
                });
            }
            for edge in &plan.edges_add {
                let id = CanonicalId::generate(Prefix::WorkEdge, &mut UlidGenerator::new());
                state.edges.push(SnapshotEdge {
                    id: id.to_string(),
                    from_node_id: edge.from_node_id.clone(),
                    to_node_id: edge.to_node_id.clone(),
                    kind: edge.kind,
                    revision: 1,
                });
            }
            for removal in &plan.edges_remove {
                state.edges.retain(|edge| edge.id != removal.edge_id);
            }
            for update in &plan.nodes_update {
                if let Some(node) = state
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == update.node_id)
                {
                    node.revision += 1;
                }
            }
            revision = state.revision.get() + 1;
            state.revision = Revision::new(revision);
        }
        let event_id = self.emit_applied(&plan, revision).await?;
        Ok(PlanCommitOutcome {
            revision,
            applied,
            event_ids: vec![event_id],
        })
    }
}

/// A migrated scratch database seeded with one tenant/workspace/work node plus a planner.
struct Fixture {
    name: String,
    pool: PgPool,
    events: EventStore,
    planner: Planner,
    workspace: Arc<RecordingWorkspace>,
}

async fn setup(prefix: &str, state: PlanGraphSnapshot) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, SEED_NODE).await;
    let events = EventStore::new(pool.clone());
    let workspace = Arc::new(RecordingWorkspace::new(events.clone(), identity(), state));
    let port: Arc<dyn PlanWorkspacePort> = Arc::clone(&workspace) as Arc<dyn PlanWorkspacePort>;
    let planner = Planner::new(pool.clone(), port, identity()).expect("planner");
    Some(Fixture {
        name,
        pool,
        events,
        planner,
        workspace,
    })
}

async fn event_types(store: &EventStore) -> Vec<String> {
    read_events(store)
        .await
        .into_iter()
        .map(|event| event.event_type.to_string())
        .collect()
}

async fn read_events(store: &EventStore) -> Vec<RuntimeEvent> {
    store
        .read_events_after(TENANT, None, 1000)
        .await
        .expect("read events")
}

async fn event_count(pool: &PgPool) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let count = sqlx::query_scalar("SELECT count(*) FROM runtime_events")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    count
}

async fn seed_policy(pool: &PgPool, max_plan_nodes: i32) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, max_plan_nodes) \
         VALUES ($1, $2, NULL, 'tenant', $3)",
    )
    .bind("pol_01J8Z3K6F1N8VQ2X5W9Y0GGGGG")
    .bind(TENANT)
    .bind(max_plan_nodes)
    .execute(&mut *tx)
    .await
    .expect("insert policy");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn malformed_proposals_are_rejected_and_write_nothing() {
    let Some(fixture) = setup("planning_malformed", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };

    // Missing identity is not a PlanProposal at all: no event, no graph read.
    let missing = json!({"workspace_id": WORKSPACE, "base_revision": 1, "rationale": "r"});
    let error = fixture
        .planner
        .propose(&missing, proposer())
        .await
        .expect_err("missing proposal_id");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert_eq!(event_count(&fixture.pool).await, 0, "nothing is written");

    // Missing rationale is malformed too.
    let mut no_rationale = proposal(1, vec![node("a", "task", contract())]);
    no_rationale["rationale"] = json!("");
    let error = fixture
        .planner
        .propose(&no_rationale, proposer())
        .await
        .expect_err("missing rationale");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");

    // Unknown kind.
    let unknown_kind = proposal(1, vec![node("a", "errand", contract())]);
    let error = fixture
        .planner
        .propose(&unknown_kind, proposer())
        .await
        .expect_err("unknown kind");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");

    // Unknown status.
    let mut unknown_status = node("a", "task", contract());
    unknown_status["status"] = json!("almost_done");
    let error = fixture
        .planner
        .propose(&proposal(1, vec![unknown_status]), proposer())
        .await
        .expect_err("unknown status");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");

    assert_eq!(
        event_count(&fixture.pool).await,
        0,
        "malformed input writes no audit event either"
    );
    assert_eq!(fixture.workspace.node_count(), 0);

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn duplicate_ids_and_unknown_endpoints_are_rejected() {
    let Some(fixture) = setup("planning_duplicate", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };

    // Duplicate node ids.
    let duplicate = proposal(
        1,
        vec![node("a", "task", contract()), node("a", "task", contract())],
    );
    let error = fixture
        .planner
        .propose(&duplicate, proposer())
        .await
        .expect_err("duplicate node ids");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert_eq!(
        event_types(&fixture.events)
            .await
            .last()
            .map(String::as_str),
        Some("work.plan_rejected")
    );

    // An edge to an unknown node.
    let mut unknown_edge = proposal(1, vec![node("a", "task", contract())]);
    unknown_edge["edges_add"] = json!([{
        "id": "e1",
        "from_node_id": "a",
        "to_node_id": "ghost",
        "kind": "depends_on"
    }]);
    let error = fixture
        .planner
        .propose(&unknown_edge, proposer())
        .await
        .expect_err("edge to an unknown node");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");

    assert_eq!(fixture.workspace.node_count(), 0, "no node was created");
    assert_eq!(fixture.workspace.revision(), 1);
    let events = read_events(&fixture.events).await;
    assert_eq!(
        events.last().expect("rejection").payload["code"],
        "VALIDATION_SCHEMA"
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn a_plan_above_the_policy_bound_is_rejected_with_validation_bounds() {
    let Some(fixture) = setup("planning_bounds", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture.pool, 1).await;

    let error = fixture
        .planner
        .propose(
            &proposal(
                1,
                vec![node("a", "task", contract()), node("b", "task", contract())],
            ),
            proposer(),
        )
        .await
        .expect_err("two nodes exceed max_plan_nodes = 1");
    assert_eq!(error.code(), "VALIDATION_BOUNDS");
    assert!(matches!(
        error,
        PlanError::Bounds {
            requested: 2,
            limit: 1
        }
    ));

    let events = read_events(&fixture.events).await;
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.to_string())
            .collect::<Vec<_>>(),
        vec!["work.plan_proposed", "work.plan_rejected"]
    );
    assert_eq!(events[1].payload["code"], "VALIDATION_BOUNDS");
    assert_eq!(fixture.workspace.node_count(), 0, "nothing was written");

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn a_self_edge_cycle_is_rejected() {
    let Some(fixture) = setup("planning_self_edge", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };
    let mut cyclic = proposal(1, vec![node("a", "task", contract())]);
    cyclic["edges_add"] = json!([{
        "id": "e1", "from_node_id": "a", "to_node_id": "a", "kind": "depends_on"
    }]);

    let error = fixture
        .planner
        .propose(&cyclic, proposer())
        .await
        .expect_err("a self edge is a cycle");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert!(matches!(error, PlanError::Cycle { .. }));
    assert_eq!(fixture.workspace.node_count(), 0);

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn a_two_node_cycle_is_rejected() {
    let Some(fixture) = setup("planning_two_node", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };
    let mut cyclic = proposal(
        1,
        vec![node("a", "task", contract()), node("b", "task", contract())],
    );
    cyclic["edges_add"] = json!([
        {"id": "e1", "from_node_id": "a", "to_node_id": "b", "kind": "depends_on"},
        {"id": "e2", "from_node_id": "b", "to_node_id": "a", "kind": "depends_on"}
    ]);

    let error = fixture
        .planner
        .propose(&cyclic, proposer())
        .await
        .expect_err("two-node cycle");
    assert!(matches!(error, PlanError::Cycle { .. }), "{error:?}");

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn a_cycle_through_proposed_parent_ids_is_rejected() {
    let Some(fixture) = setup("planning_parent_cycle", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };
    let mut parent_a = node("a", "task", contract());
    parent_a["parent_id"] = json!("b");
    let mut parent_b = node("b", "task", json!({}));
    parent_b["parent_id"] = json!("a");

    let error = fixture
        .planner
        .propose(&proposal(1, vec![parent_a, parent_b]), proposer())
        .await
        .expect_err("parent_id cycle");
    assert!(matches!(error, PlanError::Cycle { .. }), "{error:?}");

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn a_cycle_against_the_existing_graph_is_rejected() {
    let x = snapshot_node("wn_01J8Z3K6F1N8VQ2X5W9Y0XXXXX", None, contract());
    let y = snapshot_node("wn_01J8Z3K6F1N8VQ2X5W9Y0YYYYY", None, contract());
    let edge = SnapshotEdge {
        id: "we_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
        from_node_id: x.id.clone(),
        to_node_id: y.id.clone(),
        kind: EdgeKind::DependsOn,
        revision: 1,
    };
    let Some(fixture) = setup("planning_existing_cycle", snapshot(vec![x, y], vec![edge])).await
    else {
        blocked_marker();
        return;
    };

    let mut cyclic = proposal(1, vec![]);
    cyclic["edges_add"] = json!([{
        "id": "e1",
        "from_node_id": "wn_01J8Z3K6F1N8VQ2X5W9Y0YYYYY",
        "to_node_id": "wn_01J8Z3K6F1N8VQ2X5W9Y0XXXXX",
        "kind": "depends_on"
    }]);
    let error = fixture
        .planner
        .propose(&cyclic, proposer())
        .await
        .expect_err("the added edge closes the existing path");
    assert!(matches!(error, PlanError::Cycle { .. }), "{error:?}");

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn an_over_authorized_plan_is_rejected_and_writes_nothing() {
    let Some(fixture) = setup("planning_capability", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };
    let mut expensive = node("a", "task", contract());
    expensive["capability_needs"] = json!([{
        "effect_class": "payment.execute",
        "resource": {"kind": "connector", "selector": "cnx_01J8Z3K6F1N8VQ2X5W9Y0GGGGG"},
        "constraints": {}
    }]);

    let error = fixture
        .planner
        .propose(&proposal(1, vec![expensive]), proposer())
        .await
        .expect_err("a plan may only narrow the proposer's projection");
    assert_eq!(error.code(), "CAPABILITY_DENIED");
    assert!(matches!(error, PlanError::CapabilityNotNarrowed { .. }));

    let events = read_events(&fixture.events).await;
    assert_eq!(
        events.last().expect("rejection").payload["code"],
        "CAPABILITY_DENIED"
    );
    assert_eq!(fixture.workspace.node_count(), 0, "nothing was written");
    assert_eq!(fixture.workspace.revision(), 1);

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn completion_contract_inheritance_is_recorded_and_missing_contracts_are_rejected() {
    let objective = snapshot_node(OBJECTIVE, None, contract());
    let Some(fixture) = setup("planning_contract", snapshot(vec![objective], vec![])).await else {
        blocked_marker();
        return;
    };

    let mut inheriting = node("child", "subtask", json!({}));
    inheriting["parent_id"] = json!(OBJECTIVE);
    let outcome = fixture
        .planner
        .propose(&proposal(1, vec![inheriting]), proposer())
        .await
        .expect("a node inherits its parent's contract");
    assert_eq!(
        outcome.contract_sources,
        vec![ContractSource::InheritedFrom {
            parent_id: OBJECTIVE.to_string(),
            created_in_plan: false,
        }]
    );
    assert_eq!(outcome.nodes_added, 1);
    assert_eq!(outcome.revision, 2);

    let types = event_types(&fixture.events).await;
    assert_eq!(types, vec!["work.plan_proposed", "work.plan_applied"]);

    // A node with neither its own contract nor a parent that provides one is rejected.
    let orphan = node("orphan", "task", json!({}));
    let mut second = proposal(2, vec![orphan]);
    second["proposal_id"] = json!("prop_01J8Z3K6F1N8VQ2X5W9Y0HHHHH");
    let error = fixture
        .planner
        .propose(&second, proposer())
        .await
        .expect_err("an orphan node has no contract");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert!(matches!(error, PlanError::MissingCompletionContract { .. }));
    assert_eq!(
        fixture.workspace.node_count(),
        2,
        "the rejected plan added no node"
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn replaying_an_applied_plan_conflicts_and_never_duplicates_nodes() {
    let Some(fixture) = setup("planning_replay", snapshot(vec![], vec![])).await else {
        blocked_marker();
        return;
    };
    let plan = proposal(1, vec![node("once", "task", contract())]);

    let first = fixture
        .planner
        .propose(&plan, proposer())
        .await
        .expect("first application");
    assert_eq!(first.revision, 2);
    assert_eq!(fixture.workspace.node_count(), 1);

    // The same proposal with the same base_revision is stale: the second attempt is
    // rejected by the revision compare-and-set and adds nothing.
    let error = fixture
        .planner
        .propose(&plan, proposer())
        .await
        .expect_err("a replayed proposal is stale");
    assert_eq!(error.code(), "CONFLICT_REVISION");
    assert!(matches!(error, PlanError::StaleBaseRevision { .. }));
    assert_eq!(
        fixture.workspace.node_count(),
        1,
        "the plan's node exists exactly once"
    );
    assert_eq!(fixture.workspace.revision(), 2);

    let types = event_types(&fixture.events).await;
    assert_eq!(
        types,
        vec![
            "work.plan_proposed",
            "work.plan_applied",
            "work.plan_proposed",
            "work.plan_rejected"
        ]
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn the_same_proposal_over_the_same_starting_state_is_deterministic() {
    let mut plan = proposal(
        1,
        vec![
            node("first", "task", contract()),
            node("second", "subtask", json!({})),
        ],
    );
    plan["nodes_add"][1]["parent_id"] = json!(OBJECTIVE);
    plan["edges_add"] = json!([{
        "id": "e1",
        "from_node_id": "wn_01J8Z3K6F1N8VQ2X5W9Y0XXXXX",
        "to_node_id": "wn_01J8Z3K6F1N8VQ2X5W9Y0YYYYY",
        "kind": "depends_on"
    }]);

    let state = || {
        snapshot(
            vec![
                snapshot_node(OBJECTIVE, None, contract()),
                snapshot_node("wn_01J8Z3K6F1N8VQ2X5W9Y0XXXXX", None, contract()),
                snapshot_node("wn_01J8Z3K6F1N8VQ2X5W9Y0YYYYY", None, contract()),
            ],
            vec![],
        )
    };

    let Some(first) = setup("planning_det_a", state()).await else {
        blocked_marker();
        return;
    };
    let Some(second) = setup("planning_det_b", state()).await else {
        blocked_marker();
        drop_pool(&first.pool, &first.name).await;
        return;
    };

    let first_outcome = first
        .planner
        .propose(&plan, proposer())
        .await
        .expect("first run");
    let second_outcome = second
        .planner
        .propose(&plan, proposer())
        .await
        .expect("second run");

    assert_eq!(first_outcome.revision, second_outcome.revision);
    assert_eq!(
        first_outcome.mutation_summary,
        second_outcome.mutation_summary
    );
    assert_eq!(
        first_outcome.contract_sources,
        second_outcome.contract_sources
    );
    assert_eq!(
        event_types(&first.events).await,
        event_types(&second.events).await
    );

    // A simulated restart: a brand-new store over the same durable database replays the
    // same event sequence.
    let restarted = EventStore::new(first.pool.clone());
    assert_eq!(
        event_types(&restarted).await,
        event_types(&first.events).await
    );

    drop_pool(&first.pool, &first.name).await;
    drop_pool(&second.pool, &second.name).await;
}
