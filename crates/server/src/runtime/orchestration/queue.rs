//! Ready-queue selection, dependency release and fan-in joins (DOMAIN.md §4.1, §4.2).
//!
//! Everything in this module is a pure function over a [`GraphSnapshot`]: given the same
//! graph, the same selection and the same order. That is what makes "no dependent work starts
//! before prerequisites" and "ordering is deterministic where semantics require it" testable
//! without a database, and it keeps the orchestrator's decisions auditable.

use std::collections::{BTreeMap, BTreeSet};

use super::graph::{GraphSnapshot, NodeTransition, RunRef, WorkNodeView};
use super::status;
use super::OrchestrationError;

/// A node that may be dispatched now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchCandidate {
    /// Node to start.
    pub node_id: String,
    /// Priority that ordered it.
    pub priority: i16,
    /// The queued Run to move to `RUNNING`.
    pub run: RunRef,
}

/// A node that is ready to start but was held back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    /// Node held back.
    pub node_id: String,
    /// Why it was held back.
    pub reason: DeferralReason,
}

/// Why a ready node was not dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferralReason {
    /// The concurrency budget had no free slot.
    Capacity,
    /// Another node in the same tick was selected first; the budget is re-read next tick.
    SelectedElsewhere,
}

/// What one selection pass decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Nodes to dispatch, in deterministic order.
    pub dispatch: Vec<DispatchCandidate>,
    /// Ready nodes the concurrency budget would not admit.
    pub deferred: Vec<Deferred>,
    /// Nodes whose Run is parked or suspended: they are waiting, not ready to start.
    pub waiting: Vec<String>,
    /// Nodes that can never start because a prerequisite failed or was cancelled.
    pub blocked: Vec<String>,
}

impl Selection {
    /// Whether the pass selected nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dispatch.is_empty()
    }
}

/// The dependency-release decision for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePlan {
    /// Transitions that make newly satisfied work ready, or block work whose prerequisites
    /// can never complete.
    pub transitions: Vec<NodeTransition>,
}

impl ReleasePlan {
    /// Whether the plan changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }
}

/// The fan-in join decision for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinPlan {
    /// Transitions that release a parent once its last child finished, or block it because a
    /// child can never finish.
    pub transitions: Vec<NodeTransition>,
}

impl JoinPlan {
    /// Whether the plan changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }
}

/// Order nodes deterministically: priority desc, then node id ascending.
///
/// Priority is the semantic ordering (DOMAIN.md §4.1); the id tie-break keeps a replay of the
/// same graph byte-identical whatever order the rows arrived in.
fn deterministic_order<'a>(nodes: impl Iterator<Item = &'a WorkNodeView>) -> Vec<&'a WorkNodeView> {
    let mut ordered: Vec<&WorkNodeView> = nodes.collect();
    ordered.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    ordered
}

/// Compute the dependency-release plan for a snapshot.
///
/// A `draft` or `blocked` node becomes `ready` once every `depends_on` prerequisite is
/// `done`; a node whose prerequisite failed or was cancelled becomes `blocked` (it can never
/// run) rather than silently staying ready. A node already `ready` is left alone, so
/// repeating the plan over the same snapshot is idempotent.
#[must_use]
pub fn release_plan(snapshot: &GraphSnapshot) -> ReleasePlan {
    let mut transitions = Vec::new();
    for node in deterministic_order(snapshot.nodes.iter()) {
        if node.is_terminal() || node.status == status::READY {
            continue;
        }
        if node.status != status::DRAFT && node.status != status::BLOCKED {
            continue;
        }
        let prerequisites = snapshot.prerequisites(&node.id);
        if prerequisites.is_empty() {
            // No prerequisites: the node is releasable as soon as it exists.
            if node.status == status::DRAFT {
                transitions.push(transition(node, status::READY, "no prerequisites"));
            }
            continue;
        }
        let mut all_done = true;
        let mut can_never_complete = false;
        for prerequisite in &prerequisites {
            match snapshot.node(prerequisite) {
                Some(parent) if parent.status == status::DONE => {}
                Some(parent)
                    if parent.status == status::FAILED || parent.status == status::CANCELLED =>
                {
                    can_never_complete = true;
                    all_done = false;
                }
                _ => all_done = false,
            }
        }
        if can_never_complete {
            if node.status != status::BLOCKED {
                transitions.push(transition(
                    node,
                    status::BLOCKED,
                    "a prerequisite failed or was cancelled",
                ));
            }
        } else if all_done {
            // Either status can be released once every prerequisite is done; the guard above
            // already established the node is `draft` or `blocked`.
            transitions.push(transition(
                node,
                status::READY,
                "all prerequisites are done",
            ));
        }
    }
    ReleasePlan { transitions }
}

/// Compute the fan-in join plan for a snapshot.
///
/// A node in `waiting` whose children have all reached `done` is released to `in_progress`
/// exactly once; if any child failed or was cancelled the parent becomes `blocked` instead of
/// being released on a set of children that can never all succeed.
#[must_use]
pub fn join_plan(snapshot: &GraphSnapshot) -> JoinPlan {
    let mut transitions = Vec::new();
    for node in deterministic_order(snapshot.nodes.iter()) {
        if node.status != status::WAITING {
            continue;
        }
        let children = snapshot.children(&node.id);
        if children.is_empty() {
            continue;
        }
        let mut all_done = true;
        let mut any_broken = false;
        for child in &children {
            match snapshot.node(child) {
                Some(view) if view.status == status::DONE => {}
                Some(view) if view.status == status::FAILED || view.status == status::CANCELLED => {
                    any_broken = true;
                    all_done = false;
                }
                _ => all_done = false,
            }
        }
        if any_broken {
            transitions.push(transition(
                node,
                status::BLOCKED,
                "a child failed or was cancelled",
            ));
        } else if all_done {
            transitions.push(transition(node, status::IN_PROGRESS, "every child joined"));
        }
    }
    JoinPlan { transitions }
}

/// Select the nodes that may be dispatched now, within `headroom` free slots.
///
/// A candidate is a node that is `ready`, has a `QUEUED` Run, and whose prerequisites are all
/// `done`. Nodes whose Run is parked or suspended are reported as waiting (they hold no slot
/// and must not be dispatched again); nodes whose prerequisites failed or were cancelled are
/// reported as blocked. The pass never exceeds `headroom`.
#[must_use]
pub fn select(snapshot: &GraphSnapshot, headroom: usize) -> Selection {
    let mut selection = Selection {
        dispatch: Vec::new(),
        deferred: Vec::new(),
        waiting: Vec::new(),
        blocked: Vec::new(),
    };
    let mut remaining = headroom;

    for node in deterministic_order(snapshot.nodes.iter()) {
        match node.run.as_ref() {
            Some(_) if node.is_parked() => {
                selection.waiting.push(node.id.clone());
                continue;
            }
            Some(run) if run.status == "QUEUED" => {
                if node.status != status::READY {
                    continue;
                }
                if let Some((prerequisite_id, prerequisite_status)) =
                    snapshot.unsatisfied_prerequisite(&node.id)
                {
                    let _ = (prerequisite_id, prerequisite_status);
                    selection.blocked.push(node.id.clone());
                    continue;
                }
                if remaining == 0 {
                    selection.deferred.push(Deferred {
                        node_id: node.id.clone(),
                        reason: DeferralReason::Capacity,
                    });
                    continue;
                }
                remaining -= 1;
                selection.dispatch.push(DispatchCandidate {
                    node_id: node.id.clone(),
                    priority: node.priority,
                    run: run.clone(),
                });
            }
            _ => {}
        }
    }
    selection
}

/// Apply a set of transitions to a snapshot in memory.
///
/// Used to reason about the graph the orchestrator is about to commit, so one tick selects
/// against the state its own release produces without a second read.
#[must_use]
pub fn project(snapshot: &GraphSnapshot, transitions: &[NodeTransition]) -> GraphSnapshot {
    let mut projected = snapshot.clone();
    let updates: BTreeMap<&str, &str> = transitions
        .iter()
        .map(|transition| (transition.node_id.as_str(), transition.to.as_str()))
        .collect();
    for node in &mut projected.nodes {
        if let Some(to) = updates.get(node.id.as_str()) {
            node.status = (*to).to_string();
            node.revision += 1;
        }
    }
    projected
}

/// Verify a snapshot is acyclic over `depends_on` edges, failing closed if it is not.
///
/// The graph refuses to create a cycle, so a cycle here means a corrupt or foreign writer;
/// orchestration must not select work from a graph it cannot reason about.
///
/// # Errors
/// Returns [`OrchestrationError::DependencyCycle`] naming a node on the cycle.
pub fn ensure_acyclic(snapshot: &GraphSnapshot) -> Result<(), OrchestrationError> {
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &snapshot.edges {
        if edge.kind == "depends_on" {
            adjacency
                .entry(edge.from_node_id.as_str())
                .or_default()
                .push(edge.to_node_id.as_str());
        }
    }
    // Iterative three-colour walk; a back edge to a node already on the stack is a cycle.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Colour {
        White,
        Grey,
        Black,
    }
    let mut colours: BTreeMap<&str, Colour> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), Colour::White))
        .collect();
    for start in snapshot.nodes.iter().map(|node| node.id.as_str()) {
        if colours.get(start) != Some(&Colour::White) {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        colours.insert(start, Colour::Grey);
        let empty: Vec<&str> = Vec::new();
        while let Some((node, index)) = stack.pop() {
            let successors = adjacency.get(node).unwrap_or(&empty);
            if index < successors.len() {
                stack.push((node, index + 1));
                let next = successors[index];
                match colours.get(next).copied() {
                    Some(Colour::Grey) => {
                        return Err(OrchestrationError::DependencyCycle {
                            node_id: next.to_string(),
                        })
                    }
                    Some(Colour::White) => {
                        colours.insert(next, Colour::Grey);
                        stack.push((next, 0));
                    }
                    _ => {}
                }
            } else {
                colours.insert(node, Colour::Black);
            }
        }
    }
    Ok(())
}

/// The set of node ids the snapshot knows about, for cheap membership checks.
#[must_use]
pub fn node_ids(snapshot: &GraphSnapshot) -> BTreeSet<&str> {
    snapshot.nodes.iter().map(|node| node.id.as_str()).collect()
}

fn transition(node: &WorkNodeView, to: &str, reason: &str) -> NodeTransition {
    NodeTransition {
        node_id: node.id.clone(),
        expected_revision: node.revision,
        to: to.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph::{DependencyEdge, GraphSnapshot, RunRef, WorkNodeView};
    use super::*;

    fn node(id: &str, status: &str) -> WorkNodeView {
        WorkNodeView {
            id: id.to_string(),
            kind: "task".to_string(),
            status: status.to_string(),
            parent_id: None,
            priority: 1,
            revision: 1,
            run: None,
        }
    }

    fn node_with_run(id: &str, status: &str, run_status: &str) -> WorkNodeView {
        let mut view = node(id, status);
        view.run = Some(RunRef {
            run_id: format!("run_{id}"),
            generation: 1,
            status: run_status.to_string(),
        });
        view
    }

    fn depends_on(from: &str, to: &str) -> DependencyEdge {
        DependencyEdge {
            from_node_id: from.to_string(),
            to_node_id: to.to_string(),
            kind: "depends_on".to_string(),
        }
    }

    fn snapshot(nodes: Vec<WorkNodeView>, edges: Vec<DependencyEdge>) -> GraphSnapshot {
        GraphSnapshot {
            workspace_id: "ws_test".to_string(),
            revision: 7,
            nodes,
            edges,
        }
    }

    #[test]
    fn a_dependent_never_starts_before_its_prerequisite() {
        let graph = snapshot(
            vec![
                node("wn_a", status::DONE),
                node_with_run("wn_b", status::READY, "QUEUED"),
            ],
            vec![depends_on("wn_b", "wn_a")],
        );
        let selection = select(&graph, 4);
        assert_eq!(selection.dispatch.len(), 1, "the prerequisite is done");

        let graph = snapshot(
            vec![
                node("wn_a", status::IN_PROGRESS),
                node_with_run("wn_b", status::READY, "QUEUED"),
            ],
            vec![depends_on("wn_b", "wn_a")],
        );
        let selection = select(&graph, 4);
        assert!(selection.is_empty(), "a queued run is not enough to start");
        assert_eq!(selection.blocked, vec!["wn_b".to_string()]);
    }

    #[test]
    fn release_makes_satisfied_work_ready_and_blocks_work_that_can_never_run() {
        let graph = snapshot(
            vec![
                node("wn_a", status::DONE),
                node("wn_b", status::DRAFT),
                node("wn_c", status::DRAFT),
                node("wn_d", status::CANCELLED),
                node("wn_e", status::BLOCKED),
            ],
            vec![
                depends_on("wn_b", "wn_a"),
                depends_on("wn_c", "wn_d"),
                depends_on("wn_e", "wn_d"),
            ],
        );
        let plan = release_plan(&graph);
        let by_node: Vec<(&str, &str)> = plan
            .transitions
            .iter()
            .map(|transition| (transition.node_id.as_str(), transition.to.as_str()))
            .collect();
        assert!(by_node.contains(&("wn_b", status::READY)));
        assert!(by_node.contains(&("wn_c", status::BLOCKED)));
        assert!(
            !by_node.iter().any(|(node, _)| *node == "wn_e"),
            "an already blocked node with a cancelled prerequisite stays blocked"
        );
    }

    #[test]
    fn release_is_idempotent_over_the_same_snapshot() {
        let graph = snapshot(
            vec![node("wn_a", status::DONE), node("wn_b", status::DRAFT)],
            vec![depends_on("wn_b", "wn_a")],
        );
        let first = release_plan(&graph);
        assert_eq!(first.transitions.len(), 1);
        let projected = project(&graph, &first.transitions);
        assert!(
            release_plan(&projected).is_empty(),
            "a released graph releases nothing more"
        );
    }

    #[test]
    fn joins_release_once_and_block_on_a_broken_child() {
        let mut parent = node("wn_p", status::WAITING);
        parent.kind = "objective".to_string();
        let mut child_a = node("wn_c1", status::DONE);
        child_a.parent_id = Some("wn_p".to_string());
        let mut child_b = node("wn_c2", status::DONE);
        child_b.parent_id = Some("wn_p".to_string());
        let graph = snapshot(vec![parent.clone(), child_a.clone(), child_b], vec![]);
        let plan = join_plan(&graph);
        assert_eq!(plan.transitions.len(), 1);
        assert_eq!(plan.transitions[0].to, status::IN_PROGRESS);

        let mut failed = node("wn_c2", status::FAILED);
        failed.parent_id = Some("wn_p".to_string());
        let graph = snapshot(vec![parent, child_a, failed], vec![]);
        let plan = join_plan(&graph);
        assert_eq!(plan.transitions.len(), 1);
        assert_eq!(plan.transitions[0].to, status::BLOCKED);
    }

    #[test]
    fn ordering_is_priority_then_id() {
        let mut high = node_with_run("wn_z", status::READY, "QUEUED");
        high.priority = 3;
        let mut low = node_with_run("wn_a", status::READY, "QUEUED");
        low.priority = 0;
        let mut mid = node_with_run("wn_b", status::READY, "QUEUED");
        mid.priority = 1;
        let graph = snapshot(vec![low, mid, high], vec![]);
        let selection = select(&graph, 8);
        let order: Vec<&str> = selection
            .dispatch
            .iter()
            .map(|candidate| candidate.node_id.as_str())
            .collect();
        assert_eq!(order, vec!["wn_z", "wn_b", "wn_a"]);
    }

    #[test]
    fn capacity_defers_the_tail_deterministically() {
        let graph = snapshot(
            vec![
                node_with_run("wn_a", status::READY, "QUEUED"),
                node_with_run("wn_b", status::READY, "QUEUED"),
                node_with_run("wn_c", status::READY, "QUEUED"),
            ],
            vec![],
        );
        let selection = select(&graph, 2);
        assert_eq!(selection.dispatch.len(), 2);
        assert_eq!(selection.deferred.len(), 1);
        assert_eq!(selection.deferred[0].node_id, "wn_c");
        assert_eq!(selection.deferred[0].reason, DeferralReason::Capacity);
    }

    #[test]
    fn parked_runs_are_waiting_not_dispatchable() {
        let graph = snapshot(
            vec![node_with_run(
                "wn_a",
                status::IN_PROGRESS,
                "WAITING_APPROVAL",
            )],
            vec![],
        );
        let selection = select(&graph, 4);
        assert!(selection.is_empty());
        assert_eq!(selection.waiting, vec!["wn_a".to_string()]);
        assert_eq!(graph.active_slots(), 0, "a parked run holds no slot");
    }

    #[test]
    fn a_cycle_is_refused() {
        let graph = snapshot(
            vec![node("wn_a", status::READY), node("wn_b", status::READY)],
            vec![depends_on("wn_a", "wn_b"), depends_on("wn_b", "wn_a")],
        );
        assert!(matches!(
            ensure_acyclic(&graph),
            Err(OrchestrationError::DependencyCycle { .. })
        ));
    }
}
