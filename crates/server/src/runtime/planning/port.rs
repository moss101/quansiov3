//! The WorkGraph port the compiler validates against and commits through (RUN-003).
//!
//! `quansio-graph` depends on `quansio-server` for `control::schema`, so `crates/server`
//! cannot depend on `quansio_graph` without a package cycle (the workspace conformance
//! gate forbids one, dev-dependencies included). The planning module therefore defines
//! this port; the graph-backed implementation lives where both crates are visible and
//! applies a [`CompiledPlan`] through `quansio_graph`'s event-emitting `GraphTransaction`
//! (`GraphTransaction::apply_plan`), so there is exactly one graph mutation path.

use async_trait::async_trait;
use quansio_core::EventId;

use super::compile::{CompiledPlan, PlanGraphSnapshot};
use super::PlanError;

/// The task that owns the event-emitting graph transaction (CORE-005).
pub const PLAN_GRAPH_OWNER: &str = "CORE-005";

/// Read the WorkGraph and commit a validated plan as one graph transaction.
#[async_trait]
pub trait PlanWorkspacePort: Send + Sync {
    /// The workspace's nodes, edges and graph revision as the proposer observed them.
    ///
    /// # Errors
    /// Returns [`PlanError::Workspace`] when the graph cannot be read.
    async fn snapshot(&self, workspace_id: &str) -> Result<PlanGraphSnapshot, PlanError>;

    /// Apply a validated plan atomically and emit its RuntimeEvents.
    ///
    /// The implementation must compare-and-set `plan.base_revision`; a stale base returns
    /// [`PlanError::StaleBaseRevision`] and writes nothing.
    ///
    /// # Errors
    /// Returns the typed rejection when the transaction refuses the plan.
    async fn apply(&self, plan: CompiledPlan) -> Result<PlanCommitOutcome, PlanError>;
}

/// What an applied plan produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCommitOutcome {
    /// The workspace graph revision after the transaction.
    pub revision: u64,
    /// How many graph changes were applied.
    pub applied: usize,
    /// The RuntimeEvents the transaction committed, in order.
    pub event_ids: Vec<EventId>,
}

/// The default port: plan application belongs to CORE-005's GraphTransaction.
///
/// It never fabricates success; a runtime without a graph-backed port fails closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailablePlanWorkspace;

#[async_trait]
impl PlanWorkspacePort for UnavailablePlanWorkspace {
    async fn snapshot(&self, _workspace_id: &str) -> Result<PlanGraphSnapshot, PlanError> {
        Err(PlanError::Workspace {
            detail: format!("the WorkGraph port is owned by {PLAN_GRAPH_OWNER}"),
        })
    }

    async fn apply(&self, _plan: CompiledPlan) -> Result<PlanCommitOutcome, PlanError> {
        Err(PlanError::Workspace {
            detail: format!("the WorkGraph port is owned by {PLAN_GRAPH_OWNER}"),
        })
    }
}
