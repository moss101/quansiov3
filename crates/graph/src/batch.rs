//! Atomic multi-graph mutation: the seam CORE-005 (GraphTransaction) builds on.
//!
//! [`GraphStore::apply_batch`] applies a batch of WorkGraph, AgentGraph and StateGraph
//! changes in one PostgreSQL transaction behind one compare-and-set of the workspace graph
//! revision. If any item fails, the transaction is abandoned and *no* item is visible: a
//! partially applied graph is impossible.
//!
//! CORE-003 owns the event store and CORE-005 owns the eventing transaction, so this
//! module deliberately emits no events; it is the state-only core those tasks wrap.

use quansio_core::{CanonicalId, Generation, Revision};
use serde_json::Value;

use crate::agent::{
    AgentThread, Delegated, DelegationRequest, NewAgentThread, StructuralDelegationCheck,
};
use crate::error::{Entity, GraphError};
use crate::runtime::{NewRun, NewStep, NewTurn, Run};
use crate::state::{
    AgentThreadStatus, AttemptStatus, RunStatus, StepStatus, TurnStatus, WorkNodeStatus,
};
use crate::store::GraphStore;
use crate::work::{NewWorkEdge, NewWorkNode, WorkEdge, WorkNode};

/// One mutation in a [`GraphBatch`].
///
/// Large payloads are boxed so the enum stays small; build values with the associated
/// constructor functions rather than the variants.
#[derive(Debug, Clone)]
pub enum GraphChange {
    /// Create a WorkGraph node.
    CreateWorkNode(Box<NewWorkNode>),
    /// Create a WorkGraph edge (cycle-checked for `depends_on`/`parent_of`).
    CreateWorkEdge(NewWorkEdge),
    /// Remove a WorkGraph edge at an expected revision.
    RemoveWorkEdge {
        /// The edge to remove.
        edge_id: CanonicalId,
        /// The revision the caller observed.
        expected: Revision,
    },
    /// Apply a legal WorkNode transition at an expected revision.
    TransitionWorkNode {
        /// The node to transition.
        node_id: CanonicalId,
        /// The revision the caller observed.
        expected: Revision,
        /// The requested state.
        to: WorkNodeStatus,
    },
    /// Mark a node done through the verification path.
    VerificationPassed {
        /// The node to mark done.
        node_id: CanonicalId,
        /// The revision the caller observed.
        expected: Revision,
    },
    /// Create an AgentThread.
    CreateAgentThread(Box<NewAgentThread>),
    /// Delegate to a new child AgentThread and record the AgentGraph edge.
    Delegate {
        /// The delegating parent.
        parent_id: CanonicalId,
        /// The delegation to apply.
        request: Box<DelegationRequest>,
    },
    /// Apply a legal AgentThread transition.
    TransitionAgentThread {
        /// The thread to transition.
        thread_id: CanonicalId,
        /// The requested state.
        to: AgentThreadStatus,
    },
    /// Create a Run.
    CreateRun(Box<NewRun>),
    /// Apply a legal Run transition.
    TransitionRun {
        /// The run to transition.
        run_id: CanonicalId,
        /// The requested state.
        to: RunStatus,
        /// Typed terminal reason.
        terminal_reason: Option<String>,
    },
    /// Create a Turn at the next run sequence.
    CreateTurn {
        /// The parent run.
        run_id: CanonicalId,
        /// The turn to create.
        turn: NewTurn,
    },
    /// Apply a legal Turn transition.
    TransitionTurn {
        /// The turn to transition.
        turn_id: CanonicalId,
        /// The requested state.
        to: TurnStatus,
    },
    /// Create a Step at the next turn sequence.
    CreateStep {
        /// The parent turn.
        turn_id: CanonicalId,
        /// The step to create.
        step: NewStep,
    },
    /// Apply a legal Step transition.
    TransitionStep {
        /// The step to transition.
        step_id: CanonicalId,
        /// The requested state.
        to: StepStatus,
    },
    /// Append a new `started` Attempt.
    StartAttempt {
        /// The parent step.
        step_id: CanonicalId,
        /// The generation the attempt is dispatched under.
        generation: quansio_core::Generation,
    },
    /// Finish an Attempt.
    FinishAttempt {
        /// The attempt to finish.
        attempt_id: CanonicalId,
        /// The finishing state.
        status: AttemptStatus,
        /// Typed error.
        error: Option<Value>,
    },
}

impl GraphChange {
    /// A `CreateWorkNode` change.
    #[must_use]
    pub fn create_work_node(node: NewWorkNode) -> Self {
        Self::CreateWorkNode(Box::new(node))
    }

    /// A `CreateWorkEdge` change.
    #[must_use]
    pub fn create_work_edge(edge: NewWorkEdge) -> Self {
        Self::CreateWorkEdge(edge)
    }

    /// A `RemoveWorkEdge` change.
    #[must_use]
    pub fn remove_work_edge(edge_id: CanonicalId, expected: Revision) -> Self {
        Self::RemoveWorkEdge { edge_id, expected }
    }

    /// A `CreateAgentThread` change.
    #[must_use]
    pub fn create_agent_thread(thread: NewAgentThread) -> Self {
        Self::CreateAgentThread(Box::new(thread))
    }

    /// A `Delegate` change.
    #[must_use]
    pub fn delegate(parent_id: CanonicalId, request: DelegationRequest) -> Self {
        Self::Delegate {
            parent_id,
            request: Box::new(request),
        }
    }

    /// A `CreateRun` change.
    #[must_use]
    pub fn create_run(run: NewRun) -> Self {
        Self::CreateRun(Box::new(run))
    }

    /// The variant name, for diagnostics and typed rejection messages.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::CreateWorkNode(_) => "CreateWorkNode",
            Self::CreateWorkEdge(_) => "CreateWorkEdge",
            Self::RemoveWorkEdge { .. } => "RemoveWorkEdge",
            Self::TransitionWorkNode { .. } => "TransitionWorkNode",
            Self::VerificationPassed { .. } => "VerificationPassed",
            Self::CreateAgentThread(_) => "CreateAgentThread",
            Self::Delegate { .. } => "Delegate",
            Self::TransitionAgentThread { .. } => "TransitionAgentThread",
            Self::CreateRun(_) => "CreateRun",
            Self::TransitionRun { .. } => "TransitionRun",
            Self::CreateTurn { .. } => "CreateTurn",
            Self::TransitionTurn { .. } => "TransitionTurn",
            Self::CreateStep { .. } => "CreateStep",
            Self::TransitionStep { .. } => "TransitionStep",
            Self::StartAttempt { .. } => "StartAttempt",
            Self::FinishAttempt { .. } => "FinishAttempt",
        }
    }
}

/// What applying one [`GraphChange`] produced.
///
/// This is the seam CORE-005's `GraphTransaction` maps to RuntimeEvents: the store keeps
/// owning state, and the eventing layer needs the identities, states and revisions the
/// store already read and wrote — not a second read of the graph.
#[derive(Debug, Clone)]
pub(crate) enum AppliedChange {
    /// A WorkNode was created.
    WorkNodeCreated(WorkNode),
    /// A WorkEdge was created.
    WorkEdgeAdded(WorkEdge),
    /// A WorkEdge was removed, carrying the row as it was before deletion.
    WorkEdgeRemoved(WorkEdge),
    /// A WorkNode changed status.
    WorkNodeStatusChanged {
        /// The node after the transition.
        node: WorkNode,
        /// The status it held before.
        from: WorkNodeStatus,
        /// Whether the change went through the CompletionContract verification path.
        verified: bool,
    },
    /// An AgentThread was created in `PROVISIONED`.
    AgentThreadProvisioned(AgentThread),
    /// A delegation created a child AgentThread and an AgentGraph edge.
    Delegated {
        /// The delegating parent.
        parent_id: CanonicalId,
        /// The parent's controller generation.
        parent_generation: Generation,
        /// The child thread and the delegation edge.
        delegated: Box<Delegated>,
    },
    /// An AgentThread changed status.
    AgentThreadStatusChanged {
        /// The thread after the transition.
        thread: AgentThread,
        /// The status it held before.
        from: AgentThreadStatus,
    },
    /// A Run was created in `CREATED`.
    RunCreated(Run),
    /// A Run changed status.
    RunStatusChanged {
        /// The run after the transition.
        run: Run,
        /// The status it held before.
        from: RunStatus,
    },
}

/// A batch of graph changes applied atomically against one base revision.
#[derive(Debug, Clone)]
pub struct GraphBatch {
    /// Workspace whose graph revision guards the batch.
    pub workspace_id: String,
    /// The revision the caller observed when it built the batch.
    pub base_revision: Revision,
    /// Ordered changes; each item sees the effects of the previous items.
    pub changes: Vec<GraphChange>,
}

impl GraphBatch {
    /// An empty batch for a workspace, guarded by `base_revision`.
    #[must_use]
    pub fn new(workspace_id: impl Into<String>, base_revision: Revision) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            base_revision,
            changes: Vec::new(),
        }
    }

    /// Append a change.
    #[must_use]
    pub fn with(mut self, change: GraphChange) -> Self {
        self.changes.push(change);
        self
    }

    /// Append a change in place.
    pub fn push(&mut self, change: GraphChange) {
        self.changes.push(change);
    }
}

/// Result of an applied batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchOutcome {
    /// The workspace graph revision after the batch.
    pub revision: Revision,
    /// How many changes were applied.
    pub applied: usize,
}

impl GraphStore {
    /// Apply a batch atomically, using the structural delegation narrowing rule.
    ///
    /// # Errors
    /// Returns the first item's [`GraphError`] after rolling the whole batch back. A stale
    /// `base_revision` returns [`GraphError::RevisionConflict`] and applies nothing.
    pub async fn apply_batch(&self, batch: GraphBatch) -> Result<BatchOutcome, GraphError> {
        self.apply_batch_with(&StructuralDelegationCheck, batch)
            .await
    }

    /// Apply a batch atomically with an explicit delegation narrowing check.
    ///
    /// This is the documented seam CORE-005 wraps with event emission and RUN-005 supplies
    /// with the Capability Projection algebra; it performs no eventing itself.
    ///
    /// # Errors
    /// Returns the first item's [`GraphError`] after rolling the whole batch back.
    pub async fn apply_batch_with<C: crate::agent::DelegationNarrowingCheck>(
        &self,
        check: &C,
        batch: GraphBatch,
    ) -> Result<BatchOutcome, GraphError> {
        let mut tx = self.begin().await?;
        let (outcome, _evented) = self.apply_batch_in_tx(&mut tx, check, batch).await?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// Apply a batch on a transaction the caller already owns.
    ///
    /// Does everything [`GraphStore::apply_batch_with`] does except open and commit the
    /// transaction: the workspace check, the single graph-revision compare-and-set, every
    /// change in order, and the head update. The second return value is the applied change
    /// of every item that has a RuntimeEvent mapping, in batch order; changes with no
    /// mapping (turns, steps, attempts) are applied and reported as nothing, which is what
    /// keeps the state-only path working while `GraphTransaction` refuses to commit them
    /// unrecorded.
    ///
    /// # Errors
    /// Returns the first item's [`GraphError`]; the caller rolls back by dropping the
    /// transaction.
    pub(crate) async fn apply_batch_in_tx<C: crate::agent::DelegationNarrowingCheck>(
        &self,
        tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
        check: &C,
        batch: GraphBatch,
    ) -> Result<(BatchOutcome, Vec<AppliedChange>), GraphError> {
        Self::ensure_workspace_tx(tx, &self.tenant_id, &batch.workspace_id).await?;
        let current = Self::ensure_head_tx(tx, &self.tenant_id, &batch.workspace_id).await?;
        let next = current.check_and_bump(batch.base_revision).map_err(|_| {
            Self::revision_conflict(
                Entity::GraphHead,
                &batch.workspace_id,
                batch.base_revision,
                current,
            )
        })?;
        let mut evented = Vec::with_capacity(batch.changes.len());
        for change in &batch.changes {
            if let Some(applied) = self.apply_change_in_tx(tx, check, change).await? {
                evented.push(applied);
            }
        }
        sqlx::query(
            "UPDATE graph_heads SET revision = $1 WHERE tenant_id = $2 AND workspace_id = $3",
        )
        .bind(next.get() as i64)
        .bind(&self.tenant_id)
        .bind(&batch.workspace_id)
        .execute(&mut **tx)
        .await?;
        Ok((
            BatchOutcome {
                revision: next,
                applied: batch.changes.len(),
            },
            evented,
        ))
    }

    async fn apply_change_in_tx<C: crate::agent::DelegationNarrowingCheck>(
        &self,
        tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
        check: &C,
        change: &GraphChange,
    ) -> Result<Option<AppliedChange>, GraphError> {
        let applied = match change {
            GraphChange::CreateWorkNode(node) => Some(AppliedChange::WorkNodeCreated(
                self.create_node_tx(tx, (**node).clone()).await?,
            )),
            GraphChange::CreateWorkEdge(edge) => Some(AppliedChange::WorkEdgeAdded(
                self.create_edge_tx(tx, edge.clone()).await?,
            )),
            GraphChange::RemoveWorkEdge { edge_id, expected } => Some(
                AppliedChange::WorkEdgeRemoved(self.remove_edge_tx(tx, edge_id, *expected).await?),
            ),
            GraphChange::TransitionWorkNode {
                node_id,
                expected,
                to,
            } => {
                let before = self.lock_node_tx(tx, node_id).await?;
                let node = self.transition_node_tx(tx, node_id, *expected, *to).await?;
                Some(AppliedChange::WorkNodeStatusChanged {
                    node,
                    from: before.status,
                    verified: false,
                })
            }
            GraphChange::VerificationPassed { node_id, expected } => {
                let before = self.lock_node_tx(tx, node_id).await?;
                let node = self
                    .mark_verification_passed_tx(tx, node_id, *expected)
                    .await?;
                Some(AppliedChange::WorkNodeStatusChanged {
                    node,
                    from: before.status,
                    verified: true,
                })
            }
            GraphChange::CreateAgentThread(thread) => Some(AppliedChange::AgentThreadProvisioned(
                self.create_agent_thread_tx(tx, (**thread).clone()).await?,
            )),
            GraphChange::Delegate { parent_id, request } => {
                let parent = self.lock_agent_thread_tx(tx, parent_id).await?;
                let delegated = self.delegate_inner_tx(tx, check, &parent, request).await?;
                Some(AppliedChange::Delegated {
                    parent_id: parent.id,
                    parent_generation: parent.generation,
                    delegated: Box::new(delegated),
                })
            }
            GraphChange::TransitionAgentThread { thread_id, to } => {
                let before = self.lock_agent_thread_tx(tx, thread_id).await?;
                let thread = self.transition_agent_thread_tx(tx, thread_id, *to).await?;
                Some(AppliedChange::AgentThreadStatusChanged {
                    thread,
                    from: before.status,
                })
            }
            GraphChange::CreateRun(run) => Some(AppliedChange::RunCreated(
                self.create_run_tx(tx, (**run).clone()).await?,
            )),
            GraphChange::TransitionRun {
                run_id,
                to,
                terminal_reason,
            } => {
                let before = self.lock_run_tx(tx, run_id).await?;
                let run = self
                    .transition_run_tx(tx, run_id, *to, terminal_reason.clone())
                    .await?;
                Some(AppliedChange::RunStatusChanged {
                    run,
                    from: before.status,
                })
            }
            // No RuntimeEvent mapping yet (turns, steps, attempts). `GraphTransaction`
            // rejects these kinds before it opens a transaction; only the state-only
            // `apply_batch`/`apply_batch_with` path reaches them.
            GraphChange::CreateTurn { run_id, turn } => {
                self.create_turn_tx(tx, run_id, turn.clone()).await?;
                None
            }
            GraphChange::TransitionTurn { turn_id, to } => {
                self.transition_turn_tx(tx, turn_id, *to).await?;
                None
            }
            GraphChange::CreateStep { turn_id, step } => {
                self.create_step_tx(tx, turn_id, step.clone()).await?;
                None
            }
            GraphChange::TransitionStep { step_id, to } => {
                self.transition_step_tx(tx, step_id, *to).await?;
                None
            }
            GraphChange::StartAttempt {
                step_id,
                generation,
            } => {
                self.start_attempt_tx(tx, step_id, *generation).await?;
                None
            }
            GraphChange::FinishAttempt {
                attempt_id,
                status,
                error,
            } => {
                self.finish_attempt_tx(tx, attempt_id, *status, error.clone())
                    .await?;
                None
            }
        };
        Ok(applied)
    }
}
