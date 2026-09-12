//! Raw model proposal parsing (DOMAIN.md §4.5, §4.1–§4.2).
//!
//! The model emits JSON shaped like a `PlanProposal`. This module is the *only* place
//! that reads that untrusted shape: it deserializes into raw structs, then validates the
//! canonical `kind`, `status` and edge-`kind` value sets before the compiler ever looks at
//! the graph. A value outside DOMAIN.md §4.1–§4.2 is rejected with `VALIDATION_SCHEMA`
//! rather than passed on, so an unknown state can never reach a store.

use serde::Deserialize;
use serde_json::Value;

use super::PlanError;

/// Raw proposal, as emitted by a model.
#[derive(Debug, Clone, Deserialize)]
pub struct RawPlanProposal {
    /// Identity of the proposal, for audit and replay reporting.
    pub proposal_id: String,
    /// The Run the proposal came from.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Workspace the proposal applies to.
    pub workspace_id: String,
    /// The graph revision the proposer observed; the compare-and-set key.
    pub base_revision: u64,
    /// Nodes to create.
    #[serde(default)]
    pub nodes_add: Vec<RawNode>,
    /// Node status updates.
    #[serde(default)]
    pub nodes_update: Vec<RawNodeUpdate>,
    /// Edges to create.
    #[serde(default)]
    pub edges_add: Vec<RawEdge>,
    /// Edges to remove.
    #[serde(default)]
    pub edges_remove: Vec<RawEdgeRemoval>,
    /// The proposer's rationale; required and non-empty.
    #[serde(default)]
    pub rationale: Option<String>,
    /// Grant templates the plan needs (§6.1); must narrow the proposer's projection.
    #[serde(default)]
    pub capability_needs: Value,
}

/// One proposed node.
#[derive(Debug, Clone, Deserialize)]
pub struct RawNode {
    /// Proposal-local identity used by edges and `parent_id` references.
    pub id: String,
    /// DOMAIN.md §4.1 node kind.
    pub kind: String,
    /// Human-readable title.
    pub title: String,
    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
    /// Parent node; a proposal-local id or an existing `wn_…` id.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// DOMAIN.md §4.1 status; defaults to `draft`.
    #[serde(default)]
    pub status: Option<String>,
    /// CompletionContract (§4.4); empty means the node inherits its parent's.
    #[serde(default)]
    pub completion_contract: Value,
    /// Capability need templates (§6.1).
    #[serde(default)]
    pub capability_needs: Value,
    /// Priority 0–3; defaults to 1.
    #[serde(default)]
    pub priority: Option<i16>,
    /// Budget row reference (§13.2).
    #[serde(default)]
    pub budget_id: Option<String>,
    /// Thread the node originates from.
    #[serde(default)]
    pub thread_id: Option<String>,
}

/// One proposed node status update.
#[derive(Debug, Clone, Deserialize)]
pub struct RawNodeUpdate {
    /// The existing node to transition.
    pub node_id: String,
    /// The revision the proposer observed.
    pub expected_revision: u64,
    /// The requested DOMAIN.md §4.1 status.
    pub to: String,
}

/// One proposed edge.
#[derive(Debug, Clone, Deserialize)]
pub struct RawEdge {
    /// Proposal-local edge identity.
    pub id: String,
    /// Source node; a proposal-local id or an existing `wn_…` id.
    #[serde(alias = "from")]
    pub from_node_id: String,
    /// Target node; a proposal-local id or an existing `wn_…` id.
    #[serde(alias = "to")]
    pub to_node_id: String,
    /// DOMAIN.md §4.2 edge kind.
    pub kind: String,
}

/// One proposed edge removal.
#[derive(Debug, Clone, Deserialize)]
pub struct RawEdgeRemoval {
    /// The existing edge to remove.
    pub edge_id: String,
    /// The revision the proposer observed.
    pub expected_revision: u64,
}

/// Parse a raw model proposal into its wire shape.
///
/// # Errors
/// Returns [`PlanError::Schema`] when the value is not a JSON object matching the
/// DOMAIN.md §4.5 `PlanProposal` shape.
pub fn parse_raw(value: &Value) -> Result<RawPlanProposal, PlanError> {
    serde_json::from_value(value.clone())
        .map_err(|error| PlanError::schema(format!("proposal is not a PlanProposal: {error}")))
}

/// Validate a raw proposal's identity fields and canonical value sets.
///
/// # Errors
/// Returns [`PlanError::Schema`] when `proposal_id`/`workspace_id` are empty, the
/// rationale is missing or blank, or a `kind`, `status` or edge `kind` is not a
/// DOMAIN.md §4.1–§4.2 value.
pub(crate) fn validate_raw(raw: &RawPlanProposal) -> Result<(), PlanError> {
    if raw.proposal_id.trim().is_empty() {
        return Err(PlanError::schema("proposal_id is required"));
    }
    if raw.workspace_id.trim().is_empty() {
        return Err(PlanError::schema("workspace_id is required"));
    }
    if raw.rationale.as_deref().is_none_or(str::is_empty) {
        return Err(PlanError::schema("rationale is required"));
    }
    for node in &raw.nodes_add {
        NodeKind::parse(&node.kind)?;
        if let Some(status) = &node.status {
            NodeStatus::parse(status)?;
        }
        if node.id.trim().is_empty() {
            return Err(PlanError::schema("a node id must not be empty"));
        }
        if node.title.trim().is_empty() {
            return Err(PlanError::schema(format!(
                "node {:?} must have a title",
                node.id
            )));
        }
        if let Some(priority) = node.priority {
            if !(0..=3).contains(&priority) {
                return Err(PlanError::schema(format!(
                    "node {:?} priority {priority} is outside 0..=3",
                    node.id
                )));
            }
        }
    }
    for update in &raw.nodes_update {
        NodeStatus::parse(&update.to)?;
    }
    for edge in &raw.edges_add {
        EdgeKind::parse(&edge.kind)?;
    }
    Ok(())
}

/// DOMAIN.md §4.1 WorkNode kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// Root long-running goal.
    Objective,
    /// Unit of work under an objective.
    Task,
    /// Unit of work under a task.
    Subtask,
    /// Node that waits on an external condition.
    Wait,
    /// Marker node for a completion point.
    Milestone,
}

impl NodeKind {
    /// Every kind, in DOMAIN.md §4.1 order.
    pub const ALL: [Self; 5] = [
        Self::Objective,
        Self::Task,
        Self::Subtask,
        Self::Wait,
        Self::Milestone,
    ];

    /// The canonical stored form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Objective => "objective",
            Self::Task => "task",
            Self::Subtask => "subtask",
            Self::Wait => "wait",
            Self::Milestone => "milestone",
        }
    }

    /// Parse a canonical value.
    ///
    /// # Errors
    /// Returns [`PlanError::Schema`] for a value outside DOMAIN.md §4.1.
    pub fn parse(value: &str) -> Result<Self, PlanError> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
            .ok_or_else(|| PlanError::schema(format!("unknown WorkNode kind {value:?}")))
    }
}

/// DOMAIN.md §4.1 WorkNode status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeStatus {
    /// Not yet planned.
    Draft,
    /// Planned and dispatchable.
    Ready,
    /// Waiting on a dependency or a blocker.
    Blocked,
    /// A Run is executing the node.
    InProgress,
    /// Waiting for a question, timer, approval or child result.
    Waiting,
    /// CompletionContract verification is running.
    Verifying,
    /// Verified complete.
    Done,
    /// Ended unsuccessfully.
    Failed,
    /// Cancelled before completion.
    Cancelled,
}

impl NodeStatus {
    /// Every status, in DOMAIN.md §4.1 order.
    pub const ALL: [Self; 9] = [
        Self::Draft,
        Self::Ready,
        Self::Blocked,
        Self::InProgress,
        Self::Waiting,
        Self::Verifying,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
    ];

    /// The canonical stored form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::InProgress => "in_progress",
            Self::Waiting => "waiting",
            Self::Verifying => "verifying",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a canonical value.
    ///
    /// # Errors
    /// Returns [`PlanError::Schema`] for a value outside DOMAIN.md §4.1.
    pub fn parse(value: &str) -> Result<Self, PlanError> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.as_str() == value)
            .ok_or_else(|| PlanError::schema(format!("unknown WorkNode status {value:?}")))
    }
}

/// DOMAIN.md §4.2 WorkEdge kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// `from` depends on `to`; must stay acyclic.
    DependsOn,
    /// `from` is the parent of `to`; must stay acyclic.
    ParentOf,
    /// `from` produces an artifact associated with `to`.
    ProducesArtifact,
    /// `from` is verified by `to`.
    VerifiedBy,
    /// `from` is blocked by `to`.
    BlockedBy,
}

impl EdgeKind {
    /// Every kind, in DOMAIN.md §4.2 order.
    pub const ALL: [Self; 5] = [
        Self::DependsOn,
        Self::ParentOf,
        Self::ProducesArtifact,
        Self::VerifiedBy,
        Self::BlockedBy,
    ];

    /// The canonical stored form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DependsOn => "depends_on",
            Self::ParentOf => "parent_of",
            Self::ProducesArtifact => "produces_artifact",
            Self::VerifiedBy => "verified_by",
            Self::BlockedBy => "blocked_by",
        }
    }

    /// Parse a canonical value.
    ///
    /// # Errors
    /// Returns [`PlanError::Schema`] for a value outside DOMAIN.md §4.2.
    pub fn parse(value: &str) -> Result<Self, PlanError> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
            .ok_or_else(|| PlanError::schema(format!("unknown WorkEdge kind {value:?}")))
    }

    /// Whether this kind must remain acyclic (DOMAIN.md §4.2).
    #[must_use]
    pub const fn is_acyclic_relation(self) -> bool {
        matches!(self, Self::DependsOn | Self::ParentOf)
    }
}
