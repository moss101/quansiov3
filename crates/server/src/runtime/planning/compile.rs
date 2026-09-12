//! Proposal compilation: bounds, identity, endpoints, acyclicity, CompletionContract
//! inheritance, capability narrowing and revision discipline (DOMAIN.md §4.5).
//!
//! [`compile`] is a pure function of the raw proposal, the graph snapshot, the proposer's
//! capability projection and the effective node bound. It writes nothing and calls nothing:
//! the ordering of [`CompiledPlan`]'s mutation lists, the recorded [`ContractSource`] of
//! every added node and the rejection it returns are all deterministic functions of those
//! inputs, which is what makes the accepted mutation set reproducible.

use std::collections::{HashMap, HashSet};

use quansio_capability::{check_narrowing_grants, CapabilityError, Grant};
use quansio_core::Revision;
use serde_json::Value;

use super::wire::{self, EdgeKind, NodeKind, NodeStatus, RawPlanProposal};
use super::PlanError;

/// Upper bound used when a tenant/workspace has no `policies` row.
///
/// It mirrors the `policies.max_plan_nodes` default in `0001_canonical_schema.sql` and
/// `quansio_graph`'s `DEFAULT_MAX_PLAN_NODES`, so a tenant without a policy row is bounded
/// exactly like one holding the default policy.
pub const DEFAULT_MAX_PLAN_NODES: usize = 25;

/// The WorkGraph as the proposer observed it (DOMAIN.md §4.1–§4.2).
///
/// The compiler needs the existing structure to validate edge endpoints, detect cycles
/// through `parent_id` chains and resolve CompletionContract inheritance. It is supplied
/// by the `PlanWorkspacePort` so `crates/server` never reads the graph tables directly.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanGraphSnapshot {
    /// Workspace the snapshot describes.
    pub workspace_id: String,
    /// The workspace graph revision the snapshot was taken at.
    pub revision: Revision,
    /// Existing nodes, in the store's deterministic order.
    pub nodes: Vec<SnapshotNode>,
    /// Existing edges, in the store's deterministic order.
    pub edges: Vec<SnapshotEdge>,
}

impl PlanGraphSnapshot {
    /// An empty workspace at `revision`.
    #[must_use]
    pub fn empty(workspace_id: impl Into<String>, revision: Revision) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            revision,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

/// One existing node as validation sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotNode {
    /// Canonical `wn_…` id.
    pub id: String,
    /// Existing parent, when the node has one.
    pub parent_id: Option<String>,
    /// The node's revision, for compare-and-set of updates.
    pub revision: u64,
    /// The node's CompletionContract.
    pub completion_contract: Value,
}

/// One existing edge as validation sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotEdge {
    /// Canonical `we_…` id.
    pub id: String,
    /// Source node.
    pub from_node_id: String,
    /// Target node.
    pub to_node_id: String,
    /// Edge kind.
    pub kind: EdgeKind,
    /// The edge's revision, for compare-and-set of removals.
    pub revision: u64,
}

/// Who proposed a plan and the authority it was proposed under (DOMAIN.md §4.5, §6.2).
#[derive(Debug, Clone)]
pub struct PlanProposerProjection {
    /// The AgentThread that proposed the plan, when a run proposed it.
    pub agent_thread_id: Option<String>,
    /// The proposer's CapabilityProjection grants — the set every plan need must narrow.
    /// The model never supplies this field.
    pub grants: Value,
    /// The `created_by {kind, id}` stamped on every node the plan creates.
    pub created_by: Value,
}

/// Where an added node's CompletionContract comes from (DOMAIN.md §4.4, §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractSource {
    /// The node carries its own non-empty contract.
    Own,
    /// The node inherits the contract of its parent.
    InheritedFrom {
        /// The parent whose contract the node inherits.
        parent_id: String,
        /// Whether that parent is also created by this plan.
        created_in_plan: bool,
    },
}

/// One node to create, resolved against the snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledNode {
    /// Proposal-local identity, used for duplicate detection and reference resolution.
    pub draft_id: String,
    /// Canonical node kind.
    pub kind: NodeKind,
    /// Title.
    pub title: String,
    /// Description.
    pub description: Option<String>,
    /// Resolved existing parent (a plan cannot reference a node it creates).
    pub parent_id: Option<String>,
    /// Requested status; only `draft` is applicable because the store creates nodes draft.
    pub status: NodeStatus,
    /// CompletionContract payload (`{}` when inherited).
    pub completion_contract: Value,
    /// Where the contract comes from.
    pub contract_source: ContractSource,
    /// Capability need templates.
    pub capability_needs: Value,
    /// Priority 0–3.
    pub priority: i16,
    /// Budget row reference.
    pub budget_id: Option<String>,
    /// Originating thread.
    pub thread_id: Option<String>,
}

/// One node status update.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledNodeUpdate {
    /// Existing node to transition.
    pub node_id: String,
    /// The revision the proposer observed.
    pub expected_revision: u64,
    /// The requested status.
    pub to: NodeStatus,
}

/// One edge to create between existing nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEdge {
    /// Source node.
    pub from_node_id: String,
    /// Target node.
    pub to_node_id: String,
    /// Edge kind.
    pub kind: EdgeKind,
}

/// One edge removal.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEdgeRemoval {
    /// Existing edge to remove.
    pub edge_id: String,
    /// The revision the proposer observed.
    pub expected_revision: u64,
}

/// A validated, deterministic mutation set ready to be applied as one graph transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledPlan {
    /// Proposal identity.
    pub proposal_id: String,
    /// Originating Run.
    pub run_id: Option<String>,
    /// Workspace the plan applies to.
    pub workspace_id: String,
    /// The revision the proposer observed; the compare-and-set key.
    pub base_revision: Revision,
    /// The proposer's rationale.
    pub rationale: String,
    /// Nodes to create, in proposal order.
    pub nodes_add: Vec<CompiledNode>,
    /// Node transitions, in proposal order.
    pub nodes_update: Vec<CompiledNodeUpdate>,
    /// Edges to create, in proposal order.
    pub edges_add: Vec<CompiledEdge>,
    /// Edges to remove, in proposal order.
    pub edges_remove: Vec<CompiledEdgeRemoval>,
    /// The proposer's grant templates, unmodified.
    pub capability_needs: Value,
    /// The proposer AgentThread, when a run proposed the plan.
    pub proposer_agent_thread_id: Option<String>,
    /// The proposer's CapabilityProjection grants the plan was checked against.
    pub proposer_grants: Value,
    /// `created_by` stamped on every created node.
    pub created_by: Value,
}

impl CompiledPlan {
    /// A deterministic, ordered one-line description of every mutation.
    ///
    /// Two compilations of the same proposal against the same snapshot produce identical
    /// summaries; the acceptance determinism fixture asserts exactly that.
    #[must_use]
    pub fn mutation_summary(&self) -> Vec<String> {
        let mut summary = Vec::new();
        for node in &self.nodes_add {
            let source = match &node.contract_source {
                ContractSource::Own => "own".to_string(),
                ContractSource::InheritedFrom {
                    parent_id,
                    created_in_plan,
                } => format!("inherit:{parent_id}:in_plan={created_in_plan}"),
            };
            summary.push(format!(
                "create {} kind={} status={} parent={} contract={source}",
                node.draft_id,
                node.kind.as_str(),
                node.status.as_str(),
                node.parent_id.as_deref().unwrap_or("-"),
            ));
        }
        for update in &self.nodes_update {
            summary.push(format!(
                "transition {} expected={} to={}",
                update.node_id,
                update.expected_revision,
                update.to.as_str()
            ));
        }
        for edge in &self.edges_add {
            summary.push(format!(
                "edge {} -> {} kind={}",
                edge.from_node_id,
                edge.to_node_id,
                edge.kind.as_str()
            ));
        }
        for removal in &self.edges_remove {
            summary.push(format!(
                "remove {} expected={}",
                removal.edge_id, removal.expected_revision
            ));
        }
        summary
    }

    /// The number of graph changes the plan carries (excluding the plan event).
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.nodes_add.len()
            + self.nodes_update.len()
            + self.edges_add.len()
            + self.edges_remove.len()
    }
}

/// Compile a raw model proposal into a validated mutation set (DOMAIN.md §4.5).
///
/// Nothing is written on either path: a rejection is returned without touching the graph,
/// and the compiled plan describes the mutation the port will commit atomically.
///
/// # Errors
/// Returns the [`PlanError`] naming the first DOMAIN.md §4.5 rule the proposal breaks.
pub fn compile(
    raw: &RawPlanProposal,
    snapshot: &PlanGraphSnapshot,
    proposer: &PlanProposerProjection,
    limit: usize,
) -> Result<CompiledPlan, PlanError> {
    wire::validate_raw(raw)?;

    if snapshot.workspace_id != raw.workspace_id {
        return Err(PlanError::Workspace {
            detail: format!(
                "the snapshot is for workspace {} but the proposal names {}",
                snapshot.workspace_id, raw.workspace_id
            ),
        });
    }

    if raw.nodes_add.is_empty()
        && raw.nodes_update.is_empty()
        && raw.edges_add.is_empty()
        && raw.edges_remove.is_empty()
    {
        return Err(PlanError::EmptyPlan);
    }

    let existing_nodes: HashMap<&str, &SnapshotNode> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let existing_edges: HashMap<&str, &SnapshotEdge> = snapshot
        .edges
        .iter()
        .map(|edge| (edge.id.as_str(), edge))
        .collect();

    let proposed_nodes = unique_node_ids(raw, &existing_nodes)?;
    unique_edge_ids(raw, &existing_edges)?;

    let requested = raw.nodes_add.len() + raw.nodes_update.len();
    if requested > limit {
        return Err(PlanError::Bounds { requested, limit });
    }

    let updates = resolve_updates(raw, &existing_nodes)?;
    let removals = resolve_removals(raw, &existing_edges)?;

    check_endpoints(raw, &proposed_nodes, &existing_nodes)?;
    let nodes_add = resolve_nodes(raw, &existing_nodes)?;
    detect_cycle(snapshot, raw, &removals)?;
    check_applicable_references(raw, &proposed_nodes)?;
    check_capability(raw, proposer, &nodes_add)?;

    let base_revision = Revision::new(raw.base_revision);
    if base_revision != snapshot.revision {
        return Err(PlanError::StaleBaseRevision {
            expected: raw.base_revision,
            current: snapshot.revision.get(),
        });
    }

    Ok(CompiledPlan {
        proposal_id: raw.proposal_id.clone(),
        run_id: raw.run_id.clone(),
        workspace_id: raw.workspace_id.clone(),
        base_revision,
        rationale: raw.rationale.clone().unwrap_or_default(),
        nodes_add,
        nodes_update: updates,
        edges_add: raw
            .edges_add
            .iter()
            .map(|edge| CompiledEdge {
                from_node_id: edge.from_node_id.clone(),
                to_node_id: edge.to_node_id.clone(),
                kind: EdgeKind::parse(&edge.kind).expect("validated by validate_raw"),
            })
            .collect(),
        edges_remove: removals,
        capability_needs: normalize_needs(&raw.capability_needs),
        proposer_agent_thread_id: proposer.agent_thread_id.clone(),
        proposer_grants: proposer.grants.clone(),
        created_by: proposer.created_by.clone(),
    })
}

/// The proposal-local node ids, rejecting duplicates and collisions with existing nodes.
fn unique_node_ids<'a>(
    raw: &'a RawPlanProposal,
    existing: &HashMap<&str, &SnapshotNode>,
) -> Result<HashSet<&'a str>, PlanError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for node in &raw.nodes_add {
        if !seen.insert(node.id.as_str()) {
            return Err(PlanError::schema(format!(
                "duplicate node id {:?} in nodes_add",
                node.id
            )));
        }
        if existing.contains_key(node.id.as_str()) {
            return Err(PlanError::schema(format!(
                "node id {:?} already exists in the workspace",
                node.id
            )));
        }
    }
    Ok(seen)
}

/// Reject duplicate edge ids and collisions with existing edges.
fn unique_edge_ids(
    raw: &RawPlanProposal,
    existing: &HashMap<&str, &SnapshotEdge>,
) -> Result<(), PlanError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for edge in &raw.edges_add {
        if !seen.insert(edge.id.as_str()) {
            return Err(PlanError::schema(format!(
                "duplicate edge id {:?} in edges_add",
                edge.id
            )));
        }
        if existing.contains_key(edge.id.as_str()) {
            return Err(PlanError::schema(format!(
                "edge id {:?} already exists in the workspace",
                edge.id
            )));
        }
    }
    Ok(())
}

/// Resolve node updates against the snapshot.
fn resolve_updates(
    raw: &RawPlanProposal,
    existing: &HashMap<&str, &SnapshotNode>,
) -> Result<Vec<CompiledNodeUpdate>, PlanError> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut updates = Vec::with_capacity(raw.nodes_update.len());
    for update in &raw.nodes_update {
        if !seen.insert(update.node_id.as_str()) {
            return Err(PlanError::schema(format!(
                "node {:?} is updated more than once",
                update.node_id
            )));
        }
        let Some(node) = existing.get(update.node_id.as_str()) else {
            return Err(PlanError::schema(format!(
                "node update references unknown node {:?}",
                update.node_id
            )));
        };
        if node.revision != update.expected_revision {
            return Err(PlanError::StaleBaseRevision {
                expected: update.expected_revision,
                current: node.revision,
            });
        }
        updates.push(CompiledNodeUpdate {
            node_id: update.node_id.clone(),
            expected_revision: update.expected_revision,
            to: NodeStatus::parse(&update.to)?,
        });
    }
    Ok(updates)
}

/// Resolve edge removals against the snapshot.
fn resolve_removals(
    raw: &RawPlanProposal,
    existing: &HashMap<&str, &SnapshotEdge>,
) -> Result<Vec<CompiledEdgeRemoval>, PlanError> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut removals = Vec::with_capacity(raw.edges_remove.len());
    for removal in &raw.edges_remove {
        if !seen.insert(removal.edge_id.as_str()) {
            return Err(PlanError::schema(format!(
                "edge {:?} is removed more than once",
                removal.edge_id
            )));
        }
        let Some(edge) = existing.get(removal.edge_id.as_str()) else {
            return Err(PlanError::schema(format!(
                "edge removal references unknown edge {:?}",
                removal.edge_id
            )));
        };
        if edge.revision != removal.expected_revision {
            return Err(PlanError::StaleBaseRevision {
                expected: removal.expected_revision,
                current: edge.revision,
            });
        }
        removals.push(CompiledEdgeRemoval {
            edge_id: removal.edge_id.clone(),
            expected_revision: removal.expected_revision,
        });
    }
    Ok(removals)
}

/// Every `parent_id` and added-edge endpoint must name a known node.
fn check_endpoints(
    raw: &RawPlanProposal,
    proposed: &HashSet<&str>,
    existing: &HashMap<&str, &SnapshotNode>,
) -> Result<(), PlanError> {
    let known = |id: &str| proposed.contains(id) || existing.contains_key(id);
    for node in &raw.nodes_add {
        let Some(parent) = &node.parent_id else {
            continue;
        };
        if !known(parent) {
            return Err(PlanError::schema(format!(
                "node {:?} has unknown parent {parent:?}",
                node.id
            )));
        }
    }
    for edge in &raw.edges_add {
        for endpoint in [&edge.from_node_id, &edge.to_node_id] {
            if !known(endpoint) {
                return Err(PlanError::schema(format!(
                    "edge {:?} references unknown node {endpoint:?}",
                    edge.id
                )));
            }
        }
    }
    Ok(())
}

/// Resolve every added node, recording where its CompletionContract comes from.
fn resolve_nodes(
    raw: &RawPlanProposal,
    existing: &HashMap<&str, &SnapshotNode>,
) -> Result<Vec<CompiledNode>, PlanError> {
    let drafts: HashMap<&str, &wire::RawNode> = raw
        .nodes_add
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut nodes = Vec::with_capacity(raw.nodes_add.len());
    for node in &raw.nodes_add {
        let contract_source = if has_contract(&node.completion_contract) {
            ContractSource::Own
        } else if let Some(parent_id) = &node.parent_id {
            match drafts.get(parent_id.as_str()) {
                Some(parent) if has_contract(&parent.completion_contract) => {
                    ContractSource::InheritedFrom {
                        parent_id: parent_id.clone(),
                        created_in_plan: true,
                    }
                }
                Some(_) => {
                    return Err(PlanError::MissingCompletionContract {
                        node: node.title.clone(),
                    })
                }
                None => match existing.get(parent_id.as_str()) {
                    Some(parent) if has_contract(&parent.completion_contract) => {
                        ContractSource::InheritedFrom {
                            parent_id: parent_id.clone(),
                            created_in_plan: false,
                        }
                    }
                    _ => {
                        return Err(PlanError::MissingCompletionContract {
                            node: node.title.clone(),
                        })
                    }
                },
            }
        } else {
            return Err(PlanError::MissingCompletionContract {
                node: node.title.clone(),
            });
        };
        nodes.push(CompiledNode {
            draft_id: node.id.clone(),
            kind: NodeKind::parse(&node.kind)?,
            title: node.title.clone(),
            description: node.description.clone(),
            parent_id: node.parent_id.clone(),
            status: match &node.status {
                Some(status) => NodeStatus::parse(status)?,
                None => NodeStatus::Draft,
            },
            completion_contract: node.completion_contract.clone(),
            contract_source,
            capability_needs: normalize_needs(&node.capability_needs),
            priority: node.priority.unwrap_or(1),
            budget_id: node.budget_id.clone(),
            thread_id: node.thread_id.clone(),
        });
    }
    Ok(nodes)
}

/// Detect whether applying the proposal would close an acyclic-relation cycle.
///
/// The structure is the workspace's existing `depends_on`/`parent_of` edges plus every
/// `work_nodes.parent_id`, with the proposal's removals dropped and its additions applied.
/// Depth-first colouring reports the edge that would close the cycle.
fn detect_cycle(
    snapshot: &PlanGraphSnapshot,
    raw: &RawPlanProposal,
    removals: &[CompiledEdgeRemoval],
) -> Result<(), PlanError> {
    let removed: HashSet<&str> = removals
        .iter()
        .map(|removal| removal.edge_id.as_str())
        .collect();
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for node in &snapshot.nodes {
        if let Some(parent) = &node.parent_id {
            adjacency
                .entry(node.id.clone())
                .or_default()
                .push(parent.clone());
        }
    }
    for edge in &snapshot.edges {
        if edge.kind.is_acyclic_relation() && !removed.contains(edge.id.as_str()) {
            adjacency
                .entry(edge.from_node_id.clone())
                .or_default()
                .push(edge.to_node_id.clone());
        }
    }
    for edge in &raw.edges_add {
        if EdgeKind::parse(&edge.kind)?.is_acyclic_relation() {
            adjacency
                .entry(edge.from_node_id.clone())
                .or_default()
                .push(edge.to_node_id.clone());
        }
    }
    for node in &raw.nodes_add {
        if let Some(parent) = &node.parent_id {
            adjacency
                .entry(node.id.clone())
                .or_default()
                .push(parent.clone());
        }
    }

    let mut state: HashMap<&str, u8> = HashMap::new();
    let roots: Vec<&str> = adjacency.keys().map(String::as_str).collect();
    for root in roots {
        if let Some((from, to)) = visit(root, &adjacency, &mut state) {
            return Err(PlanError::Cycle { from, to });
        }
    }
    Ok(())
}

/// One depth-first step of [`detect_cycle`], returning the edge that closes a cycle.
fn visit<'a>(
    node: &'a str,
    adjacency: &'a HashMap<String, Vec<String>>,
    state: &mut HashMap<&'a str, u8>,
) -> Option<(String, String)> {
    state.insert(node, 1);
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
    state.insert(node, 2);
    None
}

/// Reject references a single `GraphTransaction` cannot express.
///
/// The canonical graph store assigns a new node's canonical id inside the transaction and
/// offers no alias for it, so a plan cannot name a node it creates in the same batch. Every
/// intra-plan reference is still validated above (endpoint existence, cycles, inheritance);
/// here the compiler refuses to compile one rather than emit a mutation the store cannot
/// apply.
fn check_applicable_references(
    raw: &RawPlanProposal,
    proposed: &HashSet<&str>,
) -> Result<(), PlanError> {
    for node in &raw.nodes_add {
        if node
            .status
            .as_deref()
            .is_some_and(|status| status != "draft")
        {
            return Err(PlanError::schema(format!(
                "node {:?} requests status {}; a created node starts draft and a plan cannot \
                 transition a node whose id is generated inside the transaction",
                node.id,
                node.status.as_deref().unwrap_or("draft")
            )));
        }
        let Some(parent) = &node.parent_id else {
            continue;
        };
        if proposed.contains(parent.as_str()) {
            return Err(PlanError::schema(format!(
                "node {:?} references planned parent {parent:?}; a plan cannot reference a \
                 node created in the same transaction",
                node.id
            )));
        }
    }
    for edge in &raw.edges_add {
        if proposed.contains(edge.from_node_id.as_str())
            || proposed.contains(edge.to_node_id.as_str())
        {
            return Err(PlanError::schema(format!(
                "edge {:?} references a node created in the same transaction",
                edge.id
            )));
        }
    }
    Ok(())
}

/// Reject a plan whose capability needs are not a narrowing of the proposer's projection.
fn check_capability(
    raw: &RawPlanProposal,
    proposer: &PlanProposerProjection,
    nodes: &[CompiledNode],
) -> Result<(), PlanError> {
    let held = parse_grants(&proposer.grants, "proposer projection").map_err(|error| {
        PlanError::Workspace {
            detail: format!("the proposer capability projection is not canonical: {error}"),
        }
    })?;
    let mut needed = parse_grants(&raw.capability_needs, "capability_needs")?;
    for node in nodes {
        needed.extend(parse_grants(
            &node.capability_needs,
            "node capability_needs",
        )?);
    }
    match check_narrowing_grants(&held, &needed) {
        Ok(()) => Ok(()),
        Err(CapabilityError::WideningRejected(rejection)) => {
            Err(PlanError::CapabilityNotNarrowed {
                need: rejection.grant.to_string(),
            })
        }
        Err(other) => Err(PlanError::Workspace {
            detail: format!("capability narrowing check failed: {other}"),
        }),
    }
}

/// Parse a `capability_needs` value into canonical grants.
fn parse_grants(value: &Value, what: &str) -> Result<Vec<Grant>, PlanError> {
    grant_templates(value)
        .iter()
        .map(|template| {
            Grant::from_json(template).map_err(|error| {
                PlanError::schema(format!("{what} contains a non-canonical grant: {error}"))
            })
        })
        .collect()
}

/// The grant templates of a `capability_needs` value.
fn grant_templates(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

/// Normalise `capability_needs` to an array so the compiled plan is shape-stable.
fn normalize_needs(value: &Value) -> Value {
    Value::Array(grant_templates(value))
}

/// Whether a CompletionContract is present rather than empty.
fn has_contract(contract: &Value) -> bool {
    match contract {
        Value::Null => false,
        Value::Object(object) => !object.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::String(value) => !value.is_empty(),
        Value::Number(_) | Value::Bool(_) => true,
    }
}
