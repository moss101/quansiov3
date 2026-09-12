//! Bounded concurrency: the execution-slot budget (DOMAIN.md §13.2 `limits.concurrency`).
//!
//! The number of slots in use is counted from **durable** run state — every node whose run is
//! live and not parked — rather than from in-process bookkeeping. That choice is deliberate:
//! an orchestrator that restarts must not believe it has more capacity than it has, and two
//! orchestrator instances sharing a workspace must agree on how much work is in flight.
//!
//! The limit itself is a budget value and is supplied by the caller (the composition root
//! reads it from the run's budget; RUN-010 owns budget enforcement). This module never invents
//! a default limit, because a default would be a policy value living in source.

use super::graph::GraphSnapshot;
use super::OrchestrationError;

/// A refusal to admit more work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityRefusal {
    /// Runs already holding a slot.
    pub in_flight: usize,
    /// The configured limit.
    pub limit: usize,
}

impl CapacityRefusal {
    /// The refusal as a typed orchestration error.
    #[must_use]
    pub fn into_error(self) -> OrchestrationError {
        OrchestrationError::CapacitySaturated {
            in_flight: self.in_flight,
            limit: self.limit,
        }
    }
}

/// The bounded-concurrency gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityGate {
    limit: usize,
}

impl CapacityGate {
    /// Build a gate with the effective `concurrency` limit.
    ///
    /// A limit of zero is legal and freezes new work (the orchestrator's kill-switch state):
    /// it admits nothing rather than being treated as "unbounded".
    #[must_use]
    pub const fn new(limit: usize) -> Self {
        Self { limit }
    }

    /// The effective limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// How many more runs may start.
    #[must_use]
    pub fn headroom(&self, snapshot: &GraphSnapshot) -> usize {
        self.limit.saturating_sub(snapshot.active_slots())
    }

    /// Whether the gate admits more work.
    #[must_use]
    pub fn admits(&self, snapshot: &GraphSnapshot) -> bool {
        self.headroom(snapshot) > 0
    }

    /// Admit at most `headroom` candidates, reporting the refusal when none fit.
    ///
    /// # Errors
    /// Returns [`OrchestrationError::CapacitySaturated`] when the limit is already reached.
    pub fn admit(&self, snapshot: &GraphSnapshot) -> Result<usize, OrchestrationError> {
        let headroom = self.headroom(snapshot);
        if headroom == 0 && self.limit > 0 {
            return Err(self.refusal(snapshot).into_error());
        }
        Ok(headroom)
    }

    /// The current refusal, whether or not the gate is saturated.
    #[must_use]
    pub fn refusal(&self, snapshot: &GraphSnapshot) -> CapacityRefusal {
        CapacityRefusal {
            in_flight: snapshot.active_slots(),
            limit: self.limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph::{GraphSnapshot, RunRef, WorkNodeView};
    use super::*;

    fn node_with_run(id: &str, run_status: &str) -> WorkNodeView {
        WorkNodeView {
            id: id.to_string(),
            kind: "task".to_string(),
            status: "in_progress".to_string(),
            parent_id: None,
            priority: 1,
            revision: 1,
            run: Some(RunRef {
                run_id: format!("run_{id}"),
                generation: 1,
                status: run_status.to_string(),
            }),
        }
    }

    fn snapshot(nodes: Vec<WorkNodeView>) -> GraphSnapshot {
        GraphSnapshot {
            workspace_id: "ws_test".to_string(),
            revision: 1,
            nodes,
            edges: Vec::new(),
        }
    }

    #[test]
    fn slots_are_counted_from_durable_run_state() {
        let graph = snapshot(vec![
            node_with_run("wn_a", "RUNNING"),
            node_with_run("wn_b", "WAITING_QUESTION"),
            node_with_run("wn_c", "SUCCEEDED"),
            node_with_run("wn_d", "CANCELLED"),
        ]);
        let gate = CapacityGate::new(4);
        assert_eq!(graph.active_slots(), 1, "only the running run holds a slot");
        assert_eq!(gate.headroom(&graph), 3);
    }

    #[test]
    fn a_saturated_gate_refuses_with_a_typed_error() {
        let graph = snapshot(vec![
            node_with_run("wn_a", "RUNNING"),
            node_with_run("wn_b", "RUNNING"),
        ]);
        let gate = CapacityGate::new(2);
        let error = gate.admit(&graph).expect_err("saturated");
        assert_eq!(error.code(), "BUDGET_EXHAUSTED");
        assert!(matches!(
            error,
            OrchestrationError::CapacitySaturated {
                in_flight: 2,
                limit: 2
            }
        ));
    }

    #[test]
    fn a_zero_limit_freezes_rather_than_unbounding() {
        let graph = snapshot(Vec::new());
        let gate = CapacityGate::new(0);
        assert_eq!(gate.headroom(&graph), 0);
        assert!(
            gate.admit(&graph).is_ok(),
            "a frozen gate reports zero slots without a saturation error"
        );
    }
}
