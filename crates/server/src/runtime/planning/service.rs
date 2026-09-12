//! The planning service: ingest a raw model proposal, validate it, commit it, audit it
//! (RUN-003, DOMAIN.md §4.5, §5.6).
//!
//! [`Planner::propose`] is the seam the turn loop calls when a `ModelProposal` carries a
//! `plan_proposal`. It is the only entry point, and it never mutates the WorkGraph itself:
//! after compilation it hands the [`CompiledPlan`] to the [`PlanWorkspacePort`], whose
//! graph-backed implementation commits it as one `GraphTransaction`.
//!
//! Audit trail (DOMAIN.md §9.2), each emitted through the canonical transactional event
//! store exactly once:
//!
//! * `work.plan_proposed` — on ingestion, carrying the proposal identity and counts;
//! * `work.plan_rejected` — when the compiler or the port refuses the plan, carrying the
//!   DOMAIN.md §15 code and the typed reason;
//! * `work.plan_applied` — emitted by the graph transaction when the plan is accepted.
//!
//! A malformed proposal (not a `PlanProposal`, unknown kind/status, missing identity or
//! rationale) is refused before any graph is read and before any event is written: nothing
//! is written. Rejections that happen after ingestion are recorded on the audit trail but
//! still mutate no WorkGraph state.

use std::sync::Arc;

use quansio_core::EventId;
use quansio_events::{EventDraft, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::PgPool;

use super::compile::{compile, CompiledPlan, ContractSource, PlanProposerProjection};
use super::policy::max_plan_nodes;
use super::port::PlanWorkspacePort;
use super::wire::{parse_raw, validate_raw, RawPlanProposal};
use super::PlanError;
use crate::control::schema;
use crate::runtime::state_machine::{RuntimeError, RuntimeIdentity};

/// What a compiled and committed plan produced.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOutcome {
    /// Proposal identity.
    pub proposal_id: String,
    /// The workspace graph revision after the transaction.
    pub revision: u64,
    /// How many graph changes were applied.
    pub applied: usize,
    /// The RuntimeEvents the graph transaction committed, in order.
    pub event_ids: Vec<EventId>,
    /// Nodes created.
    pub nodes_added: usize,
    /// Nodes transitioned.
    pub nodes_updated: usize,
    /// Edges created.
    pub edges_added: usize,
    /// Edges removed.
    pub edges_removed: usize,
    /// Where each added node's CompletionContract came from, in proposal order.
    pub contract_sources: Vec<ContractSource>,
    /// The deterministic mutation set the plan compiled to.
    pub mutation_summary: Vec<String>,
}

/// Compiles and commits model plans through the canonical graph transaction.
pub struct Planner {
    pool: PgPool,
    events: EventStore,
    workspace: Arc<dyn PlanWorkspacePort>,
    identity: RuntimeIdentity,
}

impl Planner {
    /// Build a planner over a PostgreSQL pool and a WorkGraph port.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when `identity.tenant_id` is not canonical.
    pub fn new(
        pool: PgPool,
        workspace: Arc<dyn PlanWorkspacePort>,
        identity: RuntimeIdentity,
    ) -> Result<Self, RuntimeError> {
        schema::validate_tenant_id(&identity.tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool.clone()),
            pool,
            workspace,
            identity,
        })
    }

    /// The event store the audit trail is committed through.
    #[must_use]
    pub fn events(&self) -> &EventStore {
        &self.events
    }

    /// Ingest, validate and commit one raw plan proposal.
    ///
    /// # Errors
    /// Returns the typed [`PlanError`] for the first rule the proposal breaks. A malformed
    /// proposal writes nothing; a later rejection is recorded as `work.plan_rejected` and
    /// still leaves the WorkGraph untouched.
    pub async fn propose(
        &self,
        raw: &Value,
        proposer: PlanProposerProjection,
    ) -> Result<PlanOutcome, PlanError> {
        // Malformed input is refused before anything is read or written.
        let raw = parse_raw(raw)?;
        validate_raw(&raw)?;

        let snapshot = self.workspace.snapshot(&raw.workspace_id).await?;
        self.emit_plan_event(
            "work.plan_proposed",
            &raw.workspace_id,
            snapshot.revision.get(),
            proposed_payload(&raw),
        )
        .await?;

        let limit = max_plan_nodes(&self.pool, &self.identity.tenant_id, &raw.workspace_id).await?;
        let compiled = match compile(&raw, &snapshot, &proposer, limit) {
            Ok(compiled) => compiled,
            Err(error) => return Err(self.reject(&raw, snapshot.revision.get(), error).await),
        };

        let outcome = self.compiled_outcome(&compiled);
        match self.workspace.apply(compiled).await {
            Ok(committed) => Ok(PlanOutcome {
                revision: committed.revision,
                applied: committed.applied,
                event_ids: committed.event_ids,
                ..outcome
            }),
            Err(error) => Err(self.reject(&raw, snapshot.revision.get(), error).await),
        }
    }

    /// Record a plan verdict on the audit trail and return the rejection unchanged.
    ///
    /// Infrastructure failures are not plan verdicts, so they are returned without a
    /// `work.plan_rejected` event.
    async fn reject(&self, raw: &RawPlanProposal, revision: u64, error: PlanError) -> PlanError {
        if error.is_rejection() {
            if let Err(record) = self.record_rejection(raw, revision, &error).await {
                return record;
            }
        }
        error
    }

    /// The deterministic parts of the outcome, derived from the compiled plan.
    fn compiled_outcome(&self, compiled: &CompiledPlan) -> PlanOutcome {
        PlanOutcome {
            proposal_id: compiled.proposal_id.clone(),
            revision: compiled.base_revision.get(),
            applied: compiled.change_count(),
            event_ids: Vec::new(),
            nodes_added: compiled.nodes_add.len(),
            nodes_updated: compiled.nodes_update.len(),
            edges_added: compiled.edges_add.len(),
            edges_removed: compiled.edges_remove.len(),
            contract_sources: compiled
                .nodes_add
                .iter()
                .map(|node| node.contract_source.clone())
                .collect(),
            mutation_summary: compiled.mutation_summary(),
        }
    }

    /// Record a rejection as `work.plan_rejected` with its typed code and reason.
    async fn record_rejection(
        &self,
        raw: &RawPlanProposal,
        revision: u64,
        error: &PlanError,
    ) -> Result<(), PlanError> {
        let payload = json!({
            "proposal_id": raw.proposal_id,
            "run_id": raw.run_id,
            "workspace_id": raw.workspace_id,
            "base_revision": raw.base_revision,
            "code": error.code(),
            "reason": error.to_string(),
        });
        self.emit_plan_event("work.plan_rejected", &raw.workspace_id, revision, payload)
            .await
    }

    /// Commit one `work_graph` plan event through the canonical event store.
    async fn emit_plan_event(
        &self,
        event_type: &str,
        workspace_id: &str,
        version: u64,
        payload: Value,
    ) -> Result<(), PlanError> {
        let event_type = EventType::parse(event_type).map_err(|error| PlanError::Workspace {
            detail: error.to_string(),
        })?;
        let tenant_id = self.identity.tenant_id.clone();
        let actor = self.identity.actor.clone();
        let correlation_id = self.identity.correlation_id;
        let causation_id = self.identity.causation_id.clone();
        let command_id = self.identity.command_id;
        let aggregate_id = workspace_id.to_string();
        let workspace = workspace_id.to_string();
        self.events
            .commit_mutation(&tenant_id, move |_conn, batch| {
                Box::pin(async move {
                    let mut draft = EventDraft::new(
                        "work_graph",
                        aggregate_id,
                        version.max(1),
                        event_type,
                        correlation_id,
                        actor,
                    )
                    .with_workspace(workspace)
                    .with_payload(payload);
                    if let Some(command_id) = command_id {
                        draft = draft.with_command_id(command_id);
                    }
                    if let Some(causation_id) = causation_id {
                        draft = draft.with_causation_id(causation_id);
                    }
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await
            .map_err(|error| PlanError::Workspace {
                detail: format!("runtime event store: {error}"),
            })
    }
}

/// The `work.plan_proposed` payload (DOMAIN.md §9.2).
fn proposed_payload(raw: &RawPlanProposal) -> Value {
    json!({
        "proposal_id": raw.proposal_id,
        "run_id": raw.run_id,
        "workspace_id": raw.workspace_id,
        "base_revision": raw.base_revision,
        "nodes_add": raw.nodes_add.len(),
        "nodes_update": raw.nodes_update.len(),
        "edges_add": raw.edges_add.len(),
        "edges_remove": raw.edges_remove.len(),
        "rationale": raw.rationale,
    })
}
