//! GraphTransaction: the only way to mutate WorkGraph, AgentGraph and StateGraph.
//!
//! DOMAIN.md §0 defines a GraphTransaction as "the only way to mutate
//! WorkGraph/AgentGraph/StateGraph: atomic, revision-checked, event-emitting". This module
//! implements exactly that, on top of the two canonical owners instead of duplicating
//! either:
//!
//! * `crates/graph`'s [`GraphStore::apply_batch_in_tx`] applies the changes and performs
//!   the single compare-and-set of the workspace graph revision (`graph_heads`);
//! * `crates/events`' [`EventStore`] opens the transaction, assigns the tenant-monotonic
//!   `sequence` and writes `runtime_events` plus the transactional outbox.
//!
//! There is no second graph store, no second event store and no second revision
//! mechanism: the events store's transaction *is* the graph transaction.
//!
//! Atomicity. The state change and every RuntimeEvent are written on one PostgreSQL
//! transaction. A change the store rejects, a batch that cannot be recorded, or a stale
//! `base_revision` makes the whole transaction roll back, so a partially applied graph and
//! an event that does not match a committed mutation are both impossible.
//!
//! ```no_run
//! # use quansio_core::Revision;
//! # use quansio_events::{Actor, EventStore};
//! # use quansio_graph::{GraphBatch, GraphChange, GraphStore, NewWorkNode, WorkNodeKind};
//! # use quansio_graph::transaction::{GraphTransaction, TransactionContext};
//! # async fn example(pool: sqlx::PgPool, correlation_id: quansio_core::CorrelationId) -> Result<(), Box<dyn std::error::Error>> {
//! let graph = GraphStore::new(pool.clone(), "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")?;
//! let workspace = graph.graph_revision("ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC").await?;
//! let transaction = GraphTransaction::new(
//!     graph,
//!     EventStore::new(pool),
//!     TransactionContext::agent(
//!         "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
//!         correlation_id,
//!     ),
//! );
//! let outcome = transaction
//!     .apply(
//!         GraphBatch::new("ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", workspace).with(
//!             GraphChange::create_work_node(NewWorkNode::new(
//!                 "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
//!                 WorkNodeKind::Task,
//!                 "write the report",
//!                 serde_json::json!({"kind": "agent", "id": "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"}),
//!             )),
//!         ),
//!     )
//!     .await?;
//! assert_eq!(outcome.event_ids.len(), 1);
//! # Ok(())
//! # }
//! ```

use std::sync::{Arc, Mutex, PoisonError};

use quansio_core::{CausationId, CommandId, CorrelationId, EventId, Revision};
use quansio_events::{Actor, EventBatch, EventError, EventStore};
use sqlx::{Postgres, Transaction};

use crate::agent::{DelegationNarrowingCheck, StructuralDelegationCheck};
use crate::batch::GraphBatch;
use crate::error::GraphError;
use crate::store::GraphStore;

mod event;
mod plan;

pub use plan::{
    PlanCapabilityNarrowingCheck, PlanEdgeRemoval, PlanNodeUpdate, PlanProposal, PlanProposer,
    PlanRejection, StructuralPlanCapabilityCheck, DEFAULT_MAX_PLAN_NODES,
};

use event::AggregateVersions;
use plan::PlanApplied;

/// The identity and replay-safety metadata stamped on every RuntimeEvent a
/// [`GraphTransaction`] emits (DOMAIN.md §1.2, §9.1).
///
/// Every event in one transaction shares the same `correlation_id` — everything caused by
/// one external input does — and the same direct cause, which is the command or event that
/// caused the transaction.
#[derive(Debug, Clone)]
pub struct TransactionContext {
    /// Who caused the mutation.
    pub actor: Actor,
    /// Everything caused by one external input shares this id.
    pub correlation_id: CorrelationId,
    /// The event or command that directly caused this transaction.
    pub causation_id: Option<CausationId>,
    /// The client command that carried the mutation, when there is one.
    pub command_id: Option<CommandId>,
}

impl TransactionContext {
    /// A context for the given actor and correlation id.
    #[must_use]
    pub const fn new(actor: Actor, correlation_id: CorrelationId) -> Self {
        Self {
            actor,
            correlation_id,
            causation_id: None,
            command_id: None,
        }
    }

    /// A context for a user action (`usr_…`).
    #[must_use]
    pub fn user(user_id: impl Into<String>, correlation_id: CorrelationId) -> Self {
        Self::new(Actor::user(user_id), correlation_id)
    }

    /// A context for an agent action (`ath_…`).
    #[must_use]
    pub fn agent(agent_thread_id: impl Into<String>, correlation_id: CorrelationId) -> Self {
        Self::new(Actor::agent(agent_thread_id), correlation_id)
    }

    /// A context for the runtime acting on its own.
    #[must_use]
    pub fn system(subsystem: impl Into<String>, correlation_id: CorrelationId) -> Self {
        Self::new(Actor::system(subsystem), correlation_id)
    }

    /// Record the client command that carried this mutation.
    #[must_use]
    pub const fn with_command_id(mut self, command_id: CommandId) -> Self {
        self.command_id = Some(command_id);
        self
    }

    /// Record the event or command that directly caused this transaction.
    #[must_use]
    pub const fn with_causation_id(mut self, causation_id: CausationId) -> Self {
        self.causation_id = Some(causation_id);
        self
    }
}

/// What a committed [`GraphTransaction`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionOutcome {
    /// The workspace graph revision after the transaction.
    pub revision: Revision,
    /// How many changes were applied.
    pub applied: usize,
    /// The identities of the RuntimeEvents the transaction committed, in order.
    pub event_ids: Vec<EventId>,
}

/// A graph mutation rejected before it could be committed.
#[derive(Debug, thiserror::Error)]
pub enum GraphTransactionError {
    /// The graph store rejected a change; nothing was committed.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// The event store failed; nothing was committed.
    #[error("runtime event store error: {0}")]
    Event(#[from] EventError),
    /// The batch carries no change, so there is nothing to record.
    #[error("a GraphTransaction must carry at least one change")]
    EmptyBatch,
    /// A change has no RuntimeEvent mapping in DOMAIN.md §9.2.
    #[error("graph change {change} has no RuntimeEvent mapping: {detail}")]
    UnsupportedChange {
        /// The [`GraphChange::kind`] that was refused.
        change: &'static str,
        /// Why it has no mapping.
        detail: String,
    },
    /// A PlanProposal did not survive validation (DOMAIN.md §4.5).
    #[error("plan proposal {proposal_id} rejected: {rejection}")]
    PlanRejected {
        /// The rejected proposal.
        proposal_id: String,
        /// The rule it broke.
        rejection: PlanRejection,
    },
}

impl GraphTransactionError {
    /// The DOMAIN.md §15 error code this error maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Graph(error) => error.code(),
            Self::Event(EventError::UnknownEventFamily { .. })
            | Self::Event(EventError::MalformedEventType { .. })
            | Self::Event(EventError::Json(_)) => "VALIDATION_SCHEMA",
            Self::Event(_) => "INTERNAL",
            Self::EmptyBatch | Self::UnsupportedChange { .. } => "VALIDATION_SCHEMA",
            Self::PlanRejected { rejection, .. } => rejection.code(),
        }
    }
}

/// The single entry point for atomic, revision-checked, event-emitting graph mutation.
///
/// A `GraphTransaction` binds one tenant's [`GraphStore`], the [`EventStore`] that writes
/// the RuntimeEvents, and the [`TransactionContext`] every emitted event carries. It owns
/// no state of its own: each call opens one PostgreSQL transaction and commits or rolls
/// back as a unit.
#[derive(Clone)]
pub struct GraphTransaction {
    graph: Arc<GraphStore>,
    events: EventStore,
    context: TransactionContext,
}

impl GraphTransaction {
    /// Bind a graph store, an event store and an event context for one tenant.
    ///
    /// The graph store and the event store must address the same database and tenant; the
    /// event store re-validates the tenant id when it opens the transaction.
    #[must_use]
    pub fn new(graph: GraphStore, events: EventStore, context: TransactionContext) -> Self {
        Self {
            graph: Arc::new(graph),
            events,
            context,
        }
    }

    /// The graph store the transaction mutates.
    #[must_use]
    pub fn graph(&self) -> &GraphStore {
        &self.graph
    }

    /// The event store the transaction commits through.
    #[must_use]
    pub fn events(&self) -> &EventStore {
        &self.events
    }

    /// The context stamped on every emitted RuntimeEvent.
    #[must_use]
    pub fn context(&self) -> &TransactionContext {
        &self.context
    }

    /// Apply a batch atomically, using the structural delegation narrowing rule.
    ///
    /// # Errors
    /// Returns [`GraphTransactionError::UnsupportedChange`] when a change has no
    /// RuntimeEvent mapping, [`GraphTransactionError::Graph`] when the store rejects a
    /// change (a stale `base_revision` returns `CONFLICT_REVISION`), and
    /// [`GraphTransactionError::Event`] when the event store fails.
    pub async fn apply(
        &self,
        batch: GraphBatch,
    ) -> Result<TransactionOutcome, GraphTransactionError> {
        self.apply_with(&StructuralDelegationCheck, batch).await
    }

    /// Apply a batch atomically with an explicit delegation narrowing check.
    ///
    /// This is the RUN-005 seam: the Capability Projection algebra is supplied here and
    /// runs inside the same transaction as the change it authorises.
    ///
    /// # Errors
    /// See [`GraphTransaction::apply`].
    ///
    /// The narrowing check is cloned into the transaction, so it must be an owned value.
    pub async fn apply_with<C: DelegationNarrowingCheck + Clone + Send + Sync + 'static>(
        &self,
        check: &C,
        batch: GraphBatch,
    ) -> Result<TransactionOutcome, GraphTransactionError> {
        self.validate(&batch)?;
        self.commit(batch, None, check.clone()).await
    }

    /// Validate and apply a PlanProposal as one GraphTransaction (DOMAIN.md §4.5).
    ///
    /// Uses [`StructuralPlanCapabilityCheck`]; RUN-005 calls
    /// [`GraphTransaction::apply_plan_with`] with the Capability Projection algebra.
    ///
    /// # Errors
    /// Returns [`GraphTransactionError::PlanRejected`] for every §4.5 rule the proposal
    /// breaks. The WorkGraph and the event log are untouched on rejection.
    pub async fn apply_plan(
        &self,
        proposer: &PlanProposer,
        proposal: PlanProposal,
    ) -> Result<TransactionOutcome, GraphTransactionError> {
        self.apply_plan_with(&StructuralPlanCapabilityCheck, proposer, proposal)
            .await
    }

    /// Validate and apply a PlanProposal with an explicit capability narrowing check.
    ///
    /// # Errors
    /// See [`GraphTransaction::apply_plan`].
    pub async fn apply_plan_with<C: PlanCapabilityNarrowingCheck>(
        &self,
        check: &C,
        proposer: &PlanProposer,
        proposal: PlanProposal,
    ) -> Result<TransactionOutcome, GraphTransactionError> {
        let validated = plan::validate_plan(&self.graph, check, proposer, &proposal).await?;
        self.validate(&validated.batch)?;
        self.commit(
            validated.batch,
            Some(validated.applied),
            StructuralDelegationCheck,
        )
        .await
    }

    /// Refuse a batch that cannot be recorded in full, before anything is written.
    fn validate(&self, batch: &GraphBatch) -> Result<(), GraphTransactionError> {
        if batch.changes.is_empty() {
            return Err(GraphTransactionError::EmptyBatch);
        }
        for change in &batch.changes {
            event::check_mappable(change)?;
        }
        Ok(())
    }

    /// Apply the batch and its events in one transaction, or write nothing.
    async fn commit<C: DelegationNarrowingCheck + Send + Sync + 'static>(
        &self,
        batch: GraphBatch,
        plan: Option<PlanApplied>,
        check: C,
    ) -> Result<TransactionOutcome, GraphTransactionError> {
        let tenant_id = self.graph.tenant_id().to_string();
        let graph = Arc::clone(&self.graph);
        let context = self.context.clone();
        // `commit_mutation_tx` reports a rejection as an `EventError`, because its
        // mutation closure is typed by the event store. The rejection is a graph verdict,
        // so it is carried out of the transaction in this slot and returned as itself.
        let rejection: Arc<Mutex<Option<GraphTransactionError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(&tenant_id, move |tx, events| {
                Box::pin(async move {
                    match Self::apply_on(tx, &graph, &context, &check, batch, plan, events).await {
                        Ok(outcome) => Ok(outcome),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: "crates/graph",
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(EventError::MutationRejected { .. }) => {
                let error = rejection
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                match error {
                    Some(error) => Err(error),
                    None => Err(GraphTransactionError::Event(EventError::MutationRejected {
                        owner: "crates/graph",
                        message: "transaction rejected without a recorded reason".to_string(),
                    })),
                }
            }
            Err(error) => Err(GraphTransactionError::Event(error)),
        }
    }

    /// Apply the graph changes and stage their RuntimeEvents on the open transaction.
    async fn apply_on<C: DelegationNarrowingCheck + Send + Sync>(
        tx: &mut Transaction<'static, Postgres>,
        graph: &GraphStore,
        context: &TransactionContext,
        check: &C,
        batch: GraphBatch,
        plan: Option<PlanApplied>,
        events: &mut EventBatch,
    ) -> Result<TransactionOutcome, GraphTransactionError> {
        let changes = batch.changes.len();
        let workspace_id = batch.workspace_id.clone();
        let (outcome, evented) = graph.apply_batch_in_tx(tx, check, batch).await?;
        if evented.len() != changes {
            // `validate` refuses unmapped kinds, so this can only mean a mapping and the
            // store's change list have drifted apart: refuse rather than commit a change
            // the event stream would not record.
            return Err(GraphTransactionError::UnsupportedChange {
                change: "GraphBatch",
                detail: format!(
                    "{changes} changes were applied but {} have an event",
                    evented.len()
                ),
            });
        }

        let tenant_id = graph.tenant_id();
        let mut versions = AggregateVersions::default();
        let mut event_ids = Vec::new();
        for applied in &evented {
            for draft in event::drafts_for(applied, context, tenant_id, tx, &mut versions).await? {
                event_ids.push(draft.event_id);
                events.emit(draft);
            }
        }
        if let Some(plan) = &plan {
            let draft = event::plan_applied_draft(
                context,
                &workspace_id,
                outcome.revision,
                plan.payload(outcome.revision),
            )?;
            event_ids.push(draft.event_id);
            events.emit(draft);
        }
        Ok(TransactionOutcome {
            revision: outcome.revision,
            applied: outcome.applied,
            event_ids,
        })
    }
}
