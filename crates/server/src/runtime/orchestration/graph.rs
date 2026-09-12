//! The WorkGraph view orchestration selects from, and the port it releases through.
//!
//! `quansio-graph` depends on `quansio-server` for `control::schema`, so `crates/server`
//! cannot depend on `quansio_graph` without a package cycle (the workspace conformance gate
//! forbids one). Orchestration therefore composes the WorkGraph through this port — the same
//! seam pattern as `runtime::planning::PlanWorkspacePort` and `scheduler::RunDispatch`: the
//! production implementation applies every transition through `crates/graph`'s event-emitting
//! `GraphTransaction`, and the default [`UnavailableWorkGraph`] fails closed naming the owner
//! instead of inventing graph state.

use async_trait::async_trait;

use super::OrchestrationError;

/// The task that owns the canonical, event-emitting graph transaction.
pub const GRAPH_TRANSACTION_OWNER: &str = "CORE-005";

/// The Run a node is currently executing, when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRef {
    /// `run_…` identity.
    pub run_id: String,
    /// Generation the run is fenced by.
    pub generation: u64,
    /// Current Run status (DOMAIN.md §5.2).
    pub status: String,
}

/// One WorkNode as orchestration observes it (DOMAIN.md §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkNodeView {
    /// `wn_…` identity.
    pub id: String,
    /// Node kind (`objective`, `task`, `subtask`, `wait`, `milestone`).
    pub kind: String,
    /// Current status (DOMAIN.md §4.1).
    pub status: String,
    /// Parent node, the ancestor relation cancellation propagates along.
    pub parent_id: Option<String>,
    /// Priority 0–3; higher runs first.
    pub priority: i16,
    /// Aggregate revision, the compare-and-set guard for a transition.
    pub revision: u64,
    /// The node's Run, when the command path created one.
    pub run: Option<RunRef>,
}

impl WorkNodeView {
    /// Whether the node has already finished.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        super::is_terminal_node_status(&self.status)
    }

    /// Whether the node's Run is parked or suspended, so it holds no execution slot and must
    /// not be dispatched again.
    #[must_use]
    pub fn is_parked(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.status.starts_with("WAITING_") || run.status == "SUSPENDED")
    }

    /// Whether the node's run is occupying an execution slot right now.
    ///
    /// Only *executing* work holds a slot: a `QUEUED` run has not been admitted yet, and a
    /// parked (`WAITING_*`) or `SUSPENDED` run has released its slot. Counting either as
    /// in-flight would make the gate starve itself — the admitted work would consume the
    /// headroom that its own dispatch needs.
    #[must_use]
    pub fn holds_slot(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| matches!(run.status.as_str(), "RUNNING" | "VERIFYING"))
    }
}

/// A WorkGraph edge (DOMAIN.md §4.2). `depends_on` reads `from` depends on `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyEdge {
    /// The dependent endpoint.
    pub from_node_id: String,
    /// The prerequisite endpoint (for `depends_on`).
    pub to_node_id: String,
    /// Edge kind (`depends_on`, `parent_of`, …).
    pub kind: String,
}

/// The workspace's nodes and edges as orchestration observed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSnapshot {
    /// Owning workspace.
    pub workspace_id: String,
    /// Graph revision the snapshot was read at.
    pub revision: u64,
    /// Every node in the workspace.
    pub nodes: Vec<WorkNodeView>,
    /// Every edge in the workspace.
    pub edges: Vec<DependencyEdge>,
}

impl GraphSnapshot {
    /// Find a node by identity.
    #[must_use]
    pub fn node(&self, node_id: &str) -> Option<&WorkNodeView> {
        self.nodes.iter().find(|node| node.id == node_id)
    }

    /// The prerequisites of a node: every `to` of an outgoing `depends_on` edge.
    #[must_use]
    pub fn prerequisites<'a>(&'a self, node_id: &str) -> Vec<&'a str> {
        self.edges
            .iter()
            .filter(|edge| edge.kind == "depends_on" && edge.from_node_id == node_id)
            .map(|edge| edge.to_node_id.as_str())
            .collect()
    }

    /// The nodes that depend on `node_id`.
    #[must_use]
    pub fn dependents<'a>(&'a self, node_id: &str) -> Vec<&'a str> {
        self.edges
            .iter()
            .filter(|edge| edge.kind == "depends_on" && edge.to_node_id == node_id)
            .map(|edge| edge.from_node_id.as_str())
            .collect()
    }

    /// The direct children of a node, by the unambiguous `work_nodes.parent_id` column.
    #[must_use]
    pub fn children<'a>(&'a self, node_id: &str) -> Vec<&'a str> {
        self.nodes
            .iter()
            .filter(|node| node.parent_id.as_deref() == Some(node_id))
            .map(|node| node.id.as_str())
            .collect()
    }

    /// `node_id` plus every descendant reachable through `parent_id`.
    #[must_use]
    pub fn subtree<'a>(&'a self, node_id: &'a str) -> Vec<&'a str> {
        let mut subtree = vec![node_id];
        let mut frontier = vec![node_id.to_string()];
        while let Some(current) = frontier.pop() {
            for child in self.children(&current) {
                if !subtree.contains(&child) {
                    subtree.push(child);
                    frontier.push(child.to_string());
                }
            }
        }
        subtree
    }

    /// Nodes that hold an execution slot right now (DOMAIN.md §13.2 `concurrency`).
    #[must_use]
    pub fn active_slots(&self) -> usize {
        self.nodes.iter().filter(|node| node.holds_slot()).count()
    }

    /// Whether a node's prerequisites are all `done`.
    #[must_use]
    pub fn prerequisites_satisfied(&self, node_id: &str) -> bool {
        self.prerequisites(node_id).iter().all(|prerequisite| {
            self.node(prerequisite)
                .is_some_and(|node| node.status == super::status::DONE)
        })
    }

    /// The first unsatisfied prerequisite of a node, if any.
    #[must_use]
    pub fn unsatisfied_prerequisite(&self, node_id: &str) -> Option<(String, String)> {
        for prerequisite in self.prerequisites(node_id) {
            match self.node(prerequisite) {
                Some(node) if node.status == super::status::DONE => {}
                Some(node) => return Some((node.id.clone(), node.status.clone())),
                None => return Some((prerequisite.to_string(), "missing".to_string())),
            }
        }
        None
    }
}

/// One compare-and-set node transition to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeTransition {
    /// Node to transition.
    pub node_id: String,
    /// Revision the caller observed.
    pub expected_revision: u64,
    /// Target status.
    pub to: String,
    /// Why, recorded on the emitted event.
    pub reason: String,
}

/// A batch of node transitions applied as one graph transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBatch {
    /// Workspace whose graph revision guards the batch.
    pub workspace_id: String,
    /// Revision the batch was built against.
    pub base_revision: u64,
    /// Ordered transitions.
    pub transitions: Vec<NodeTransition>,
}

/// What applying a batch produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOutcome {
    /// Graph revision after the transaction.
    pub revision: u64,
    /// How many transitions were applied.
    pub applied: usize,
}

/// Read the WorkGraph and apply orchestration transitions to it.
#[async_trait]
pub trait WorkGraphPort: Send + Sync {
    /// The workspace's nodes and edges as orchestration should observe them.
    ///
    /// # Errors
    /// Returns [`OrchestrationError::PortUnavailable`] when the port is not wired.
    async fn snapshot(&self, workspace_id: &str) -> Result<GraphSnapshot, OrchestrationError>;

    /// Apply one batch of node transitions atomically, compare-and-setting `base_revision`.
    ///
    /// # Errors
    /// Returns [`OrchestrationError::RevisionConflict`] when the graph moved since the
    /// snapshot, and writes nothing in that case.
    async fn apply(&self, batch: ReleaseBatch) -> Result<ReleaseOutcome, OrchestrationError>;

    /// Read the workspace's graph revision without reading the graph.
    ///
    /// # Errors
    /// Returns [`OrchestrationError::PortUnavailable`] when the port is not wired.
    async fn revision(&self, workspace_id: &str) -> Result<u64, OrchestrationError>;
}

/// The default port: graph mutation belongs to [`GRAPH_TRANSACTION_OWNER`]'s transaction, so
/// a runtime without a graph-backed port fails closed instead of inventing graph state.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableWorkGraph;

#[async_trait]
impl WorkGraphPort for UnavailableWorkGraph {
    async fn snapshot(&self, _workspace_id: &str) -> Result<GraphSnapshot, OrchestrationError> {
        Err(OrchestrationError::PortUnavailable {
            owner: GRAPH_TRANSACTION_OWNER,
            detail: "the WorkGraph port is not wired".to_string(),
        })
    }

    async fn apply(&self, _batch: ReleaseBatch) -> Result<ReleaseOutcome, OrchestrationError> {
        Err(OrchestrationError::PortUnavailable {
            owner: GRAPH_TRANSACTION_OWNER,
            detail: "the WorkGraph port is not wired".to_string(),
        })
    }

    async fn revision(&self, _workspace_id: &str) -> Result<u64, OrchestrationError> {
        Err(OrchestrationError::PortUnavailable {
            owner: GRAPH_TRANSACTION_OWNER,
            detail: "the WorkGraph port is not wired".to_string(),
        })
    }
}
