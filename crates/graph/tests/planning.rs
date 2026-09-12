//! The graph-backed planning port: compiled plans commit through `GraphTransaction`
//! (RUN-003, DOMAIN.md §4.5).
//!
//! The server-side planning module compiles a model proposal into a `CompiledPlan` and
//! hands it to a `PlanWorkspacePort`. This test supplies the production-shaped
//! implementation — it reads the WorkGraph, converts the compiled plan into `quansio_graph`
//! types and applies it with `GraphTransaction::apply_plan_with`, using RUN-005's capability
//! algebra as the final narrowing guard. That proves a compiled plan is committed as one
//! revision-checked, event-emitting transaction, and that a replay conflicts instead of
//! duplicating work.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{blocked_marker, drop_pool, prepare, SEED_NODE, TENANT, WORKSPACE};
use quansio_capability::{check_narrowing_grants, CapabilityError, Grant};
use quansio_core::{CanonicalId, CorrelationId, Prefix, Revision, UlidGenerator};
use quansio_events::EventStore;
use quansio_graph::transaction::{
    GraphTransaction, GraphTransactionError, PlanCapabilityNarrowingCheck, PlanEdgeRemoval,
    PlanNodeUpdate, PlanProposal, PlanProposer, PlanRejection, TransactionContext,
};
use quansio_graph::{
    GraphError, GraphStore, NewWorkEdge, NewWorkNode, WorkEdgeKind, WorkNodeKind, WorkNodeStatus,
};
use quansio_server::runtime::planning::{
    CompiledPlan, EdgeKind, PlanCommitOutcome, PlanError, PlanGraphSnapshot,
    PlanProposerProjection, PlanWorkspacePort, Planner, SnapshotEdge, SnapshotNode,
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

/// RUN-005's capability algebra as the graph transaction's narrowing guard.
#[derive(Debug, Clone, Copy, Default)]
struct AlgebraNarrowing;

impl PlanCapabilityNarrowingCheck for AlgebraNarrowing {
    fn check(&self, proposer: &PlanProposer, proposal: &PlanProposal) -> Result<(), PlanRejection> {
        let held = as_grants(&proposer.capability_needs);
        let mut needed = as_grants(&proposal.capability_needs);
        for node in &proposal.nodes_add {
            needed.extend(as_grants(&node.capability_needs));
        }
        match check_narrowing_grants(&held, &needed) {
            Ok(()) => Ok(()),
            Err(CapabilityError::WideningRejected(rejection)) => {
                Err(PlanRejection::CapabilityNotNarrowed {
                    need: rejection.grant.to_string(),
                })
            }
            Err(error) => Err(PlanRejection::CapabilityNotNarrowed {
                need: error.to_string(),
            }),
        }
    }
}

fn as_grants(value: &Value) -> Vec<Grant> {
    let templates: Vec<Value> = match value {
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    };
    templates
        .iter()
        .filter_map(|template| Grant::from_json(template).ok())
        .collect()
}

/// The WorkGraph port backed by `quansio_graph`.
struct GraphPlanWorkspace {
    pool: PgPool,
    tenant: String,
}

impl GraphPlanWorkspace {
    fn store(&self) -> Result<GraphStore, PlanError> {
        GraphStore::new(self.pool.clone(), self.tenant.clone()).map_err(workspace_error)
    }
}

#[async_trait]
impl PlanWorkspacePort for GraphPlanWorkspace {
    async fn snapshot(&self, workspace_id: &str) -> Result<PlanGraphSnapshot, PlanError> {
        let store = self.store()?;
        let nodes = store
            .list_nodes(workspace_id)
            .await
            .map_err(workspace_error)?;
        let edges = store
            .list_edges(workspace_id)
            .await
            .map_err(workspace_error)?;
        let revision = store
            .graph_revision(workspace_id)
            .await
            .map_err(workspace_error)?;
        Ok(PlanGraphSnapshot {
            workspace_id: workspace_id.to_string(),
            revision,
            nodes: nodes
                .iter()
                .map(|node| SnapshotNode {
                    id: node.id.to_string(),
                    parent_id: node.parent_id.as_ref().map(ToString::to_string),
                    revision: node.revision.get(),
                    completion_contract: node.completion_contract.clone(),
                })
                .collect(),
            edges: edges
                .iter()
                .map(|edge| SnapshotEdge {
                    id: edge.id.to_string(),
                    from_node_id: edge.from_node_id.to_string(),
                    to_node_id: edge.to_node_id.to_string(),
                    kind: edge_kind(edge.kind),
                    revision: edge.revision.get(),
                })
                .collect(),
        })
    }

    async fn apply(&self, plan: CompiledPlan) -> Result<PlanCommitOutcome, PlanError> {
        let proposal = to_proposal(&plan)?;
        let proposer = PlanProposer {
            agent_thread_id: plan
                .proposer_agent_thread_id
                .as_ref()
                .and_then(|id| CanonicalId::parse_typed(id, Prefix::AgentThread).ok()),
            capability_needs: plan.proposer_grants.clone(),
        };
        let transaction = GraphTransaction::new(
            self.store()?,
            EventStore::new(self.pool.clone()),
            TransactionContext::agent(AGENT, correlation()),
        );
        let outcome = transaction
            .apply_plan_with(&AlgebraNarrowing, &proposer, proposal)
            .await
            .map_err(map_transaction_error)?;
        Ok(PlanCommitOutcome {
            revision: outcome.revision.get(),
            applied: outcome.applied,
            event_ids: outcome.event_ids,
        })
    }
}

fn workspace_error(error: impl std::fmt::Display) -> PlanError {
    PlanError::Workspace {
        detail: error.to_string(),
    }
}

fn edge_kind(kind: WorkEdgeKind) -> EdgeKind {
    match kind {
        WorkEdgeKind::DependsOn => EdgeKind::DependsOn,
        WorkEdgeKind::ParentOf => EdgeKind::ParentOf,
        WorkEdgeKind::ProducesArtifact => EdgeKind::ProducesArtifact,
        WorkEdgeKind::VerifiedBy => EdgeKind::VerifiedBy,
        WorkEdgeKind::BlockedBy => EdgeKind::BlockedBy,
    }
}

fn parse_work_node(id: Option<&String>) -> Result<Option<CanonicalId>, PlanError> {
    id.map(|value| {
        CanonicalId::parse_typed(value, Prefix::WorkNode)
            .map_err(|error| PlanError::schema(error.to_string()))
    })
    .transpose()
}

fn to_proposal(plan: &CompiledPlan) -> Result<PlanProposal, PlanError> {
    let nodes_add = plan
        .nodes_add
        .iter()
        .map(|node| {
            let kind = WorkNodeKind::from_db_str(node.kind.as_str())
                .map_err(|error| PlanError::schema(error.to_string()))?;
            let mut created = NewWorkNode::new(
                plan.workspace_id.clone(),
                kind,
                node.title.clone(),
                plan.created_by.clone(),
            );
            created.description = node.description.clone();
            created.parent_id = parse_work_node(node.parent_id.as_ref())?;
            created.completion_contract = node.completion_contract.clone();
            created.capability_needs = node.capability_needs.clone();
            created.priority = node.priority;
            created.budget_id = node.budget_id.clone();
            created.thread_id = node
                .thread_id
                .as_ref()
                .map(|id| {
                    CanonicalId::parse_typed(id, Prefix::Thread)
                        .map_err(|error| PlanError::schema(error.to_string()))
                })
                .transpose()?;
            Ok(created)
        })
        .collect::<Result<Vec<_>, PlanError>>()?;

    let nodes_update = plan
        .nodes_update
        .iter()
        .map(|update| {
            Ok(PlanNodeUpdate {
                node_id: CanonicalId::parse_typed(&update.node_id, Prefix::WorkNode)
                    .map_err(|error| PlanError::schema(error.to_string()))?,
                expected_revision: Revision::new(update.expected_revision),
                to: WorkNodeStatus::from_db_str(update.to.as_str())
                    .map_err(|error| PlanError::schema(error.to_string()))?,
            })
        })
        .collect::<Result<Vec<_>, PlanError>>()?;

    let edges_add = plan
        .edges_add
        .iter()
        .map(|edge| {
            Ok(NewWorkEdge::new(
                plan.workspace_id.clone(),
                CanonicalId::parse_typed(&edge.from_node_id, Prefix::WorkNode)
                    .map_err(|error| PlanError::schema(error.to_string()))?,
                CanonicalId::parse_typed(&edge.to_node_id, Prefix::WorkNode)
                    .map_err(|error| PlanError::schema(error.to_string()))?,
                WorkEdgeKind::from_db_str(edge.kind.as_str())
                    .map_err(|error| PlanError::schema(error.to_string()))?,
            ))
        })
        .collect::<Result<Vec<_>, PlanError>>()?;

    let edges_remove = plan
        .edges_remove
        .iter()
        .map(|removal| {
            Ok(PlanEdgeRemoval {
                edge_id: CanonicalId::parse_typed(&removal.edge_id, Prefix::WorkEdge)
                    .map_err(|error| PlanError::schema(error.to_string()))?,
                expected_revision: Revision::new(removal.expected_revision),
            })
        })
        .collect::<Result<Vec<_>, PlanError>>()?;

    Ok(PlanProposal {
        proposal_id: plan.proposal_id.clone(),
        run_id: plan
            .run_id
            .as_ref()
            .and_then(|id| CanonicalId::parse_typed(id, Prefix::Run).ok()),
        workspace_id: plan.workspace_id.clone(),
        base_revision: plan.base_revision,
        nodes_add,
        nodes_update,
        edges_add,
        edges_remove,
        rationale: Some(plan.rationale.clone()),
        capability_needs: plan.capability_needs.clone(),
    })
}

fn map_transaction_error(error: GraphTransactionError) -> PlanError {
    match error {
        GraphTransactionError::PlanRejected { rejection, .. } => match rejection {
            PlanRejection::StaleBaseRevision { expected, current } => {
                PlanError::StaleBaseRevision { expected, current }
            }
            PlanRejection::TooManyNodes { requested, limit } => {
                PlanError::Bounds { requested, limit }
            }
            PlanRejection::CapabilityNotNarrowed { need } => {
                PlanError::CapabilityNotNarrowed { need }
            }
            PlanRejection::Cycle { from, to } => PlanError::Cycle { from, to },
            PlanRejection::MissingCompletionContract { node } => {
                PlanError::MissingCompletionContract { node }
            }
            PlanRejection::EmptyPlan => PlanError::EmptyPlan,
            PlanRejection::WorkspaceMismatch { .. } => {
                PlanError::schema("the proposal names a different workspace")
            }
        },
        GraphTransactionError::Graph(GraphError::RevisionConflict {
            expected, current, ..
        }) => PlanError::StaleBaseRevision { expected, current },
        other => PlanError::Workspace {
            detail: other.to_string(),
        },
    }
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
    let port: Arc<dyn PlanWorkspacePort> = Arc::new(GraphPlanWorkspace {
        pool: pool.clone(),
        tenant: TENANT.to_string(),
    });
    let planner = Planner::new(pool.clone(), port, identity()).expect("planner");
    Some(Fixture {
        name,
        pool: pool.clone(),
        events: EventStore::new(pool),
        planner,
    })
}

async fn event_types(events: &EventStore) -> Vec<String> {
    events
        .read_events_after(TENANT, None, 1000)
        .await
        .expect("read events")
        .into_iter()
        .map(|event| event.event_type.to_string())
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
            event_types(&fixture.events).await,
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
        assert_eq!(event_types(&restarted_events).await, snapshot.1);
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
        event_types(&fixture_b.events).await,
        node_shape(&fixture_b.pool).await,
    );

    assert_eq!(plan_a.0, snapshot_b.0, "same resulting revision");
    assert_eq!(plan_a.1, snapshot_b.1, "same event sequence");
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
