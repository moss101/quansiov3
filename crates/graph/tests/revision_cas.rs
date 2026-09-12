//! Revision compare-and-set: a stale mutation fails and changes nothing (CORE-004).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, WORKSPACE};
use quansio_core::Revision;
use quansio_graph::{
    GraphError, GraphStore, NewWorkEdge, NewWorkNode, WorkEdgeKind, WorkNodeKind, WorkNodeStatus,
};

fn actor() -> serde_json::Value {
    serde_json::json!({"kind": "user", "id": "usr_seed"})
}

#[tokio::test]
async fn stale_revision_conflicts_and_writes_nothing() {
    let Some((name, pool)) = prepare("revision_cas").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let node = store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "cas",
            actor(),
        ))
        .await
        .expect("create node");
    assert_eq!(node.revision, Revision::INITIAL);

    let ready = store
        .transition_node(&node.id, node.revision, WorkNodeStatus::Ready)
        .await
        .expect("ready");
    assert_eq!(ready.revision, Revision::new(2));

    // The stale mutation is rejected with a typed conflict naming both revisions.
    let error = store
        .transition_node(&node.id, node.revision, WorkNodeStatus::InProgress)
        .await
        .expect_err("stale revision");
    match &error {
        GraphError::RevisionConflict {
            entity,
            expected,
            current,
            ..
        } => {
            assert_eq!(*entity, "work_node");
            assert_eq!(*expected, 1);
            assert_eq!(*current, 2);
        }
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(error.code(), "CONFLICT_REVISION");

    // Nothing changed: status, revision and the workspace graph head are untouched.
    let head_before = store.graph_revision(WORKSPACE).await.expect("head");
    let unchanged = store.get_node(&node.id).await.expect("node");
    assert_eq!(unchanged.status, WorkNodeStatus::Ready);
    assert_eq!(unchanged.revision, Revision::new(2));

    let error = store
        .transition_node(&node.id, node.revision, WorkNodeStatus::Waiting)
        .await
        .expect_err("stale revision again");
    assert!(matches!(error, GraphError::RevisionConflict { .. }));
    assert_eq!(
        store.graph_revision(WORKSPACE).await.expect("head"),
        head_before
    );
    assert_eq!(
        store.get_node(&node.id).await.expect("node").status,
        WorkNodeStatus::Ready
    );

    // Edge removal is compare-and-set on the edge revision too.
    let other = store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "other",
            actor(),
        ))
        .await
        .expect("create node");
    let edge = store
        .create_edge(NewWorkEdge::new(
            WORKSPACE,
            node.id,
            other.id,
            WorkEdgeKind::DependsOn,
        ))
        .await
        .expect("create edge");
    let error = store
        .remove_edge(&edge.id, Revision::new(9))
        .await
        .expect_err("stale edge revision");
    assert!(
        matches!(error, GraphError::RevisionConflict { .. }),
        "{error:?}"
    );
    assert_eq!(store.list_edges(WORKSPACE).await.expect("edges").len(), 1);

    store
        .remove_edge(&edge.id, Revision::INITIAL)
        .await
        .expect("remove edge");
    assert!(store.list_edges(WORKSPACE).await.expect("edges").is_empty());

    drop_pool(&pool, &name).await;
}
