//! PlanProposal validation and application (DOMAIN.md §4.5).
//!
//! A model proposes; the runtime validates and commits. Validation runs before the
//! transaction opens, so a rejected proposal leaves the WorkGraph, the AgentGraph, the
//! StateGraph and the event log exactly as they were:
//!
//! * the plan is bounded by `policies.max_plan_nodes` (`MIN` over the tenant and workspace
//!   policy rows, or [`DEFAULT_MAX_PLAN_NODES`] when the tenant has no policy row);
//! * the resulting `depends_on`/`parent_of` structure stays acyclic;
//! * every added node carries a CompletionContract or inherits one from its parent;
//! * every capability need is a narrowing of the proposer's needs, checked through
//!   [`PlanCapabilityNarrowingCheck`] — RUN-005 replaces [`StructuralPlanCapabilityCheck`]
//!   with the Capability Projection algebra by implementing the trait, without touching
//!   this module;
//! * `base_revision` is the current workspace graph revision. The compare-and-set inside
//!   the transaction is the authority: a proposal that raced another writer is rejected
//!   with `CONFLICT_REVISION` even if it passed the pre-flight read.

use std::collections::{HashMap, HashSet};

use quansio_core::{CanonicalId, Revision};
use serde_json::Value;

use super::GraphTransactionError;
use crate::batch::{GraphBatch, GraphChange};
use crate::error::GraphError;
use crate::state::{WorkNodeStatus, WorkOrigin};
use crate::store::GraphStore;
use crate::work::{NewWorkEdge, NewWorkNode, WorkEdge, WorkNode};

/// Upper bound used when the tenant has no `policies` row.
///
/// It mirrors the `policies.max_plan_nodes` default in `0001_canonical_schema.sql`, so a
/// tenant without a policy row is bounded exactly like one holding the default policy.
pub const DEFAULT_MAX_PLAN_NODES: usize = 25;

/// A model-proposed WorkGraph mutation awaiting validation (DOMAIN.md §4.5).
#[derive(Debug, Clone)]
pub struct PlanProposal {
    /// Identity of the proposal, for audit and idempotency reporting.
    pub proposal_id: String,
    /// The Run the proposal came from; `None` for a user-originated graph edit.
    pub run_id: Option<CanonicalId>,
    /// Workspace the proposal applies to.
    pub workspace_id: String,
    /// The graph revision the proposer observed; the compare-and-set key.
    pub base_revision: Revision,
    /// Nodes to create; each becomes a `work.node_created` event.
    pub nodes_add: Vec<NewWorkNode>,
    /// Node status updates, compare-and-set on the revision the proposer observed.
    pub nodes_update: Vec<PlanNodeUpdate>,
    /// Edges to create; each becomes a `work.edge_added` event.
    pub edges_add: Vec<NewWorkEdge>,
    /// Edges to remove; each becomes a `work.edge_removed` event.
    pub edges_remove: Vec<PlanEdgeRemoval>,
    /// The proposer's rationale, recorded for audit.
    pub rationale: Option<String>,
    /// Grant templates the plan needs (§6.1); must narrow the proposer's needs.
    pub capability_needs: Value,
}

/// One node status update inside a [`PlanProposal`].
#[derive(Debug, Clone)]
pub struct PlanNodeUpdate {
    /// The node to transition.
    pub node_id: CanonicalId,
    /// The revision the proposer observed.
    pub expected_revision: Revision,
    /// The requested state.
    pub to: WorkNodeStatus,
}

/// One edge removal inside a [`PlanProposal`].
#[derive(Debug, Clone)]
pub struct PlanEdgeRemoval {
    /// The edge to remove.
    pub edge_id: CanonicalId,
    /// The revision the proposer observed.
    pub expected_revision: Revision,
}

/// Who proposed a plan and the authority it was proposed under (DOMAIN.md §4.5, §6.2).
#[derive(Debug, Clone)]
pub struct PlanProposer {
    /// The AgentThread that proposed the plan, when a run proposed it.
    pub agent_thread_id: Option<CanonicalId>,
    /// The proposer's grant templates — the CapabilityProjection `grants` the plan must
    /// narrow. The model never supplies this field.
    pub capability_needs: Value,
}

/// Why a [`PlanProposal`] was rejected; each variant names its DOMAIN.md §15 code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRejection {
    /// The proposal carries no change at all.
    EmptyPlan,
    /// A node or edge names a workspace other than the proposal's.
    WorkspaceMismatch {
        /// The row that disagreed.
        row: String,
        /// The proposal's workspace.
        expected: String,
        /// The workspace the row names.
        found: String,
    },
    /// The proposal is larger than the effective policy bound.
    TooManyNodes {
        /// Nodes the proposal adds or updates.
        requested: usize,
        /// The effective `policies.max_plan_nodes` bound.
        limit: usize,
    },
    /// The resulting `depends_on`/`parent_of` structure would contain a cycle.
    Cycle {
        /// The edge that would close the cycle.
        from: String,
        /// The other endpoint.
        to: String,
    },
    /// An added node has neither its own CompletionContract nor a parent that provides one.
    MissingCompletionContract {
        /// The offending node (its title, since its id is not assigned yet).
        node: String,
    },
    /// A capability need is not a narrowing of the proposer's needs.
    CapabilityNotNarrowed {
        /// The rejected grant template, as canonical JSON.
        need: String,
    },
    /// `base_revision` is not the current workspace graph revision.
    StaleBaseRevision {
        /// The revision the proposal carried.
        expected: u64,
        /// The current graph revision.
        current: u64,
    },
}

impl PlanRejection {
    /// The DOMAIN.md §15 error code this rejection maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooManyNodes { .. } => "VALIDATION_BOUNDS",
            Self::StaleBaseRevision { .. } => "CONFLICT_REVISION",
            Self::CapabilityNotNarrowed { .. } => "CAPABILITY_DENIED",
            Self::EmptyPlan
            | Self::WorkspaceMismatch { .. }
            | Self::Cycle { .. }
            | Self::MissingCompletionContract { .. } => "VALIDATION_SCHEMA",
        }
    }
}

impl std::fmt::Display for PlanRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPlan => f.write_str("the proposal carries no change"),
            Self::WorkspaceMismatch {
                row,
                expected,
                found,
            } => write!(f, "{row} belongs to workspace {found}, not {expected}"),
            Self::TooManyNodes { requested, limit } => {
                write!(
                    f,
                    "the plan touches {requested} nodes, above the limit {limit}"
                )
            }
            Self::Cycle { from, to } => {
                write!(f, "the plan would close a cycle through {from} -> {to}")
            }
            Self::MissingCompletionContract { node } => write!(
                f,
                "node {node:?} has no CompletionContract and no parent that provides one"
            ),
            Self::CapabilityNotNarrowed { need } => {
                write!(
                    f,
                    "capability need {need} is not a narrowing of the proposer's"
                )
            }
            Self::StaleBaseRevision { expected, current } => write!(
                f,
                "base revision {expected} is not the current graph revision {current}"
            ),
        }
    }
}

/// The validation hook for a plan's capability needs.
///
/// [`StructuralPlanCapabilityCheck`] ships the rule that is provable from the templates
/// alone. RUN-005 implements this trait with the Capability Projection algebra (§6.3) and
/// is called in place of the structural rule, so no plan ever bypasses narrowing.
pub trait PlanCapabilityNarrowingCheck {
    /// Reject a plan whose needs are not a narrowing of the proposer's.
    ///
    /// # Errors
    /// Returns the [`PlanRejection`] naming the first need that does not narrow.
    fn check(&self, proposer: &PlanProposer, proposal: &PlanProposal) -> Result<(), PlanRejection>;
}

/// The structural narrowing rule shipped before RUN-005.
///
/// A plan need narrows a proposer need when, for every field the proposer declares, the
/// plan declares an equal or narrower value: numbers may decrease, arrays may shrink to a
/// subset, objects may add fields but not widen or drop the proposer's, and everything else
/// must be equal. Glob selectors are compared literally until the real algebra lands, so the
/// structural rule fails closed on a merely different spelling of a narrower selector.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuralPlanCapabilityCheck;

impl PlanCapabilityNarrowingCheck for StructuralPlanCapabilityCheck {
    fn check(&self, proposer: &PlanProposer, proposal: &PlanProposal) -> Result<(), PlanRejection> {
        let needed = needs(&proposal.capability_needs);
        for need in &needed {
            if !covered(need, &proposer.capability_needs) {
                return Err(PlanRejection::CapabilityNotNarrowed {
                    need: need.to_string(),
                });
            }
        }
        for node in &proposal.nodes_add {
            for need in needs(&node.capability_needs) {
                if !covered(&need, &proposer.capability_needs) {
                    return Err(PlanRejection::CapabilityNotNarrowed {
                        need: need.to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// The grant templates of a `capability_needs` value.
fn needs(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

/// Whether at least one proposer template covers `need`.
fn covered(need: &Value, proposer: &Value) -> bool {
    needs(proposer).iter().any(|grant| narrows(need, grant))
}

/// Structural narrowing of one grant template against one proposer template.
fn narrows(plan: &Value, proposer: &Value) -> bool {
    match (plan, proposer) {
        (Value::Number(plan), Value::Number(proposer)) => {
            match (plan.as_f64(), proposer.as_f64()) {
                (Some(plan), Some(proposer)) => plan <= proposer,
                _ => plan == proposer,
            }
        }
        (Value::Array(plan), Value::Array(proposer)) => plan
            .iter()
            .all(|item| proposer.iter().any(|allowed| narrows(item, allowed))),
        (Value::Object(plan), Value::Object(proposer)) => proposer
            .iter()
            .all(|(key, value)| plan.get(key).is_some_and(|planned| narrows(planned, value))),
        (plan, proposer) => plan == proposer,
    }
}

/// A validated proposal, ready to be applied as one batch.
pub(crate) struct ValidatedPlan {
    /// The batch to apply.
    pub(crate) batch: GraphBatch,
    /// The `work.plan_applied` payload.
    pub(crate) applied: PlanApplied,
}

/// The accepted proposal, as recorded by its `work.plan_applied` event.
#[derive(Debug, Clone)]
pub(crate) struct PlanApplied {
    /// Proposal identity.
    pub(crate) proposal_id: String,
    /// The originating Run.
    pub(crate) run_id: Option<CanonicalId>,
    /// The revision the proposer observed.
    pub(crate) base_revision: Revision,
    /// The proposer's rationale.
    pub(crate) rationale: Option<String>,
    /// Node additions.
    pub(crate) nodes_added: usize,
    /// Node updates.
    pub(crate) nodes_updated: usize,
    /// Edge additions.
    pub(crate) edges_added: usize,
    /// Edge removals.
    pub(crate) edges_removed: usize,
}

impl PlanApplied {
    /// The `work.plan_applied` payload (DOMAIN.md §4.5).
    #[must_use]
    pub(crate) fn payload(&self, revision: Revision) -> Value {
        serde_json::json!({
            "proposal_id": self.proposal_id,
            "run_id": self.run_id.as_ref().map(ToString::to_string),
            "base_revision": self.base_revision.get(),
            "revision": revision.get(),
            "nodes_added": self.nodes_added,
            "nodes_updated": self.nodes_updated,
            "edges_added": self.edges_added,
            "edges_removed": self.edges_removed,
            "rationale": self.rationale,
        })
    }
}

/// Validate a proposal and turn it into the batch that applies it.
///
/// # Errors
/// Returns [`GraphTransactionError::PlanRejected`] for every DOMAIN.md §4.5 rule the
/// proposal breaks, and a [`GraphTransactionError::Graph`] when the reads the validation
/// needs fail. Nothing is written on either path.
pub(crate) async fn validate_plan<C: PlanCapabilityNarrowingCheck>(
    graph: &GraphStore,
    check: &C,
    proposer: &PlanProposer,
    proposal: &PlanProposal,
) -> Result<ValidatedPlan, GraphTransactionError> {
    let reject = |rejection: PlanRejection| GraphTransactionError::PlanRejected {
        proposal_id: proposal.proposal_id.clone(),
        rejection,
    };

    if proposal.nodes_add.is_empty()
        && proposal.nodes_update.is_empty()
        && proposal.edges_add.is_empty()
        && proposal.edges_remove.is_empty()
    {
        return Err(reject(PlanRejection::EmptyPlan));
    }
    for node in &proposal.nodes_add {
        if node.workspace_id != proposal.workspace_id {
            return Err(reject(PlanRejection::WorkspaceMismatch {
                row: format!("added node {:?}", node.title),
                expected: proposal.workspace_id.clone(),
                found: node.workspace_id.clone(),
            }));
        }
    }
    for edge in &proposal.edges_add {
        if edge.workspace_id != proposal.workspace_id {
            return Err(reject(PlanRejection::WorkspaceMismatch {
                row: format!("added edge {}", edge.from_node_id),
                expected: proposal.workspace_id.clone(),
                found: edge.workspace_id.clone(),
            }));
        }
    }

    let limit = max_plan_nodes(graph, &proposal.workspace_id).await?;
    let requested = proposal.nodes_add.len() + proposal.nodes_update.len();
    if requested > limit {
        return Err(reject(PlanRejection::TooManyNodes { requested, limit }));
    }

    check.check(proposer, proposal).map_err(reject)?;

    let nodes = graph.list_nodes(&proposal.workspace_id).await?;
    let edges = graph.list_edges(&proposal.workspace_id).await?;

    // A node without its own CompletionContract must inherit one from its parent.
    for node in &proposal.nodes_add {
        if !has_completion_contract(&node.completion_contract) {
            let Some(parent_id) = &node.parent_id else {
                return Err(reject(PlanRejection::MissingCompletionContract {
                    node: node.title.clone(),
                }));
            };
            let inherits = nodes
                .iter()
                .find(|candidate| &candidate.id == parent_id)
                .is_some_and(|parent| has_completion_contract(&parent.completion_contract));
            if !inherits {
                return Err(reject(PlanRejection::MissingCompletionContract {
                    node: node.title.clone(),
                }));
            }
        }
    }

    let updates = resolve_updates(graph, proposal).await?;
    let removals = resolve_removals(graph, proposal).await?;

    if let Some((from, to)) = cycle(&nodes, &edges, proposal, &removals) {
        return Err(reject(PlanRejection::Cycle { from, to }));
    }

    let current = graph.graph_revision(&proposal.workspace_id).await?;
    if current != proposal.base_revision {
        return Err(reject(PlanRejection::StaleBaseRevision {
            expected: proposal.base_revision.get(),
            current: current.get(),
        }));
    }

    let mut batch = GraphBatch::new(&proposal.workspace_id, proposal.base_revision);
    for node in &proposal.nodes_add {
        let mut node = node.clone();
        node.workspace_id.clone_from(&proposal.workspace_id);
        node.origin = WorkOrigin::PlanProposal;
        batch.push(GraphChange::create_work_node(node));
    }
    for update in &updates {
        batch.push(GraphChange::TransitionWorkNode {
            node_id: update.node_id,
            expected: update.expected_revision,
            to: update.to,
        });
    }
    for edge in &proposal.edges_add {
        batch.push(GraphChange::create_work_edge(edge.clone()));
    }
    for removal in &removals {
        batch.push(GraphChange::remove_work_edge(
            removal.edge_id,
            removal.expected_revision,
        ));
    }

    let applied = PlanApplied {
        proposal_id: proposal.proposal_id.clone(),
        run_id: proposal.run_id,
        base_revision: proposal.base_revision,
        rationale: proposal.rationale.clone(),
        nodes_added: proposal.nodes_add.len(),
        nodes_updated: updates.len(),
        edges_added: proposal.edges_add.len(),
        edges_removed: removals.len(),
    };
    Ok(ValidatedPlan { batch, applied })
}

/// The effective `policies.max_plan_nodes` bound for a workspace.
///
/// A workspace policy may only narrow the tenant policy, so the bound is the smallest
/// `max_plan_nodes` across the tenant row and the workspace row; with no policy row the
/// conservative [`DEFAULT_MAX_PLAN_NODES`] applies.
async fn max_plan_nodes(
    graph: &GraphStore,
    workspace_id: &str,
) -> Result<usize, GraphTransactionError> {
    let mut tx = graph.pool().begin().await.map_err(GraphError::from)?;
    quansio_server::control::schema::set_tenant_context(&mut tx, graph.tenant_id())
        .await
        .map_err(GraphError::TenantScope)?;
    let limit: Option<i32> = sqlx::query_scalar(
        "SELECT MIN(max_plan_nodes) FROM policies WHERE tenant_id = $1 \
         AND ((scope = 'tenant' AND workspace_id IS NULL) OR workspace_id = $2)",
    )
    .bind(graph.tenant_id())
    .bind(workspace_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(GraphError::from)?;
    tx.commit().await.map_err(GraphError::from)?;
    let limit = limit.and_then(|value| usize::try_from(value).ok());
    Ok(limit.unwrap_or(DEFAULT_MAX_PLAN_NODES))
}

/// Whether a CompletionContract is present rather than empty.
fn has_completion_contract(contract: &Value) -> bool {
    match contract {
        Value::Null => false,
        Value::Object(object) => !object.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::String(value) => !value.is_empty(),
        Value::Number(_) | Value::Bool(_) => true,
    }
}

/// Verify every node update names a row of the proposal's workspace.
async fn resolve_updates(
    graph: &GraphStore,
    proposal: &PlanProposal,
) -> Result<Vec<PlanNodeUpdate>, GraphTransactionError> {
    for update in &proposal.nodes_update {
        let node = graph
            .get_node(&update.node_id)
            .await
            .map_err(GraphTransactionError::Graph)?;
        if node.workspace_id != proposal.workspace_id {
            return Err(GraphTransactionError::PlanRejected {
                proposal_id: proposal.proposal_id.clone(),
                rejection: PlanRejection::WorkspaceMismatch {
                    row: format!("node {}", update.node_id),
                    expected: proposal.workspace_id.clone(),
                    found: node.workspace_id,
                },
            });
        }
    }
    Ok(proposal.nodes_update.clone())
}

/// Verify every edge removal names a row of the proposal's workspace.
async fn resolve_removals(
    graph: &GraphStore,
    proposal: &PlanProposal,
) -> Result<Vec<PlanEdgeRemoval>, GraphTransactionError> {
    for removal in &proposal.edges_remove {
        let edge = graph
            .get_edge(&removal.edge_id)
            .await
            .map_err(GraphTransactionError::Graph)?;
        if edge.workspace_id != proposal.workspace_id {
            return Err(GraphTransactionError::PlanRejected {
                proposal_id: proposal.proposal_id.clone(),
                rejection: PlanRejection::WorkspaceMismatch {
                    row: format!("edge {}", removal.edge_id),
                    expected: proposal.workspace_id.clone(),
                    found: edge.workspace_id,
                },
            });
        }
    }
    Ok(proposal.edges_remove.clone())
}

/// Detect whether applying the proposal would close a `depends_on`/`parent_of` cycle.
///
/// The structure is the workspace's existing acyclic relations (edges plus
/// `work_nodes.parent_id`), with the proposal's edge additions applied and its removals
/// dropped. The store re-checks inside the transaction, so this is a precise early
/// rejection rather than the only guard.
fn cycle(
    nodes: &[WorkNode],
    edges: &[WorkEdge],
    proposal: &PlanProposal,
    removals: &[PlanEdgeRemoval],
) -> Option<(String, String)> {
    let removed: HashSet<String> = removals
        .iter()
        .map(|removal| removal.edge_id.to_string())
        .collect();
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for node in nodes {
        if let Some(parent) = &node.parent_id {
            adjacency
                .entry(node.id.to_string())
                .or_default()
                .push(parent.to_string());
        }
    }
    for edge in edges {
        if edge.kind.is_acyclic_relation() && !removed.contains(&edge.id.to_string()) {
            adjacency
                .entry(edge.from_node_id.to_string())
                .or_default()
                .push(edge.to_node_id.to_string());
        }
    }
    for edge in &proposal.edges_add {
        if edge.kind.is_acyclic_relation() {
            adjacency
                .entry(edge.from_node_id.to_string())
                .or_default()
                .push(edge.to_node_id.to_string());
        }
    }

    // Depth-first colouring: a grey node reached again is the back edge of a cycle.
    let mut state: HashMap<String, u8> = HashMap::new();
    let roots: Vec<String> = adjacency.keys().cloned().collect();
    for root in &roots {
        if let Some(found) = visit(root, &adjacency, &mut state) {
            return Some(found);
        }
    }
    None
}

/// One depth-first step of [`cycle`], returning the edge that closes a cycle.
fn visit(
    node: &str,
    adjacency: &HashMap<String, Vec<String>>,
    state: &mut HashMap<String, u8>,
) -> Option<(String, String)> {
    state.insert(node.to_string(), 1);
    if let Some(targets) = adjacency.get(node) {
        for target in targets {
            match state.get(target.as_str()).copied() {
                Some(1) => return Some((node.to_string(), target.clone())),
                Some(2) => continue,
                _ => {
                    if let Some(found) = visit(target, adjacency, state) {
                        return Some(found);
                    }
                }
            }
        }
    }
    state.insert(node.to_string(), 2);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn proposer(needs: Value) -> PlanProposer {
        PlanProposer {
            agent_thread_id: None,
            capability_needs: needs,
        }
    }

    fn proposal(needs: Value) -> PlanProposal {
        PlanProposal {
            proposal_id: "prop-1".to_string(),
            run_id: None,
            workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC".to_string(),
            base_revision: Revision::INITIAL,
            nodes_add: Vec::new(),
            nodes_update: Vec::new(),
            edges_add: Vec::new(),
            edges_remove: Vec::new(),
            rationale: None,
            capability_needs: needs,
        }
    }

    #[test]
    fn structural_narrowing_accepts_equal_and_rejects_wider_templates() {
        let granted = json!([{
            "effect_class": "fs.write.workspace",
            "resource": {"kind": "fs", "selector": "/work/**"},
            "constraints": {"max_tier": 1}
        }]);
        let equal = granted.clone();
        assert!(StructuralPlanCapabilityCheck
            .check(&proposer(granted.clone()), &proposal(equal))
            .is_ok());

        let narrower = json!([{
            "effect_class": "fs.write.workspace",
            "resource": {"kind": "fs", "selector": "/work/**"},
            "constraints": {"max_tier": 1}
        }]);
        assert!(StructuralPlanCapabilityCheck
            .check(&proposer(granted.clone()), &proposal(narrower))
            .is_ok());

        let wider_tier = json!([{
            "effect_class": "fs.write.workspace",
            "resource": {"kind": "fs", "selector": "/work/**"},
            "constraints": {"max_tier": 3}
        }]);
        let rejection = StructuralPlanCapabilityCheck
            .check(&proposer(granted.clone()), &proposal(wider_tier))
            .expect_err("max_tier may only narrow");
        assert_eq!(rejection.code(), "CAPABILITY_DENIED");

        let absent = json!([{
            "effect_class": "payment.execute",
            "resource": {"kind": "connector", "selector": "cnx_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"},
            "constraints": {}
        }]);
        let rejection = StructuralPlanCapabilityCheck
            .check(&proposer(granted), &proposal(absent))
            .expect_err("an ungranted effect class is not a narrowing");
        assert_eq!(rejection.code(), "CAPABILITY_DENIED");
    }

    #[test]
    fn an_empty_contract_is_absent_and_a_populated_one_is_present() {
        assert!(!has_completion_contract(&json!({})));
        assert!(!has_completion_contract(&Value::Null));
        assert!(has_completion_contract(&json!({
            "deterministic_checks": [{"kind": "artifact_exists"}]
        })));
    }
}
