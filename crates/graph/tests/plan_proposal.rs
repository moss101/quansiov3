//! PlanProposal application as one GraphTransaction (CORE-005, DOMAIN.md §4.5).
//!
//! A rejected proposal must leave the WorkGraph and the event log exactly as they were;
//! an accepted one is a single transaction that emits `work.plan_applied` plus the
//! per-node and per-edge events.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, TENANT, WORKSPACE};
use quansio_core::{CanonicalId, CorrelationId, Prefix, Revision, UlidGenerator};
use quansio_events::{EventStore, RuntimeEvent};
use quansio_graph::transaction::{
    GraphTransaction, GraphTransactionError, PlanEdgeRemoval, PlanNodeUpdate, PlanProposal,
    PlanProposer, PlanRejection, TransactionContext,
};
use quansio_graph::{
    GraphStore, NewWorkEdge, NewWorkNode, WorkEdgeKind, WorkNodeKind, WorkNodeStatus,
};
use serde_json::{json, Value};
use sqlx::PgPool;

/// The agent thread that proposed the plan.
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

fn proposer() -> serde_json::Value {
    json!({"kind": "agent", "id": AGENT})
}

fn contract() -> Value {
    json!({
        "deterministic_checks": [{"kind": "artifact_exists", "artifact_role": "report"}],
        "semantic_verification": {"required": false},
        "human_signoff_required": false
    })
}

fn node(title: &str) -> NewWorkNode {
    let mut node = NewWorkNode::new(WORKSPACE, WorkNodeKind::Task, title, proposer());
    node.completion_contract = contract();
    node
}

fn inheriting_node(title: &str, parent: &CanonicalId) -> NewWorkNode {
    let mut node = NewWorkNode::new(WORKSPACE, WorkNodeKind::Subtask, title, proposer());
    node.parent_id = Some(*parent);
    node
}

fn grants() -> Value {
    json!([{
        "effect_class": "fs.write.workspace",
        "resource": {"kind": "fs", "selector": "/work/**"},
        "constraints": {"max_tier": 1}
    }])
}

fn proposal(base_revision: Revision, nodes_add: Vec<NewWorkNode>) -> PlanProposal {
    PlanProposal {
        proposal_id: "prop_01J8Z3K6F1N8VQ2X5W9Y0CCCCC".to_string(),
        run_id: None,
        workspace_id: WORKSPACE.to_string(),
        base_revision,
        nodes_add,
        nodes_update: Vec::new(),
        edges_add: Vec::new(),
        edges_remove: Vec::new(),
        rationale: Some("bounded plan".to_string()),
        capability_needs: grants(),
    }
}

fn plan_proposer() -> PlanProposer {
    PlanProposer {
        agent_thread_id: None,
        capability_needs: grants(),
    }
}

fn correlation() -> CorrelationId {
    CorrelationId::generate(&mut UlidGenerator::new())
}

fn transaction(store: GraphStore, pool: &PgPool) -> GraphTransaction {
    GraphTransaction::new(
        store,
        EventStore::new(pool.clone()),
        TransactionContext::agent(AGENT, correlation()),
    )
}

async fn tenant_count(pool: &PgPool, sql: &str) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let count = sqlx::query_scalar(sql)
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    count
}

async fn events(store: &EventStore) -> Vec<RuntimeEvent> {
    store
        .read_events_after(TENANT, None, 1000)
        .await
        .expect("read events")
}

async fn setup(prefix: &str) -> Option<(String, PgPool, GraphStore)> {
    let (name, pool) = prepare(prefix).await?;
    let store = GraphStore::new(pool.clone(), TENANT).expect("store");
    Some((name, pool, store))
}

async fn seed_policy(pool: &PgPool, max_plan_nodes: i32) {
    let mut tx = pool.begin().await.expect("begin");
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, max_plan_nodes) \
         VALUES ($1, $2, NULL, 'tenant', $3)",
    )
    .bind("pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
    .bind(TENANT)
    .bind(max_plan_nodes)
    .execute(&mut *tx)
    .await
    .expect("insert policy");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn an_accepted_plan_is_one_transaction_with_its_own_event() {
    let Some((name, pool, store)) = setup("plan_accept").await else {
        blocked_marker();
        return;
    };
    let events_store = EventStore::new(pool.clone());

    // The objective provides the CompletionContract that one added node inherits.
    let mut objective =
        NewWorkNode::new(WORKSPACE, WorkNodeKind::Objective, "objective", proposer());
    objective.completion_contract = contract();
    let objective = store.create_node(objective).await.expect("objective");
    let existing = store
        .create_node(node("existing task"))
        .await
        .expect("node");
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let mut proposal = proposal(
        base,
        vec![
            node("planned task"),
            inheriting_node("inherited contract", &objective.id),
        ],
    );
    proposal.nodes_update.push(PlanNodeUpdate {
        node_id: existing.id,
        expected_revision: existing.revision,
        to: WorkNodeStatus::Ready,
    });
    proposal.edges_add.push(NewWorkEdge::new(
        WORKSPACE,
        objective.id,
        existing.id,
        WorkEdgeKind::DependsOn,
    ));

    let outcome = transaction(store, &pool)
        .apply_plan(&plan_proposer(), proposal)
        .await
        .expect("the plan is accepted");
    assert_eq!(outcome.applied, 4, "two nodes, one update, one edge");
    assert_eq!(outcome.revision, Revision::new(base.get() + 1));
    assert_eq!(
        outcome.event_ids.len(),
        5,
        "four changes plus the plan event"
    );

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    let nodes = reader.list_nodes(WORKSPACE).await.expect("nodes");
    assert_eq!(
        nodes.len(),
        5,
        "seed node, objective, existing task, two planned"
    );
    let planned = nodes
        .iter()
        .find(|candidate| candidate.title == "planned task")
        .expect("planned task");
    assert_eq!(
        planned.origin,
        quansio_graph::WorkOrigin::PlanProposal,
        "a node added by a plan records that origin"
    );
    let inherited = nodes
        .iter()
        .find(|candidate| candidate.title == "inherited contract")
        .expect("inherited node");
    assert_eq!(inherited.parent_id.as_ref(), Some(&objective.id));
    assert!(
        inherited
            .completion_contract
            .as_object()
            .is_some_and(|v| v.is_empty()),
        "the node inherits its parent's contract instead of carrying its own"
    );
    assert_eq!(
        reader.get_node(&existing.id).await.expect("node").status,
        WorkNodeStatus::Ready
    );
    assert_eq!(reader.list_edges(WORKSPACE).await.expect("edges").len(), 1);

    let committed = events(&events_store).await;
    let types: Vec<String> = committed
        .iter()
        .map(|event| event.event_type.to_string())
        .collect();
    assert_eq!(
        types,
        vec![
            "work.node_created",
            "work.node_created",
            "work.node_status_changed",
            "work.edge_added",
            "work.plan_applied",
        ]
    );
    let applied = committed.last().expect("plan event");
    assert_eq!(applied.aggregate_type, "work_graph");
    assert_eq!(applied.aggregate_id, WORKSPACE);
    assert_eq!(
        applied.aggregate_version,
        outcome.revision.get(),
        "the plan event carries the graph revision it produced"
    );
    assert_eq!(
        applied.payload["proposal_id"],
        "prop_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"
    );
    assert_eq!(applied.payload["base_revision"], base.get());
    assert_eq!(applied.payload["revision"], outcome.revision.get());
    assert_eq!(applied.payload["nodes_added"], 2);
    assert_eq!(applied.payload["nodes_updated"], 1);
    assert_eq!(applied.payload["edges_added"], 1);
    assert_eq!(applied.payload["edges_removed"], 0);
    assert_eq!(
        committed[0].aggregate_id,
        planned.id.to_string(),
        "the first node event names the node the plan created"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_plan_above_the_policy_bound_is_rejected_and_writes_nothing() {
    let Some((name, pool, store)) = setup("plan_bound").await else {
        blocked_marker();
        return;
    };
    seed_policy(&pool, 1).await;
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let error = transaction(store, &pool)
        .apply_plan(
            &plan_proposer(),
            proposal(base, vec![node("one"), node("two")]),
        )
        .await
        .expect_err("two nodes exceed max_plan_nodes = 1");
    assert_eq!(error.code(), "VALIDATION_BOUNDS");
    match &error {
        GraphTransactionError::PlanRejected { rejection, .. } => assert_eq!(
            rejection,
            &PlanRejection::TooManyNodes {
                requested: 2,
                limit: 1
            }
        ),
        other => panic!("unexpected error {other:?}"),
    }

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(reader.list_nodes(WORKSPACE).await.expect("nodes").len(), 1);
    assert_eq!(reader.graph_revision(WORKSPACE).await.expect("head"), base);
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        0
    );
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM event_outbox").await,
        0
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_plan_node_without_a_contract_or_an_inheriting_parent_is_rejected() {
    let Some((name, pool, store)) = setup("plan_contract").await else {
        blocked_marker();
        return;
    };
    // The seed node carries the schema default `{}`: it can provide no contract.
    let seed = CanonicalId::parse_typed(common::SEED_NODE, Prefix::WorkNode).expect("seed");
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let orphan = NewWorkNode::new(WORKSPACE, WorkNodeKind::Task, "no contract", proposer());
    let error = transaction(GraphStore::new(pool.clone(), TENANT).expect("store"), &pool)
        .apply_plan(&plan_proposer(), proposal(base, vec![orphan]))
        .await
        .expect_err("a node without a contract or an inheriting parent is rejected");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    match &error {
        GraphTransactionError::PlanRejected { rejection, .. } => assert!(matches!(
            rejection,
            PlanRejection::MissingCompletionContract { .. }
        )),
        other => panic!("unexpected error {other:?}"),
    }
    // Nothing was written, so the same base revision is still current.
    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(reader.graph_revision(WORKSPACE).await.expect("head"), base);

    // A parent that provides no contract is not an inheritance either.
    let inheriting = inheriting_node("inherits nothing", &seed);
    let error = transaction(GraphStore::new(pool.clone(), TENANT).expect("store"), &pool)
        .apply_plan(&plan_proposer(), proposal(base, vec![inheriting]))
        .await
        .expect_err("the parent has no contract to inherit");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(reader.list_nodes(WORKSPACE).await.expect("nodes").len(), 1);
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        0
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_plan_that_would_close_a_cycle_is_rejected() {
    let Some((name, pool, store)) = setup("plan_cycle").await else {
        blocked_marker();
        return;
    };
    let first = store.create_node(node("first")).await.expect("first");
    let second = store.create_node(node("second")).await.expect("second");
    store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            first.id,
            second.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect("edge");
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let mut proposal = proposal(base, Vec::new());
    proposal.rationale = None;
    proposal.edges_add.push(NewWorkEdge::new(
        WORKSPACE,
        second.id,
        first.id,
        WorkEdgeKind::DependsOn,
    ));
    let error = transaction(store, &pool)
        .apply_plan(&plan_proposer(), proposal)
        .await
        .expect_err("the plan would close a cycle");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    match &error {
        GraphTransactionError::PlanRejected { rejection, .. } => {
            assert!(matches!(rejection, PlanRejection::Cycle { .. }));
        }
        other => panic!("unexpected error {other:?}"),
    }

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(reader.list_edges(WORKSPACE).await.expect("edges").len(), 1);
    assert_eq!(reader.graph_revision(WORKSPACE).await.expect("head"), base);
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        0
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_plan_whose_needs_exceed_the_proposer_is_rejected() {
    let Some((name, pool, store)) = setup("plan_capability").await else {
        blocked_marker();
        return;
    };
    let base = store.graph_revision(WORKSPACE).await.expect("head");
    let mut proposal = proposal(base, vec![node("expensive")]);
    proposal.capability_needs = json!([{
        "effect_class": "payment.execute",
        "resource": {"kind": "connector", "selector": "cnx_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"},
        "constraints": {}
    }]);

    let error = transaction(store, &pool)
        .apply_plan(&plan_proposer(), proposal)
        .await
        .expect_err("a plan may only narrow the proposer's needs");
    assert_eq!(error.code(), "CAPABILITY_DENIED");
    match &error {
        GraphTransactionError::PlanRejected { rejection, .. } => {
            assert!(matches!(
                rejection,
                PlanRejection::CapabilityNotNarrowed { .. }
            ));
        }
        other => panic!("unexpected error {other:?}"),
    }

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(reader.list_nodes(WORKSPACE).await.expect("nodes").len(), 1);
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        0
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn replaying_an_applied_plan_conflicts_instead_of_duplicating_work() {
    let Some((name, pool, store)) = setup("plan_replay").await else {
        blocked_marker();
        return;
    };
    let events_store = EventStore::new(pool.clone());
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let plan = proposal(base, vec![node("planned once")]);
    let outcome = transaction(GraphStore::new(pool.clone(), TENANT).expect("store"), &pool)
        .apply_plan(&plan_proposer(), plan.clone())
        .await
        .expect("first application");
    let after_first = events(&events_store).await;
    assert_eq!(after_first.len(), outcome.event_ids.len());

    // The same proposal replayed with the same `base_revision` is stale: the plan is
    // rejected as one unit rather than applied twice.
    let error = transaction(GraphStore::new(pool.clone(), TENANT).expect("store"), &pool)
        .apply_plan(&plan_proposer(), plan.clone())
        .await
        .expect_err("a replayed proposal is stale");
    assert_eq!(error.code(), "CONFLICT_REVISION");
    match &error {
        GraphTransactionError::PlanRejected { rejection, .. } => assert_eq!(
            rejection,
            &PlanRejection::StaleBaseRevision {
                expected: base.get(),
                current: outcome.revision.get()
            }
        ),
        other => panic!("unexpected error {other:?}"),
    }

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    let nodes = reader.list_nodes(WORKSPACE).await.expect("nodes");
    assert_eq!(nodes.len(), 2, "the plan's node exists exactly once");
    assert_eq!(
        reader.graph_revision(WORKSPACE).await.expect("head"),
        outcome.revision
    );
    assert_eq!(
        events(&events_store).await.len(),
        after_first.len(),
        "the rejected replay emits no event"
    );

    // A plan removal is also compare-and-set on the edge revision the proposer observed.
    let first = reader
        .list_nodes(WORKSPACE)
        .await
        .expect("nodes")
        .into_iter()
        .find(|candidate| candidate.title == "planned once")
        .expect("planned node");
    let seed = CanonicalId::parse_typed(common::SEED_NODE, Prefix::WorkNode).expect("seed");
    let edge = reader
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            first.id,
            seed,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect("edge");
    let base = reader.graph_revision(WORKSPACE).await.expect("head");
    let mut removal = proposal(base, Vec::new());
    removal.edges_remove.push(PlanEdgeRemoval {
        edge_id: edge.id,
        expected_revision: edge.revision,
    });
    let removal_outcome = transaction(GraphStore::new(pool.clone(), TENANT).expect("store"), &pool)
        .apply_plan(&plan_proposer(), removal)
        .await
        .expect("the removal applies");
    assert_eq!(removal_outcome.applied, 1);
    assert!(reader
        .list_edges(WORKSPACE)
        .await
        .expect("edges")
        .is_empty());
    let committed = events(&events_store).await;
    assert_eq!(
        committed.last().expect("plan event").event_type.to_string(),
        "work.plan_applied"
    );
    assert!(
        committed
            .iter()
            .any(|event| event.event_type.to_string() == "work.edge_removed"),
        "the removal is recorded as work.edge_removed"
    );

    drop_pool(&pool, &name).await;
}
