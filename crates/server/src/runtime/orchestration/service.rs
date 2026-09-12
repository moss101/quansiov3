//! The orchestrator: one tick of release → select → dispatch, and subtree cancellation.
//!
//! The orchestrator owns no state of its own. A tick reads the graph, commits the dependency
//! and join releases it must make, selects the work the concurrency budget admits, and moves
//! each selected run `QUEUED → RUNNING` through the canonical Run state machine. Running it
//! twice over an unchanged graph is a no-op, which is what makes it safe to call from a loop,
//! from a retry, or from two instances at once.

use std::sync::Arc;

use quansio_core::CanonicalId;

use crate::runtime::state_machine::{RuntimeError, RuntimeIdentity, RuntimeStore};
use crate::scheduler::{DispatchOutcome, RunDispatch, RunDispatchRequest};

use super::cancel::{cancel_plan, CancelPlan, CancelRequest, CancellationOutcome, CancelledNode};
use super::capacity::{CapacityGate, CapacityRefusal};
use super::graph::{ReleaseBatch, WorkGraphPort};
use super::queue::{ensure_acyclic, join_plan, project, release_plan, select};
use super::OrchestrationError;

/// The maximum number of times a tick recomputes after losing the graph revision race.
pub const MAX_REVISION_ATTEMPTS: usize = 3;

/// The maximum number of times a cancellation recomputes after losing the graph revision race.
///
/// Cancellation is the one operation expected to arrive in storms, so it retries harder: a
/// lost race means another canceller is doing the same work, and the pass should end up
/// reporting "already cancelled" rather than a revision conflict.
pub const MAX_CANCEL_ATTEMPTS: usize = 10;

/// What one orchestration tick did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Workspace the tick ran for.
    pub workspace_id: String,
    /// Graph revision the tick left behind.
    pub graph_revision: u64,
    /// Dependency releases applied.
    pub released: usize,
    /// Fan-in joins applied.
    pub joined: usize,
    /// Runs moved to `RUNNING`.
    pub dispatched: usize,
    /// Runs that were already running.
    pub already_running: usize,
    /// Runs whose request was fenced by a newer generation.
    pub fenced: usize,
    /// Runs the runtime refused to dispatch.
    pub not_dispatchable: usize,
    /// Ready work the concurrency budget did not admit.
    pub deferred: Vec<String>,
    /// Nodes waiting on a parked or suspended run.
    pub waiting: Vec<String>,
    /// Nodes that cannot start because a prerequisite failed or was cancelled.
    pub blocked: Vec<String>,
    /// The capacity refusal, when work was held back.
    pub capacity: Option<CapacityRefusal>,
}

impl TickReport {
    /// Whether the tick changed anything.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.released == 0 && self.joined == 0 && self.dispatched == 0
    }
}

/// The orchestration authority for one tenant.
#[derive(Clone)]
pub struct Orchestrator {
    graph: Arc<dyn WorkGraphPort>,
    dispatch: Arc<dyn RunDispatch>,
    runs: RuntimeStore,
    identity: RuntimeIdentity,
    capacity: CapacityGate,
}

impl Orchestrator {
    /// Build the orchestrator over a WorkGraph port and the canonical run dispatcher.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the identity's tenant is not canonical.
    pub fn new(
        pool: sqlx::PgPool,
        identity: RuntimeIdentity,
        graph: Arc<dyn WorkGraphPort>,
        dispatch: Arc<dyn RunDispatch>,
        capacity: CapacityGate,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            graph,
            dispatch,
            runs: RuntimeStore::new(pool, identity.clone())?,
            identity,
            capacity,
        })
    }

    /// The event identity this orchestrator stamps on its runtime transitions.
    #[must_use]
    pub fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    /// The concurrency gate in force.
    #[must_use]
    pub const fn capacity(&self) -> CapacityGate {
        self.capacity
    }

    /// Run one tick: release what became runnable, then dispatch what the budget admits.
    ///
    /// # Errors
    /// Returns [`OrchestrationError::DependencyCycle`] for a graph that cannot be reasoned
    /// about, [`OrchestrationError::RevisionConflict`] when a racing writer keeps winning, and
    /// the port's own refusal when the WorkGraph is not wired.
    pub async fn tick(&self, workspace_id: &str) -> Result<TickReport, OrchestrationError> {
        for attempt in 0..MAX_REVISION_ATTEMPTS {
            let snapshot = self.graph.snapshot(workspace_id).await?;
            ensure_acyclic(&snapshot)?;

            let release = release_plan(&snapshot);
            let join = join_plan(&snapshot);
            let mut transitions = release.transitions.clone();
            transitions.extend(join.transitions.clone());
            let released = release.transitions.len();
            let joined = join.transitions.len();

            let mut committed_revision = snapshot.revision;
            if !transitions.is_empty() {
                match self
                    .graph
                    .apply(ReleaseBatch {
                        workspace_id: snapshot.workspace_id.clone(),
                        base_revision: snapshot.revision,
                        transitions: transitions.clone(),
                    })
                    .await
                {
                    Ok(outcome) => committed_revision = outcome.revision,
                    Err(OrchestrationError::RevisionConflict { current, .. })
                        if attempt + 1 < MAX_REVISION_ATTEMPTS =>
                    {
                        // Another writer moved the graph: recompute against the new revision
                        // rather than applying transitions that no longer fit.
                        let _ = current;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }

            // Select against the graph this tick is about to leave behind, so a node released
            // in this pass can start in the same pass without a second read.
            let projected = project(&snapshot, &transitions);
            let headroom = self.capacity.headroom(&projected);
            let selection = select(&projected, headroom);

            let mut dispatched = 0;
            let mut already_running = 0;
            let mut fenced = 0;
            let mut not_dispatchable = 0;
            for candidate in &selection.dispatch {
                let outcome = self
                    .dispatch
                    .dispatch_queued(RunDispatchRequest {
                        tenant_id: self.identity.tenant_id.clone(),
                        run_id: candidate.run.run_id.clone(),
                        generation: candidate.run.generation,
                    })
                    .await?;
                match outcome {
                    DispatchOutcome::Dispatched => dispatched += 1,
                    DispatchOutcome::AlreadyRunning => already_running += 1,
                    DispatchOutcome::Fenced => fenced += 1,
                    DispatchOutcome::NotDispatchable => not_dispatchable += 1,
                }
            }

            let deferred: Vec<String> = selection
                .deferred
                .iter()
                .map(|deferred| deferred.node_id.clone())
                .collect();
            let capacity = if deferred.is_empty() {
                None
            } else {
                Some(self.capacity.refusal(&projected))
            };
            return Ok(TickReport {
                workspace_id: projected.workspace_id.clone(),
                graph_revision: committed_revision,
                released,
                joined,
                dispatched,
                already_running,
                fenced,
                not_dispatchable,
                deferred,
                waiting: selection.waiting,
                blocked: selection.blocked,
                capacity,
            });
        }
        Err(OrchestrationError::RevisionConflict {
            workspace_id: workspace_id.to_string(),
            expected: 0,
            current: self.graph.revision(workspace_id).await?,
        })
    }

    /// Cancel a node's subtree, its live runs and its dependents.
    ///
    /// Runs are cancelled first through the authoritative `RuntimeStore::cancel` (idempotent,
    /// one `run.cancelled` per run, never a re-dispatch), then the node transitions are applied
    /// as one compare-and-set batch. A lost race recomputes: the winner's cancellation makes
    /// the next plan empty, so a cancel storm converges on "already cancelled" instead of
    /// duplicating work.
    ///
    /// # Errors
    /// Returns [`OrchestrationError::NotFound`] when the node is unknown, and the port's
    /// refusal when the WorkGraph is not wired.
    pub async fn cancel_subtree(
        &self,
        request: CancelRequest,
    ) -> Result<CancellationOutcome, OrchestrationError> {
        // Work this call actually did, accumulated across retries: a cancellation that wins
        // the graph race on a later attempt must still report the runs it stopped.
        let mut did_cancel: Vec<String> = Vec::new();
        let mut was_cancelled: Vec<String> = Vec::new();
        for attempt in 0..MAX_CANCEL_ATTEMPTS {
            let snapshot = self.graph.snapshot(&request.workspace_id).await?;
            let plan = cancel_plan(&snapshot, &request)?;
            if plan.is_empty() {
                if did_cancel.is_empty() {
                    return Ok(CancellationOutcome::already_done());
                }
                return Ok(CancellationOutcome {
                    cancelled: Vec::new(),
                    blocked: Vec::new(),
                    runs_cancelled: did_cancel,
                    runs_already_cancelled: was_cancelled,
                    runs_left_unstarted: Vec::new(),
                    already_cancelled: true,
                });
            }

            let run_outcome = self.cancel_runs(&plan).await?;
            for run_id in run_outcome.0 {
                if !did_cancel.contains(&run_id) {
                    did_cancel.push(run_id);
                }
            }
            for run_id in run_outcome.1 {
                if !was_cancelled.contains(&run_id) {
                    was_cancelled.push(run_id);
                }
            }
            let run_outcome = (did_cancel.clone(), was_cancelled.clone());
            let mut cancelled = Vec::new();
            for transition in &plan.cancelled_nodes {
                let previous_status = snapshot
                    .node(&transition.node_id)
                    .map(|node| node.status.clone())
                    .unwrap_or_default();
                cancelled.push(CancelledNode {
                    node_id: transition.node_id.clone(),
                    previous_status,
                    applied: true,
                });
            }
            let blocked: Vec<String> = plan
                .blocked_dependents
                .iter()
                .map(|transition| transition.node_id.clone())
                .collect();

            match self
                .graph
                .apply(ReleaseBatch {
                    workspace_id: snapshot.workspace_id.clone(),
                    base_revision: snapshot.revision,
                    transitions: plan.transitions(),
                })
                .await
            {
                Ok(_) => {
                    let mut already = run_outcome.1;
                    already.retain(|run_id| !run_outcome.0.contains(run_id));
                    return Ok(CancellationOutcome {
                        cancelled,
                        blocked,
                        runs_cancelled: run_outcome.0,
                        runs_already_cancelled: already,
                        runs_left_unstarted: plan
                            .unstarted_runs
                            .iter()
                            .map(|run| run.run_id.clone())
                            .collect(),
                        already_cancelled: false,
                    });
                }
                Err(OrchestrationError::RevisionConflict {
                    expected, current, ..
                }) => {
                    if attempt + 1 < MAX_CANCEL_ATTEMPTS {
                        continue;
                    }
                    // Out of retries: another canceller may have finished the work, so only a
                    // genuinely still-pending change is a conflict.
                    let snapshot = self.graph.snapshot(&request.workspace_id).await?;
                    if cancel_plan(&snapshot, &request)?.is_empty() {
                        return Ok(CancellationOutcome {
                            cancelled: Vec::new(),
                            blocked: Vec::new(),
                            runs_cancelled: did_cancel,
                            runs_already_cancelled: was_cancelled,
                            runs_left_unstarted: Vec::new(),
                            already_cancelled: true,
                        });
                    }
                    return Err(OrchestrationError::RevisionConflict {
                        workspace_id: request.workspace_id.clone(),
                        expected,
                        current,
                    });
                }
                Err(error) => return Err(error),
            }
        }
        Ok(CancellationOutcome::already_done())
    }

    /// Cancel every live run the plan covers, returning (newly cancelled, already cancelled).
    async fn cancel_runs(
        &self,
        plan: &CancelPlan,
    ) -> Result<(Vec<String>, Vec<String>), OrchestrationError> {
        let mut cancelled = Vec::new();
        let mut already = Vec::new();
        for run in &plan.runs {
            let run_id = CanonicalId::parse_typed(&run.run_id, quansio_core::Prefix::Run)?;
            let before = self.runs.load_run(&run_id).await?;
            if before.status == crate::runtime::state_machine::RunStatus::Cancelled {
                already.push(run.run_id.clone());
                continue;
            }
            match self.runs.cancel(&run_id, before.generation).await {
                Ok(after)
                    if after.status == crate::runtime::state_machine::RunStatus::Cancelled =>
                {
                    cancelled.push(run.run_id.clone());
                }
                Ok(_) => already.push(run.run_id.clone()),
                Err(RuntimeError::StateConflict { .. }) => {
                    // A concurrent cancellation won the transaction: the storm converges on
                    // "already cancelled" instead of failing or cancelling twice.
                    already.push(run.run_id.clone());
                }
                Err(error) => return Err(OrchestrationError::from(error)),
            }
        }
        Ok((cancelled, already))
    }
}
