//! Cycle rejection for the acyclic WorkGraph relations (CORE-004).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (DSN used to create the scratch database);
//! see `tests/common/mod.rs`. Absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, WORKSPACE};
use quansio_graph::{GraphError, GraphStore, NewWorkEdge, NewWorkNode, WorkEdgeKind, WorkNodeKind};
use serde_json::json;

fn actor() -> serde_json::Value {
    json!({"kind": "user", "id": "usr_seed"})
}

async fn make_node(store: &GraphStore, title: &str) -> quansio_graph::WorkNode {
    store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            title,
            actor(),
        ))
        .await
        .expect("create node")
}

#[tokio::test]
async fn graph_rejects_self_two_node_and_longer_cycles() {
    let Some((name, pool)) = prepare("cycle").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");

    let a = make_node(&store, "a").await;
    let b = make_node(&store, "b").await;
    let c = make_node(&store, "c").await;

    // A self edge is rejected before the database constraint fires.
    let error = store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            a.id,
            a.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect_err("self edge");
    assert!(matches!(error, GraphError::Cycle { .. }), "{error:?}");

    // a -> b is fine; b -> a closes a two-node cycle.
    store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            a.id,
            b.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect("a depends on b");
    let error = store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            b.id,
            a.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect_err("two-node cycle");
    assert!(matches!(error, GraphError::Cycle { .. }), "{error:?}");

    // a -> b -> c; c -> a closes a longer cycle.
    store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            b.id,
            c.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect("b depends on c");
    let error = store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            c.id,
            a.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect_err("long cycle");
    assert!(matches!(error, GraphError::Cycle { .. }), "{error:?}");

    // depends_on and parent_of share one acyclic structure: with a depends_on b,
    // making b the parent of a closes a mixed cycle.
    let error = store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            b.id,
            a.id,
            WorkEdgeKind::ParentOf,
        ))
        .await
        .expect_err("parent_of cycle");
    assert!(matches!(error, GraphError::Cycle { .. }), "{error:?}");

    // A node whose parent chain reaches a cannot also be depended on by a.
    let mut child = NewWorkNode::new(WORKSPACE, WorkNodeKind::Subtask, "child", actor());
    child.parent_id = Some(a.id);
    let d = store.create_node(child).await.expect("create child");
    let error = store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            a.id,
            d.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect_err("parent chain cycle");
    assert!(matches!(error, GraphError::Cycle { .. }), "{error:?}");

    // Non-acyclic relations are still allowed between the same pair.
    store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            a.id,
            b.id,
            WorkEdgeKind::ProducesArtifact,
        ))
        .await
        .expect("produces_artifact edge");

    let edges = store.list_edges(WORKSPACE).await.expect("list edges");
    assert_eq!(
        edges.len(),
        3,
        "rejected edges must not be persisted: {edges:?}"
    );

    drop_pool(&pool, &name).await;
}
