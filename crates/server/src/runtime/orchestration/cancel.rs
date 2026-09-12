//! Cancellation propagation (DOMAIN.md §4.1, §5.2, §13.2).
//!
//! Cancelling a node must stop the work it owns and everything it spawned, and it must be safe
//! to ask for a thousand times at once. Three properties make that true here:
//!
//! * the **plan** is a pure function of one snapshot, so every caller in a cancel storm
//!   computes the same set;
//! * every node transition goes through the graph's revision compare-and-set, so a racing
//!   batch is refused instead of applied twice;
//! * every run is cancelled through the one authoritative `RuntimeStore::cancel`, which is
//!   idempotent and emits exactly one `run.cancelled`, and never re-dispatches or settles an
//!   effect. Effects whose outcome is unknown are left for reconciliation, never cancelled
//!   blindly.
//!
//! Cancellation propagates down `work_nodes.parent_id` (the unambiguous ancestor column) and
//! blocks the cancelled nodes' dependents, which can never satisfy their prerequisites.

use super::graph::{GraphSnapshot, NodeTransition, RunRef};
use super::status;
use super::OrchestrationError;

/// A request to cancel one node and its subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelRequest {
    /// Owning workspace.
    pub workspace_id: String,
    /// Node whose subtree is cancelled.
    pub node_id: String,
    /// Why, recorded on every transition and run cancellation.
    pub reason: String,
}

/// The cancellation decision for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelPlan {
    /// Nodes in the subtree that must move to `cancelled`.
    pub cancelled_nodes: Vec<NodeTransition>,
    /// Dependents outside the subtree that can never satisfy their prerequisites.
    pub blocked_dependents: Vec<NodeTransition>,
    /// Runs the subtree owns that the Run state machine can cancel, in deterministic order.
    pub runs: Vec<RunRef>,
    /// Runs the subtree owns that have not started (`CREATED`/`QUEUED`).
    ///
    /// The Run state machine has no `QUEUED → CANCELLED` edge, so these are never cancelled and
    /// never start: the node is cancelled, and orchestration only dispatches `ready` nodes.
    pub unstarted_runs: Vec<RunRef>,
}

impl CancelPlan {
    /// Whether the plan changes nothing, because the work is already cancelled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cancelled_nodes.is_empty()
            && self.blocked_dependents.is_empty()
            && self.runs.is_empty()
            && self.unstarted_runs.is_empty()
    }

    /// Every node transition the plan applies, in one deterministic order.
    #[must_use]
    pub fn transitions(&self) -> Vec<NodeTransition> {
        let mut transitions = self.cancelled_nodes.clone();
        transitions.extend(self.blocked_dependents.clone());
        transitions
    }
}

/// One node whose cancellation was applied or already present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledNode {
    /// Node identity.
    pub node_id: String,
    /// The status it held before the plan.
    pub previous_status: String,
    /// Whether this cancellation changed the node.
    pub applied: bool,
}

/// What a cancellation pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationOutcome {
    /// Nodes in the cancelled subtree.
    pub cancelled: Vec<CancelledNode>,
    /// Dependents that were blocked.
    pub blocked: Vec<String>,
    /// Runs this pass moved to `CANCELLED`.
    pub runs_cancelled: Vec<String>,
    /// Runs that were already `CANCELLED`; the pass is idempotent, not duplicating.
    pub runs_already_cancelled: Vec<String>,
    /// Runs that had not started and are therefore never dispatched for a cancelled node.
    pub runs_left_unstarted: Vec<String>,
    /// Whether the subtree was already cancelled when the pass began.
    pub already_cancelled: bool,
}

impl CancellationOutcome {
    /// The outcome of a no-op pass over an already cancelled subtree.
    #[must_use]
    pub fn already_done() -> Self {
        Self {
            cancelled: Vec::new(),
            blocked: Vec::new(),
            runs_cancelled: Vec::new(),
            runs_already_cancelled: Vec::new(),
            runs_left_unstarted: Vec::new(),
            already_cancelled: true,
        }
    }
}

/// Compute the cancellation plan for a node's subtree.
///
/// # Errors
/// Returns [`OrchestrationError::NotFound`] when the node is not in the snapshot.
pub fn cancel_plan(
    snapshot: &GraphSnapshot,
    request: &CancelRequest,
) -> Result<CancelPlan, OrchestrationError> {
    let Some(root) = snapshot.node(&request.node_id) else {
        return Err(OrchestrationError::NotFound {
            entity: "work_node",
            id: request.node_id.clone(),
        });
    };

    let subtree = snapshot.subtree(&root.id);
    let mut cancelled_nodes = Vec::new();
    let mut unstarted_runs: Vec<RunRef> = Vec::new();
    let mut runs = Vec::new();
    for node_id in &subtree {
        let Some(node) = snapshot.node(node_id) else {
            continue;
        };
        if node.status != status::CANCELLED && !node.is_terminal() {
            cancelled_nodes.push(NodeTransition {
                node_id: node.id.clone(),
                expected_revision: node.revision,
                to: status::CANCELLED.to_string(),
                reason: request.reason.clone(),
            });
        }
        if let Some(run) = &node.run {
            match run.status.as_str() {
                "RUNNING" | "VERIFYING" | "SUSPENDED" => runs.push(run.clone()),
                status if status.starts_with("WAITING_") => runs.push(run.clone()),
                "CREATED" | "QUEUED" => unstarted_runs.push(run.clone()),
                _ => {}
            }
        }
    }
    cancelled_nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    unstarted_runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));

    // A dependent outside the subtree can never satisfy a prerequisite that was cancelled.
    let mut blocked: Vec<String> = Vec::new();
    for node_id in &subtree {
        for dependent in snapshot.dependents(node_id) {
            if subtree.contains(&dependent) || blocked.iter().any(|id| id == dependent) {
                continue;
            }
            let Some(node) = snapshot.node(dependent) else {
                continue;
            };
            if node.is_terminal() || node.status == status::BLOCKED {
                continue;
            }
            blocked.push(node.id.clone());
        }
    }
    blocked.sort();
    let blocked_dependents = blocked
        .into_iter()
        .filter_map(|node_id| {
            snapshot.node(&node_id).map(|node| NodeTransition {
                node_id: node.id.clone(),
                expected_revision: node.revision,
                to: status::BLOCKED.to_string(),
                reason: format!("prerequisite cancelled: {}", request.reason),
            })
        })
        .collect();

    Ok(CancelPlan {
        cancelled_nodes,
        blocked_dependents,
        runs,
        unstarted_runs,
    })
}

#[cfg(test)]
mod tests {
    use super::super::graph::{DependencyEdge, GraphSnapshot, RunRef, WorkNodeView};
    use super::*;

    fn node(
        id: &str,
        status: &str,
        parent: Option<&str>,
        run_status: Option<&str>,
    ) -> WorkNodeView {
        WorkNodeView {
            id: id.to_string(),
            kind: "task".to_string(),
            status: status.to_string(),
            parent_id: parent.map(str::to_string),
            priority: 1,
            revision: 3,
            run: run_status.map(|status| RunRef {
                run_id: format!("run_{id}"),
                generation: 1,
                status: status.to_string(),
            }),
        }
    }

    fn snapshot(nodes: Vec<WorkNodeView>, edges: Vec<DependencyEdge>) -> GraphSnapshot {
        GraphSnapshot {
            workspace_id: "ws_test".to_string(),
            revision: 5,
            nodes,
            edges,
        }
    }

    fn request(node_id: &str) -> CancelRequest {
        CancelRequest {
            workspace_id: "ws_test".to_string(),
            node_id: node_id.to_string(),
            reason: "operator cancelled".to_string(),
        }
    }

    fn depends_on(from: &str, to: &str) -> DependencyEdge {
        DependencyEdge {
            from_node_id: from.to_string(),
            to_node_id: to.to_string(),
            kind: "depends_on".to_string(),
        }
    }

    #[test]
    fn cancellation_covers_the_subtree_its_runs_and_its_dependents() {
        let graph = snapshot(
            vec![
                node("wn_root", status::IN_PROGRESS, None, Some("RUNNING")),
                node("wn_child", status::READY, Some("wn_root"), Some("QUEUED")),
                node(
                    "wn_grandchild",
                    status::WAITING,
                    Some("wn_child"),
                    Some("WAITING_CHILD"),
                ),
                node("wn_dependent", status::READY, None, Some("QUEUED")),
            ],
            vec![depends_on("wn_dependent", "wn_child")],
        );
        let plan = cancel_plan(&graph, &request("wn_root")).expect("plan");
        let cancelled: Vec<&str> = plan
            .cancelled_nodes
            .iter()
            .map(|transition| transition.node_id.as_str())
            .collect();
        assert_eq!(cancelled, vec!["wn_child", "wn_grandchild", "wn_root"]);
        assert_eq!(
            plan.runs.len(),
            2,
            "the running run and the parked run are cancelled"
        );
        assert_eq!(
            plan.unstarted_runs.len(),
            1,
            "the queued run has no cancellation edge and is reported instead"
        );
        assert_eq!(plan.unstarted_runs[0].status, "QUEUED");
        let blocked: Vec<&str> = plan
            .blocked_dependents
            .iter()
            .map(|transition| transition.node_id.as_str())
            .collect();
        assert_eq!(blocked, vec!["wn_dependent"]);
    }

    #[test]
    fn a_queued_run_is_reported_as_unstarted_not_cancelled() {
        let graph = snapshot(
            vec![node("wn_root", status::READY, None, Some("QUEUED"))],
            vec![],
        );
        let plan = cancel_plan(&graph, &request("wn_root")).expect("plan");
        assert!(plan.runs.is_empty(), "a queued run cannot be cancelled");
        assert_eq!(plan.unstarted_runs.len(), 1);
        assert_eq!(plan.unstarted_runs[0].status, "QUEUED");
        assert_eq!(plan.cancelled_nodes.len(), 1, "the node is still cancelled");
    }

    #[test]
    fn an_already_cancelled_subtree_plans_nothing() {
        let graph = snapshot(
            vec![
                node("wn_root", status::CANCELLED, None, Some("CANCELLED")),
                node(
                    "wn_child",
                    status::CANCELLED,
                    Some("wn_root"),
                    Some("CANCELLED"),
                ),
            ],
            vec![],
        );
        let plan = cancel_plan(&graph, &request("wn_root")).expect("plan");
        assert!(
            plan.is_empty(),
            "a second cancellation is a no-op, never a second transition"
        );
    }

    #[test]
    fn a_settled_effect_inside_the_subtree_is_not_retouched() {
        let graph = snapshot(
            vec![
                node("wn_root", status::IN_PROGRESS, None, Some("RUNNING")),
                node(
                    "wn_done_child",
                    status::DONE,
                    Some("wn_root"),
                    Some("SUCCEEDED"),
                ),
            ],
            vec![],
        );
        let plan = cancel_plan(&graph, &request("wn_root")).expect("plan");
        assert_eq!(plan.runs.len(), 1, "only the live run is cancelled");
        assert_eq!(plan.runs[0].status, "RUNNING");
        assert!(plan.unstarted_runs.is_empty());
        let cancelled: Vec<&str> = plan
            .cancelled_nodes
            .iter()
            .map(|transition| transition.node_id.as_str())
            .collect();
        assert_eq!(cancelled, vec!["wn_root"], "a done child is left alone");
    }
}
