//! The script's side of the WorkGraph (ADP-007, DOMAIN.md §13.8 E).
//!
//! Nodes and edges are created only through the RUN-003 [`Planner`] (one validated PlanProposal, one GraphTransaction), and every
//! status change is a `GraphTransaction` batch. The planner cannot connect a node to another created in the same transaction, so a
//! batch of nodes lands first and the edges that join them land in a second proposal that names the nodes by their ids. A node
//! is found again by the `wf_key` its own CompletionContract carries, among the children of its parent, so a restart between the
//! two proposals (or inside a fan-out) continues instead of creating a second node.

use std::collections::HashMap;
use std::sync::Arc;

use quansio_core::{CanonicalId, CorrelationId, Prefix, Revision, UlidGenerator};
use quansio_events::EventStore;
use quansio_graph::transaction::{GraphTransaction, GraphTransactionError, TransactionContext};
use quansio_graph::{GraphBatch, GraphChange, GraphError, GraphStore, WorkNodeStatus};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use super::{ScriptRef, WorkflowError};
use crate::control::schema;
use crate::runtime::planning::{
    max_plan_nodes, GraphPlanWorkspace, PlanError, PlanProposerProjection, Planner,
};
use crate::runtime::state_machine::RuntimeIdentity;

/// One node of the script, as the executor needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRef {
    /// `wn_…`.
    pub id: String,
    /// Its status.
    pub status: String,
}

/// A node to add: its key, kind and title, and the node it hangs under.
#[derive(Debug, Clone)]
pub struct NewNode {
    /// The key that finds it again.
    pub wf_key: String,
    /// `task` or `subtask`.
    pub kind: &'static str,
    /// Its title.
    pub title: String,
    /// Its parent, an existing node.
    pub parent_id: String,
    /// `agent`, `parallel`, `reduce` or `item`.
    pub step_kind: &'static str,
}

/// The completion contract of a script node. It binds nothing (no worker is offered a completion claim: the reply is the result,
/// and the runtime, never the worker, checks it against the step's schema before it moves the node `done`), and it carries the
/// key that finds the node again in a `workflow` object the contract parser leaves alone.
#[must_use]
pub fn contract(wf_key: &str, step_kind: &str) -> Value {
    json!({
        "deterministic_checks": [],
        "semantic_verification": { "required": false },
        "human_signoff_required": false,
        "workflow": { "wf_key": wf_key, "step_kind": step_kind },
    })
}

/// The graph operations of one script run.
#[derive(Clone)]
pub struct ScriptGraph {
    pool: PgPool,
    script: ScriptRef,
}

impl ScriptGraph {
    /// The operations for `script`.
    #[must_use]
    pub fn new(pool: PgPool, script: ScriptRef) -> Self {
        Self { pool, script }
    }

    fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity::system(
            &self.script.tenant_id,
            format!("workflow:{}", self.script.run_id),
            CorrelationId::generate(&mut UlidGenerator::new()),
        )
    }

    fn store(&self) -> Result<GraphStore, WorkflowError> {
        GraphStore::new(self.pool.clone(), self.script.tenant_id.clone())
            .map_err(|error| WorkflowError::Graph(error.to_string()))
    }

    /// The child nodes of `parent`, by their keys.
    ///
    /// # Errors
    /// A database error.
    pub async fn children(&self, parent: &str) -> Result<HashMap<String, NodeRef>, WorkflowError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.script.tenant_id).await?;
        let rows = sqlx::query(
            "SELECT id, status, completion_contract #>> '{workflow,wf_key}' AS wf_key \
             FROM work_nodes WHERE tenant_id = $1 AND parent_id = $2",
        )
        .bind(&self.script.tenant_id)
        .bind(parent)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut found = HashMap::new();
        for row in rows {
            let key: Option<String> = row.try_get("wf_key")?;
            if let Some(key) = key {
                found.insert(
                    key,
                    NodeRef {
                        id: row.try_get("id")?,
                        status: row.try_get("status")?,
                    },
                );
            }
        }
        Ok(found)
    }

    /// Add nodes (and, in a second proposal, edges between existing nodes) through the planner. Nodes whose key already exists
    /// under their parent are not added again. Returns every node of the script's keys now present, by key.
    ///
    /// # Errors
    /// [`WorkflowError::Plan`] with the planner's own verdict.
    pub async fn plan(
        &self,
        label: &str,
        nodes: &[NewNode],
        edges_by_key: &[(String, String)],
        existing: &HashMap<String, NodeRef>,
        parents: &[String],
    ) -> Result<HashMap<String, NodeRef>, WorkflowError> {
        let identity = self.identity();
        let planner = Planner::new(
            self.pool.clone(),
            Arc::new(GraphPlanWorkspace::new(self.pool.clone(), identity.clone())),
            identity.clone(),
        )
        .map_err(|error| WorkflowError::Plan(error.to_string()))?;
        let proposer = PlanProposerProjection {
            agent_thread_id: Some(self.script.agent_thread_id.clone()),
            grants: json!([]),
            created_by: json!({
                "kind": "routine",
                "id": self.script.routine_id,
                "script_run": self.script.run_id,
            }),
        };
        let limit = max_plan_nodes(
            &self.pool,
            &self.script.tenant_id,
            &self.script.workspace_id,
        )
        .await
        .map_err(|error| WorkflowError::Plan(error.to_string()))?
        .max(1);
        let missing: Vec<&NewNode> = nodes
            .iter()
            .filter(|node| !existing.contains_key(&node.wf_key))
            .collect();
        for (batch_number, batch) in missing.chunks(limit).enumerate() {
            let raw = json!({
                "proposal_id": format!("wf:{}:{label}:nodes:{batch_number}", self.script.run_id),
                "run_id": self.script.run_id,
                "workspace_id": self.script.workspace_id,
                "base_revision": self.base_revision().await?.get(),
                "rationale": format!("workflow script {}: {label}", self.script.title),
                "nodes_add": batch.iter().enumerate().map(|(n, node)| json!({
                    "id": format!("n{n}"),
                    "kind": node.kind,
                    "title": node.title,
                    "parent_id": node.parent_id,
                    "completion_contract": contract(&node.wf_key, node.step_kind),
                    "priority": 1,
                })).collect::<Vec<_>>(),
                "capability_needs": [],
            });
            self.propose_with_retry(&planner, &raw, &proposer).await?;
        }
        // Everything the script has under its parents now.
        let mut present = existing.clone();
        for parent in parents {
            present.extend(self.children(parent).await?);
        }
        // The edges, between nodes that now exist, skipping the ones already there.
        let have = self.edges().await?;
        let wanted: Vec<(String, String)> = edges_by_key
            .iter()
            .filter_map(|(from, to)| {
                Some((present.get(from)?.id.clone(), present.get(to)?.id.clone()))
            })
            .filter(|pair| !have.contains(pair))
            .collect();
        for (batch_number, batch) in wanted.chunks(limit).enumerate() {
            let raw = json!({
                "proposal_id": format!("wf:{}:{label}:edges:{batch_number}", self.script.run_id),
                "run_id": self.script.run_id,
                "workspace_id": self.script.workspace_id,
                "base_revision": self.base_revision().await?.get(),
                "rationale": format!("workflow script {}: {label} dependencies", self.script.title),
                "edges_add": batch.iter().enumerate().map(|(n, (from, to))| json!({
                    "id": format!("e{n}"),
                    "from_node_id": from,
                    "to_node_id": to,
                    "kind": "depends_on",
                })).collect::<Vec<_>>(),
                "capability_needs": [],
            });
            self.propose_with_retry(&planner, &raw, &proposer).await?;
        }
        Ok(present)
    }

    async fn propose_with_retry(
        &self,
        planner: &Planner,
        raw: &Value,
        proposer: &PlanProposerProjection,
    ) -> Result<(), WorkflowError> {
        let mut raw = raw.clone();
        for attempt in 0..5 {
            match planner.propose(&raw, proposer.clone()).await {
                Ok(_) => return Ok(()),
                // Another writer moved the graph: the same proposal against the revision it is at now.
                Err(PlanError::StaleBaseRevision { .. }) if attempt < 4 => {
                    raw["base_revision"] = json!(self.base_revision().await?.get());
                }
                Err(error) => return Err(WorkflowError::Plan(error.to_string())),
            }
        }
        Err(WorkflowError::Plan("the graph kept moving".to_string()))
    }

    async fn base_revision(&self) -> Result<Revision, WorkflowError> {
        self.store()?
            .graph_revision(&self.script.workspace_id)
            .await
            .map_err(|error| WorkflowError::Graph(error.to_string()))
    }

    async fn edges(&self) -> Result<Vec<(String, String)>, WorkflowError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.script.tenant_id).await?;
        let rows = sqlx::query(
            "SELECT from_node_id, to_node_id FROM work_edges WHERE tenant_id = $1 AND workspace_id = $2 AND kind = 'depends_on'",
        )
        .bind(&self.script.tenant_id)
        .bind(&self.script.workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|row| Ok((row.try_get("from_node_id")?, row.try_get("to_node_id")?)))
            .collect::<Result<_, sqlx::Error>>()
            .map_err(WorkflowError::from)
    }

    /// Move a node toward `goal` one legal step at a time, each step one GraphTransaction. `Done` goes through `verifying` and
    /// the verification path; a node that is already past the goal (terminal) is left as it is.
    ///
    /// # Errors
    /// [`WorkflowError::Graph`] for a refusal that is not a lost race (a lost race is retried).
    pub async fn advance(&self, node_id: &str, goal: WorkNodeStatus) -> Result<(), WorkflowError> {
        let id = CanonicalId::parse_typed(node_id, Prefix::WorkNode)
            .map_err(|error| WorkflowError::Graph(error.to_string()))?;
        for _ in 0..12 {
            let store = self.store()?;
            let node = store
                .get_node(&id)
                .await
                .map_err(|error| WorkflowError::Graph(error.to_string()))?;
            let status = node.status;
            if status == goal || status.is_terminal() {
                return Ok(());
            }
            let change = match (goal, status) {
                (WorkNodeStatus::InProgress, WorkNodeStatus::Draft) => {
                    transition(&id, node.revision, WorkNodeStatus::Ready)
                }
                (WorkNodeStatus::InProgress, _) => {
                    transition(&id, node.revision, WorkNodeStatus::InProgress)
                }
                (WorkNodeStatus::Done, WorkNodeStatus::Verifying) => {
                    GraphChange::VerificationPassed {
                        node_id: id,
                        expected: node.revision,
                    }
                }
                (WorkNodeStatus::Done, WorkNodeStatus::InProgress) => {
                    transition(&id, node.revision, WorkNodeStatus::Verifying)
                }
                (WorkNodeStatus::Done | WorkNodeStatus::Failed, WorkNodeStatus::Draft) => {
                    transition(&id, node.revision, WorkNodeStatus::Ready)
                }
                (
                    WorkNodeStatus::Done | WorkNodeStatus::Failed,
                    WorkNodeStatus::Ready | WorkNodeStatus::Blocked | WorkNodeStatus::Waiting,
                ) => transition(&id, node.revision, WorkNodeStatus::InProgress),
                (
                    WorkNodeStatus::Failed,
                    WorkNodeStatus::InProgress | WorkNodeStatus::Verifying,
                ) => transition(&id, node.revision, WorkNodeStatus::Failed),
                (WorkNodeStatus::Cancelled, WorkNodeStatus::Verifying) => {
                    transition(&id, node.revision, WorkNodeStatus::Failed)
                }
                (WorkNodeStatus::Cancelled, _) => {
                    transition(&id, node.revision, WorkNodeStatus::Cancelled)
                }
                (goal, status) => {
                    return Err(WorkflowError::Graph(format!(
                        "no path from {} to {} for a script node",
                        status.as_db_str(),
                        goal.as_db_str()
                    )))
                }
            };
            let mut batch = GraphBatch::new(
                self.script.workspace_id.clone(),
                self.base_revision().await?,
            );
            batch.push(change);
            let transaction = GraphTransaction::new(
                store,
                EventStore::new(self.pool.clone()),
                TransactionContext::system(
                    format!("workflow:{}", self.script.run_id),
                    CorrelationId::generate(&mut UlidGenerator::new()),
                ),
            );
            match transaction.apply(batch).await {
                Ok(_) => {}
                // The orchestrator or another writer moved the graph or this node first: look again.
                Err(GraphTransactionError::Graph(
                    GraphError::RevisionConflict { .. } | GraphError::StateConflict { .. },
                )) => {}
                Err(error) => return Err(WorkflowError::Graph(error.to_string())),
            }
        }
        Err(WorkflowError::Graph(format!(
            "node {node_id} kept moving under the script"
        )))
    }
}

fn transition(id: &CanonicalId, expected: Revision, to: WorkNodeStatus) -> GraphChange {
    GraphChange::TransitionWorkNode {
        node_id: *id,
        expected,
        to,
    }
}
