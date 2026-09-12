//! Atomic multi-graph mutation: a batch is all-or-nothing (CORE-004).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, WORKSPACE};
use quansio_core::Revision;
use quansio_graph::{
    AgentKind, GraphBatch, GraphChange, GraphError, GraphStore, NewAgentThread, NewRun, NewTurn,
    RunStatus, RunTriggerKind, WorkEdgeKind, WorkNodeKind,
};

fn actor() -> serde_json::Value {
    serde_json::json!({"kind": "user", "id": "usr_seed"})
}

fn new_node(title: &str) -> quansio_graph::NewWorkNode {
    quansio_graph::NewWorkNode::new(WORKSPACE, WorkNodeKind::Task, title, actor())
}

#[tokio::test]
async fn one_invalid_item_rolls_back_the_whole_batch() {
    let Some((name, pool)) = prepare("batch_rollback").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let base = store.graph_revision(WORKSPACE).await.expect("head");
    let seed =
        quansio_core::CanonicalId::parse_typed(common::SEED_NODE, quansio_core::Prefix::WorkNode)
            .expect("seed id");

    // The batch creates a node and an edge, then a second edge that closes a cycle. The
    // earlier items must not survive the failure.
    let batch = GraphBatch::new(WORKSPACE, base)
        .with(GraphChange::create_work_node(new_node("batch node")))
        .with(GraphChange::create_work_edge(
            quansio_graph::NewWorkEdge::new(WORKSPACE, seed, seed, WorkEdgeKind::DependsOn),
        ))
        .with(GraphChange::create_work_node(new_node("never applied")));
    let error = store.apply_batch(batch).await.expect_err("batch must fail");
    assert!(matches!(error, GraphError::Cycle { .. }), "{error:?}");

    let nodes = store.list_nodes(WORKSPACE).await.expect("nodes");
    assert_eq!(nodes.len(), 1, "only the seed node remains: {nodes:?}");
    assert!(store.list_edges(WORKSPACE).await.expect("edges").is_empty());
    assert_eq!(
        store.graph_revision(WORKSPACE).await.expect("head"),
        base,
        "a rolled-back batch does not advance the graph revision"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn stale_batch_revision_applies_nothing() {
    let Some((name, pool)) = prepare("batch_stale").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    store
        .create_node(new_node("first"))
        .await
        .expect("create node");
    let revision = store.graph_revision(WORKSPACE).await.expect("head");

    let applied = store
        .apply_batch(
            GraphBatch::new(WORKSPACE, revision).with(GraphChange::create_work_node(new_node("b"))),
        )
        .await
        .expect("batch applies");
    assert_eq!(applied.revision, Revision::new(revision.get() + 1));
    assert_eq!(applied.applied, 1);
    let after = store.graph_revision(WORKSPACE).await.expect("head");
    assert_eq!(after, applied.revision);
    let count = store.list_nodes(WORKSPACE).await.expect("nodes").len();

    // The same base revision is now stale, so nothing in this batch is written.
    let error = store
        .apply_batch(
            GraphBatch::new(WORKSPACE, revision).with(GraphChange::create_work_node(new_node("c"))),
        )
        .await
        .expect_err("stale batch");
    match &error {
        GraphError::RevisionConflict {
            entity,
            expected,
            current,
            ..
        } => {
            assert_eq!(*entity, "graph_head");
            assert_eq!(*expected, revision.get());
            assert_eq!(*current, after.get());
        }
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(error.code(), "CONFLICT_REVISION");
    assert_eq!(
        store.list_nodes(WORKSPACE).await.expect("nodes").len(),
        count
    );
    assert_eq!(store.graph_revision(WORKSPACE).await.expect("head"), after);

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn one_batch_mutates_work_agent_and_state_graphs_together() {
    let Some((name, pool)) = prepare("batch_multi").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");

    // Pre-existing fixtures the batch refers to.
    let node = store
        .create_node(new_node("seed task"))
        .await
        .expect("node");
    let thread = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("thread");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE,
            node.id,
            thread.id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let parent = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Teammate))
        .await
        .expect("parent");

    let base = store.graph_revision(WORKSPACE).await.expect("head");
    let batch = GraphBatch::new(WORKSPACE, base)
        .with(GraphChange::create_work_node(new_node("batched work")))
        .with(GraphChange::create_work_node(new_node("batched work 2")))
        .with(GraphChange::create_agent_thread(NewAgentThread::new(
            WORKSPACE,
            AgentKind::Worker,
        )))
        .with(GraphChange::delegate(
            parent.id,
            quansio_graph::DelegationRequest::new(NewAgentThread::new(
                WORKSPACE,
                AgentKind::Worker,
            )),
        ))
        .with(GraphChange::TransitionRun {
            run_id: run.id,
            to: RunStatus::Queued,
            terminal_reason: None,
        })
        .with(GraphChange::CreateTurn {
            run_id: run.id,
            turn: NewTurn::new("manual"),
        });

    let outcome = store.apply_batch(batch).await.expect("batch applies");
    assert_eq!(outcome.applied, 6);

    assert_eq!(
        store.list_nodes(WORKSPACE).await.expect("nodes").len(),
        4,
        "the seed node, one pre-existing node and two batched nodes"
    );
    assert_eq!(
        store
            .list_agent_threads(WORKSPACE)
            .await
            .expect("threads")
            .len(),
        4,
        "two pre-existing threads plus the batched thread and the delegated child"
    );
    assert_eq!(
        store
            .list_delegations(WORKSPACE)
            .await
            .expect("delegations")
            .len(),
        1
    );
    assert_eq!(
        store.get_run(&run.id).await.expect("run").status,
        RunStatus::Queued
    );
    assert_eq!(store.list_turns(&run.id).await.expect("turns").len(), 1);
    assert_eq!(
        store.graph_revision(WORKSPACE).await.expect("head"),
        outcome.revision
    );

    drop_pool(&pool, &name).await;
}
