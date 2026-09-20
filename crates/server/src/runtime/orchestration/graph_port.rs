//! The graph-backed [`WorkGraphPort`] (RUN-001 over CORE-005): ready-queue selection reads
//! the canonical work rows and every release goes through `GraphTransaction`, so
//! orchestration never keeps a second copy of node state.

use std::collections::HashMap;

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, Revision, UlidGenerator};
use quansio_events::EventStore;
use quansio_graph::transaction::{GraphTransaction, TransactionContext};
use quansio_graph::{GraphBatch, GraphChange, GraphStore, WorkNodeStatus};
use sqlx::{PgPool, Row};

use super::{
    DependencyEdge, GraphSnapshot, OrchestrationError, ReleaseBatch, ReleaseOutcome, RunRef,
    WorkGraphPort, WorkNodeView,
};
use crate::control::schema;

/// The task that owns the graph transaction.
const GRAPH_OWNER: &str = "CORE-005";

/// The production port: read the graph, release through the graph transaction.
pub struct GraphWorkGraph {
    pool: PgPool,
    tenant_id: String,
}

impl GraphWorkGraph {
    /// A port over the pool for one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.into(),
        }
    }

    fn unavailable(error: impl std::fmt::Display) -> OrchestrationError {
        OrchestrationError::PortUnavailable {
            owner: GRAPH_OWNER,
            detail: error.to_string(),
        }
    }

    fn store(&self) -> Result<GraphStore, OrchestrationError> {
        GraphStore::new(self.pool.clone(), self.tenant_id.clone()).map_err(Self::unavailable)
    }
}

#[async_trait]
impl WorkGraphPort for GraphWorkGraph {
    async fn snapshot(&self, workspace_id: &str) -> Result<GraphSnapshot, OrchestrationError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.tenant_id)
            .await
            .map_err(Self::unavailable)?;
        let nodes = sqlx::query(
            "SELECT id, kind, status, parent_id, priority, revision FROM work_nodes \
             WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let edges = sqlx::query(
            "SELECT from_node_id, to_node_id, kind FROM work_edges \
             WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let runs = sqlx::query(
            "SELECT DISTINCT ON (work_node_id) work_node_id, id, generation, status FROM runs \
             WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY work_node_id, created_at DESC",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let revision: i64 = sqlx::query_scalar(
            "SELECT revision FROM graph_heads WHERE tenant_id = $1 AND workspace_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(1);
        tx.commit().await?;

        let mut run_by_node: HashMap<String, RunRef> = HashMap::new();
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
        let store = self.store()?;
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
            Err(error) => Err(Self::unavailable(error)),
        }
    }

    async fn revision(&self, workspace_id: &str) -> Result<u64, OrchestrationError> {
        let revision = self
            .store()?
            .graph_revision(workspace_id)
            .await
            .map_err(Self::unavailable)?;
        Ok(revision.get())
    }
}
