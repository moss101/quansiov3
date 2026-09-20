//! The graph-backed [`PlanWorkspacePort`] (RUN-003 over CORE-005).
//!
//! A [`CompiledPlan`] becomes a `quansio_graph` `PlanProposal` and is applied with
//! `GraphTransaction::apply_plan_with`, RUN-005's capability algebra being the final
//! narrowing guard. That is the one revision-checked, event-emitting mutation path of the
//! WorkGraph: a stale `base_revision` is `CONFLICT_REVISION` and writes nothing, and a
//! replay of an applied plan conflicts instead of duplicating work.

use async_trait::async_trait;
use quansio_capability::{check_narrowing_grants, CapabilityError, Grant};
use quansio_core::{CanonicalId, CorrelationId, Prefix, Revision};
use quansio_events::EventStore;
use quansio_graph::transaction::{
    GraphTransaction, GraphTransactionError, PlanCapabilityNarrowingCheck, PlanEdgeRemoval,
    PlanNodeUpdate, PlanProposal, PlanProposer, PlanRejection, TransactionContext,
};
use quansio_graph::{
    GraphError, GraphStore, NewWorkEdge, NewWorkNode, WorkEdgeKind, WorkNodeKind, WorkNodeStatus,
};
use serde_json::Value;
use sqlx::PgPool;

use super::compile::{CompiledPlan, PlanGraphSnapshot, SnapshotEdge, SnapshotNode};
use super::port::{PlanCommitOutcome, PlanWorkspacePort};
use super::wire::EdgeKind;
use super::PlanError;
use crate::runtime::state_machine::RuntimeIdentity;

/// The WorkGraph port backed by `quansio_graph`, acting as one identity.
pub struct GraphPlanWorkspace {
    pool: PgPool,
    identity: RuntimeIdentity,
}

impl GraphPlanWorkspace {
    /// A port over the pool for the identity every transaction is attributed to.
    #[must_use]
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Self {
        Self { pool, identity }
    }

    fn store(&self) -> Result<GraphStore, PlanError> {
        GraphStore::new(self.pool.clone(), self.identity.tenant_id.clone()).map_err(workspace_error)
    }

    fn context(&self) -> TransactionContext {
        let correlation: CorrelationId = self.identity.correlation_id;
        match &self.identity.actor {
            quansio_events::Actor {
                kind: quansio_events::ActorKind::User,
                id,
            } => TransactionContext::user(id.clone(), correlation),
            quansio_events::Actor {
                kind: quansio_events::ActorKind::Agent,
                id,
            } => TransactionContext::agent(id.clone(), correlation),
            quansio_events::Actor { id, .. } => TransactionContext::system(id.clone(), correlation),
        }
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
            self.context(),
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

/// RUN-005's capability algebra as the graph transaction's narrowing guard.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlgebraNarrowing;

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
