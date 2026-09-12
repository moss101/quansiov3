//! The graph-backed orchestration port: ready-queue selection reads real graph rows and
//! dependency release commits through `GraphTransaction` (RUN-004, DOMAIN.md §4.1, §4.2).
//!
//! `crates/server` cannot depend on `quansio_graph` (the graph depends on the server's
//! `control::schema`, and the workspace conformance gate forbids the cycle), so orchestration
//! composes the WorkGraph through `WorkGraphPort` — exactly as planning composes
//! `PlanWorkspacePort`. This test supplies the production-shaped implementation and proves the
//! two properties the port has to carry:
//!
//! * a snapshot of real `work_nodes`/`work_edges` rows feeds the pure ready-queue, and a
//!   dependent node is **never** releasable while any prerequisite is not `done`;
//! * a release batch applies as **one** revision-checked graph transaction, and a replay of the
//!   same batch conflicts instead of applying twice.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

mod common;

use async_trait::async_trait;
use common::{blocked_marker, drop_pool, prepare, SEED_NODE, TENANT, WORKSPACE};
use quansio_core::{CanonicalId, CorrelationId, Prefix, Revision, UlidGenerator};
use quansio_events::EventStore;
use quansio_graph::transaction::{GraphTransaction, TransactionContext};
use quansio_graph::{
    GraphBatch, GraphChange, GraphStore, NewWorkEdge, NewWorkNode, WorkEdgeKind, WorkNodeKind,
    WorkNodeStatus,
};
use quansio_server::runtime::orchestration::{
    ensure_acyclic, join_plan, release_plan, select, DependencyEdge, GraphSnapshot, NodeTransition,
    OrchestrationError, ReleaseBatch, ReleaseOutcome, RunRef, WorkGraphPort, WorkNodeView,
};
use sqlx::{PgPool, Row};

/// The production-shaped port: read the graph, release through the graph transaction.
struct GraphWorkGraph {
    pool: PgPool,
}

#[async_trait]
impl WorkGraphPort for GraphWorkGraph {
    async fn snapshot(&self, workspace_id: &str) -> Result<GraphSnapshot, OrchestrationError> {
        let mut tx = self.pool.begin().await?;
        quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
            .await
            .map_err(|error| OrchestrationError::PortUnavailable {
                owner: "CORE-005",
                detail: error.to_string(),
            })?;
        let nodes = sqlx::query(
            "SELECT id, kind, status, parent_id, priority, revision FROM work_nodes \
             WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
        )
        .bind(TENANT)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let edges = sqlx::query(
            "SELECT from_node_id, to_node_id, kind FROM work_edges \
             WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
        )
        .bind(TENANT)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let runs = sqlx::query(
            "SELECT DISTINCT ON (work_node_id) work_node_id, id, generation, status FROM runs \
             WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY work_node_id, created_at DESC",
        )
        .bind(TENANT)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let revision: i64 = sqlx::query_scalar(
            "SELECT revision FROM graph_heads WHERE tenant_id = $1 AND workspace_id = $2",
        )
        .bind(TENANT)
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(1);
        tx.commit().await?;

        let mut run_by_node: std::collections::HashMap<String, RunRef> =
            std::collections::HashMap::new();
        for row in &runs {
            let work_node_id: String = row.try_get("work_node_id")?;
            run_by_node.insert(
                work_node_id,
                RunRef {
                    run_id: row.try_get("id")?,
                    generation: u64::try_from(row.try_get::<i64, _>("generation")?).unwrap_or(1),
                    status: row.try_get("status")?,
                },
            );
        }

        let mut views = Vec::with_capacity(nodes.len());
        for row in &nodes {
            let id: String = row.try_get("id")?;
            views.push(WorkNodeView {
                run: run_by_node.get(&id).cloned(),
                id,
                kind: row.try_get("kind")?,
                status: row.try_get("status")?,
                parent_id: row.try_get("parent_id")?,
                priority: row.try_get("priority")?,
                revision: u64::try_from(row.try_get::<i64, _>("revision")?).unwrap_or(1),
            });
        }
        let edges = edges
            .iter()
            .map(|row| {
                Ok(DependencyEdge {
                    from_node_id: row.try_get("from_node_id")?,
                    to_node_id: row.try_get("to_node_id")?,
                    kind: row.try_get("kind")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;

        Ok(GraphSnapshot {
            workspace_id: workspace_id.to_string(),
            revision: u64::try_from(revision).unwrap_or(1),
            nodes: views,
            edges,
        })
    }

    async fn apply(&self, batch: ReleaseBatch) -> Result<ReleaseOutcome, OrchestrationError> {
        let store = GraphStore::new(self.pool.clone(), TENANT).map_err(|error| {
            OrchestrationError::PortUnavailable {
                owner: "CORE-005",
                detail: error.to_string(),
            }
        })?;
        let mut graph_batch = GraphBatch::new(
            batch.workspace_id.clone(),
            Revision::new(batch.base_revision),
        );
        for transition in &batch.transitions {
            let node_id = CanonicalId::parse_typed(&transition.node_id, Prefix::WorkNode)?;
            let to = WorkNodeStatus::from_db_str(&transition.to).map_err(|error| {
                OrchestrationError::IllegalNodeTransition {
                    node_id: transition.node_id.clone(),
                    from: transition.to.clone(),
                    to: error.to_string(),
                }
            })?;
            graph_batch.push(GraphChange::TransitionWorkNode {
                node_id,
                expected: Revision::new(transition.expected_revision),
                to,
            });
        }
        let transaction = GraphTransaction::new(
            store,
            EventStore::new(self.pool.clone()),
            TransactionContext::system(
                "orchestration",
                CorrelationId::generate(&mut UlidGenerator::new()),
            ),
        );
        match transaction.apply(graph_batch).await {
            Ok(outcome) => Ok(ReleaseOutcome {
                revision: outcome.revision.get(),
                applied: outcome.applied,
            }),
            Err(quansio_graph::GraphTransactionError::Graph(
                quansio_graph::GraphError::RevisionConflict {
                    expected, current, ..
                },
            )) => Err(OrchestrationError::RevisionConflict {
                workspace_id: batch.workspace_id,
                expected,
                current,
            }),
            Err(error) => Err(OrchestrationError::PortUnavailable {
                owner: "CORE-005",
                detail: error.to_string(),
            }),
        }
    }

    async fn revision(&self, workspace_id: &str) -> Result<u64, OrchestrationError> {
        let store = GraphStore::new(self.pool.clone(), TENANT).map_err(|error| {
            OrchestrationError::PortUnavailable {
                owner: "CORE-005",
                detail: error.to_string(),
            }
        })?;
        let revision = store.graph_revision(workspace_id).await.map_err(|error| {
            OrchestrationError::PortUnavailable {
                owner: "CORE-005",
                detail: error.to_string(),
            }
        })?;
        Ok(revision.get())
    }
}

/// Advance a node to `done` through the only legal path:
/// draft → ready → in_progress → verifying → passed.
async fn complete(store: &GraphStore, node_id: &CanonicalId) {
    let mut node = store.get_node(node_id).await.expect("node");
    if node.status == WorkNodeStatus::Draft {
        node = store
            .transition_node(node_id, node.revision, WorkNodeStatus::Ready)
            .await
            .unwrap_or_else(|error| panic!("ready: {error}"));
    }
    let node = store
        .transition_node(node_id, node.revision, WorkNodeStatus::InProgress)
        .await
        .unwrap_or_else(|error| panic!("in_progress: {error}"));
    let node = store
        .transition_node(node_id, node.revision, WorkNodeStatus::Verifying)
        .await
        .unwrap_or_else(|error| panic!("verifying: {error}"));
    store
        .mark_verification_passed(node_id, node.revision)
        .await
        .unwrap_or_else(|error| panic!("done: {error}"));
}

#[tokio::test]
async fn a_dependent_is_not_releasable_until_every_prerequisite_is_done() {
    let Some((name, pool)) = prepare("run004_graph").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), TENANT).expect("store");
    let node = |id: &str| CanonicalId::parse_typed(id, Prefix::WorkNode).expect("node id");

    // The seed node is `a`; `b` and `c` depend on it, and `d` depends on both.
    let a = node(SEED_NODE);
    let created_by = serde_json::json!({"kind": "system", "id": "orchestration-test"});
    let b = store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "b",
            created_by.clone(),
        ))
        .await
        .expect("b");
    let c = store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "c",
            created_by.clone(),
        ))
        .await
        .expect("c");
    let d = store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "d",
            created_by,
        ))
        .await
        .expect("d");
    for (from, to) in [(b.id, a), (c.id, a), (d.id, b.id), (d.id, c.id)] {
        store
            .create_edge(NewWorkEdge::new(
                WORKSPACE,
                from,
                to,
                WorkEdgeKind::DependsOn,
            ))
            .await
            .unwrap_or_else(|error| panic!("edge: {error}"));
    }

    let port = GraphWorkGraph { pool: pool.clone() };
    let snapshot = port.snapshot(WORKSPACE).await.expect("snapshot");
    ensure_acyclic(&snapshot).expect("acyclic");
    assert_eq!(snapshot.prerequisites(&d.id.to_string()).len(), 2);

    // Only `a` is releasable: it has no prerequisites, while b, c and d each wait on one.
    let plan = release_plan(&snapshot);
    let releasable: Vec<&str> = plan
        .transitions
        .iter()
        .map(|transition| transition.node_id.as_str())
        .collect();
    assert_eq!(releasable, vec![a.to_string().as_str()]);
    for dependent in [&b, &c, &d] {
        assert!(
            !releasable.contains(&dependent.id.to_string().as_str()),
            "{} must not be releasable while its prerequisite is unfinished",
            dependent.id
        );
    }

    // `a` completes: b and c become ready, d stays draft because c is unfinished.
    complete(&store, &a).await;
    let snapshot = port.snapshot(WORKSPACE).await.expect("snapshot");
    let plan = release_plan(&snapshot);
    let released: Vec<&str> = plan
        .transitions
        .iter()
        .map(|transition| transition.node_id.as_str())
        .collect();
    assert_eq!(released.len(), 2, "b and c are releasable: {released:?}");
    assert!(
        !released.contains(&d.id.to_string().as_str()),
        "d must not be released while c is unfinished"
    );

    // The release lands as one graph transaction with one revision bump.
    let before = snapshot.revision;
    let outcome = port
        .apply(ReleaseBatch {
            workspace_id: WORKSPACE.to_string(),
            base_revision: before,
            transitions: plan.transitions.clone(),
        })
        .await
        .expect("release applies");
    assert_eq!(outcome.applied, 2);
    assert_eq!(outcome.revision, before + 1, "one revision bump per batch");

    let after = port.snapshot(WORKSPACE).await.expect("snapshot");
    assert!(release_plan(&after).is_empty(), "the release is idempotent");
    assert_eq!(after.node(&b.id.to_string()).expect("b").status, "ready");
    assert_eq!(after.node(&d.id.to_string()).expect("d").status, "draft");
    assert!(
        select(&after, 4).is_empty(),
        "a ready node with no queued run is not dispatchable"
    );

    // Replaying the identical batch conflicts instead of applying a second time.
    let replay = port
        .apply(ReleaseBatch {
            workspace_id: WORKSPACE.to_string(),
            base_revision: before,
            transitions: plan.transitions,
        })
        .await;
    assert!(
        matches!(replay, Err(OrchestrationError::RevisionConflict { .. })),
        "a stale batch is refused: {replay:?}"
    );

    // `b` and `c` complete; only now does d become ready.
    complete(&store, &b.id).await;
    complete(&store, &c.id).await;
    let snapshot = port.snapshot(WORKSPACE).await.expect("snapshot");
    let plan = release_plan(&snapshot);
    assert_eq!(
        plan.transitions
            .iter()
            .map(|transition| transition.node_id.as_str())
            .collect::<Vec<_>>(),
        vec![d.id.to_string().as_str()],
        "d is released only once both prerequisites are done"
    );

    // And a parent waiting on children joins once the last child is done.
    let mut waiting = store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Objective,
            "parent",
            serde_json::json!({"kind": "system", "id": "orchestration-test"}),
        ))
        .await
        .expect("parent");
    assert_eq!(waiting.status, WorkNodeStatus::Draft);
    waiting = store
        .transition_node(&waiting.id, waiting.revision, WorkNodeStatus::Ready)
        .await
        .expect("ready");
    let waiting = store
        .transition_node(&waiting.id, waiting.revision, WorkNodeStatus::Waiting)
        .await
        .expect("waiting");
    let mut child = NewWorkNode::new(
        WORKSPACE,
        WorkNodeKind::Subtask,
        "child",
        serde_json::json!({"kind": "system", "id": "orchestration-test"}),
    );
    child.parent_id = Some(waiting.id);
    let child = store.create_node(child).await.expect("child");
    complete(&store, &child.id).await;

    let snapshot = port.snapshot(WORKSPACE).await.expect("snapshot");
    let joined = join_plan(&snapshot);
    assert_eq!(
        joined.transitions.len(),
        1,
        "the parent joins once its only child is done: {:?}",
        joined.transitions
    );
    assert_eq!(joined.transitions[0].node_id, waiting.id.to_string());
    assert_eq!(joined.transitions[0].to, "in_progress");
    let _: Vec<NodeTransition> = joined.transitions;
    drop_pool(&pool, &name).await;
}
