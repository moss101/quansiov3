//! The authoritative runtime: Run/Turn/Step/Attempt lifecycle, the turn-loop entry
//! points and crash recovery (RUN-001, DOSSIER.md §5, DOMAIN.md §5).
//!
//! This module is the runtime that owns reality. The model only proposes: a proposal
//! enters through an injected [`turn_loop::ModelProposalSource`], and every accepted
//! change becomes a durable row plus its `run.*`/`turn.*`/`step.*` RuntimeEvent in one
//! PostgreSQL transaction through [`store::RuntimeStore`]. Legality is decided by the
//! DOMAIN.md §5.2–§5.5 tables in [`state`] before any write, generations fence every
//! mutation, and [`engine::RuntimeEngine::recover`] rebuilds the next safe action from
//! durable state alone.
//!
//! Relationship to `crates/graph`: `quansio-graph` depends on `quansio-server` for
//! `control::schema`, so `crates/server` cannot depend on it without a package cycle
//! (see `crate::scheduler::dispatch`). The runtime therefore owns the StateGraph rows for
//! Runs, Turns, Steps and Attempts and commits them through
//! `quansio_events::EventStore::commit_mutation_tx` — the same transactional event
//! primitive `GraphTransaction` builds on — with the same DOMAIN state values and the
//! same `run.*` event names as `crates/graph`'s mapping.

use quansio_core::{CausationId, CommandId, CoreError, CorrelationId, Generation};
use quansio_events::{Actor, EventError};

use crate::control::schema::SchemaError;

mod engine;
mod state;
mod store;
mod turn_loop;

pub use engine::{RecoveryOutcome, RuntimeEngine};
pub use state::{AttemptStatus, RunStatus, RunTriggerKind, StepKind, StepStatus, TurnStatus};
pub use store::{
    Attempt, Budget, NewRun, NewStep, Run, RuntimeStore, Step, Turn, TurnInput, WaitResolution,
    DEFAULT_MAX_STEPS,
};
pub use turn_loop::{
    CompletionClaim, DelegationContext, DelegationOutcome, DelegationPort, DelegationRequest,
    ModelCallRequest, ModelProposal, ModelProposalSource, ProposedQuestion, ProposedToolCall,
    QuestionContext, QuestionOutcome, QuestionPort, ToolDispatchOutcome, ToolDispatchPort,
    ToolDispatchRequest, TurnOutcome, UnavailableDelegation, UnavailableModelProposalSource,
    UnavailableQuestions, UnavailableToolDispatch, UnavailableVerification, VerificationContext,
    VerificationOutcome, VerificationPort, DELEGATION_OWNER, MEMORY_OWNER, MODEL_GATEWAY_OWNER,
    PLAN_VALIDATION_OWNER, QUESTION_OWNER, TOOL_DISPATCH_OWNER, VERIFICATION_OWNER,
};

/// Identity and replay-safety metadata stamped on every RuntimeEvent a runtime mutation
/// commits (DOMAIN.md §1.2, §9.1).
#[derive(Debug, Clone)]
pub struct RuntimeIdentity {
    /// Owning tenant; every statement is scoped to it and RLS enforces it.
    pub tenant_id: String,
    /// Who caused the mutation.
    pub actor: Actor,
    /// Everything caused by one external input shares this id.
    pub correlation_id: CorrelationId,
    /// The event or command that directly caused this mutation.
    pub causation_id: Option<CausationId>,
    /// The client command that carried this mutation, when there is one.
    pub command_id: Option<CommandId>,
}

impl RuntimeIdentity {
    /// An identity for the runtime acting on its own (`subsystem` names the component).
    #[must_use]
    pub fn system(
        tenant_id: impl Into<String>,
        subsystem: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            actor: Actor::system(subsystem),
            correlation_id,
            causation_id: None,
            command_id: None,
        }
    }

    /// An identity for an AgentThread causing the mutation (`ath_…`).
    #[must_use]
    pub fn agent(
        tenant_id: impl Into<String>,
        agent_thread_id: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            actor: Actor::agent(agent_thread_id),
            correlation_id,
            causation_id: None,
            command_id: None,
        }
    }

    /// Record the client command that carried this mutation.
    #[must_use]
    pub fn with_command_id(mut self, command_id: CommandId) -> Self {
        self.command_id = Some(command_id);
        self
    }

    /// Record the event or command that directly caused this mutation.
    #[must_use]
    pub fn with_causation_id(mut self, causation_id: CausationId) -> Self {
        self.causation_id = Some(causation_id);
        self
    }
}

/// Errors returned by the authoritative runtime (DOMAIN.md §15).
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// PostgreSQL rejected a statement or the transaction could not complete.
    #[error("runtime database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store rejected the commit; nothing was written.
    #[error("runtime event store: {0}")]
    Event(#[from] EventError),
    /// The tenant context could not be established.
    #[error("runtime schema error: {0}")]
    Schema(#[from] SchemaError),
    /// A canonical identity could not be parsed or generated.
    #[error("runtime identity error: {0}")]
    Core(#[from] CoreError),
    /// The requested entity is not visible in this tenant.
    #[error("{entity} {id} not found for tenant {tenant_id}")]
    NotFound {
        /// Entity kind.
        entity: &'static str,
        /// Requested identity.
        id: String,
        /// Tenant scope that was searched.
        tenant_id: String,
    },
    /// DOMAIN.md §5.2–§5.5 does not allow the transition; nothing was written.
    #[error("illegal {entity} transition: {from} -> {to}")]
    IllegalTransition {
        /// Entity kind (`run`, `turn`, `step`, `attempt`).
        entity: &'static str,
        /// State the row held.
        from: String,
        /// Requested state.
        to: String,
    },
    /// The mutation carried a generation behind the row's generation; nothing was written.
    #[error("fenced: received generation {received} is behind current generation {current}")]
    FencedStaleGeneration {
        /// Generation the caller carried.
        received: u64,
        /// Generation the row holds.
        current: u64,
    },
    /// A compare-and-set found the row changed concurrently.
    #[error("state conflict on {entity} {id}")]
    StateConflict {
        /// Entity kind.
        entity: &'static str,
        /// Entity identity.
        id: String,
    },
    /// A `WAITING_*` run was offered a resolution that is not the one it is waiting for.
    #[error("run {run_id} waits for {expected}, not {received}")]
    WaitMismatch {
        /// Parked run.
        run_id: String,
        /// Wait the run actually holds.
        expected: String,
        /// Resolution that was offered.
        received: String,
    },
    /// The behaviour a proposal needs belongs to a task that is not merged yet.
    #[error("seam {seam} is not available yet; owned by {owner}")]
    SeamNotAvailable {
        /// The missing seam.
        seam: &'static str,
        /// The task that owns it.
        owner: &'static str,
    },
    /// A persisted value is not decodable.
    #[error("unknown {entity} state {value:?}")]
    UnknownState {
        /// Entity kind.
        entity: &'static str,
        /// The rejected value.
        value: String,
    },
    /// The AgentThread lifecycle policy forbids a transition the DOMAIN.md §5.1 state
    /// table would otherwise allow (RUN-002).
    #[error("agent thread lifecycle policy {rule} forbids {from} -> {to}")]
    LifecyclePolicy {
        /// State the row held.
        from: String,
        /// Requested state.
        to: String,
        /// Named policy rule that refused it.
        rule: &'static str,
    },
    /// A delegation would widen authority; nothing was written (DOMAIN.md §6.3, RUN-002).
    #[error("delegation widens capability: {detail}")]
    CapabilityWideningRejected {
        /// Why the delegation was not a narrowing.
        detail: String,
    },
    /// A handoff record cannot complete or roll back from the state it holds (RUN-002).
    #[error("handoff {handoff_id} is {status}")]
    HandoffNotRecoverable {
        /// Handoff record id (`ahf_…`).
        handoff_id: String,
        /// Status the record held.
        status: String,
    },
    /// A caller-supplied argument is invalid.
    #[error("invalid runtime argument: {0}")]
    InvalidArgument(String),
    /// The durable protocol state could not be read or written.
    #[error("protocol state: {0}")]
    ProtocolState(String),
    /// JSON encoding failed.
    #[error("runtime JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl RuntimeError {
    /// The DOMAIN.md §15 error code this error maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::IllegalTransition { .. } | Self::LifecyclePolicy { .. } => {
                "RUNTIME_ILLEGAL_TRANSITION"
            }
            Self::FencedStaleGeneration { .. } => "FENCED_STALE_GENERATION",
            Self::WaitMismatch { .. }
            | Self::StateConflict { .. }
            | Self::HandoffNotRecoverable { .. } => "CONFLICT_STATE",
            Self::CapabilityWideningRejected { .. } => "CAPABILITY_DENIED",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::InvalidArgument(_) | Self::Json(_) => "VALIDATION_SCHEMA",
            Self::Database(_)
            | Self::Event(_)
            | Self::Schema(_)
            | Self::Core(_)
            | Self::UnknownState { .. }
            | Self::ProtocolState(_)
            | Self::SeamNotAvailable { .. } => "INTERNAL",
        }
    }

    /// An unrecognised stored state value.
    #[must_use]
    pub fn unknown_state(entity: &'static str, value: &str) -> Self {
        Self::UnknownState {
            entity,
            value: value.to_string(),
        }
    }

    /// A seam whose owning task is not merged yet.
    #[must_use]
    pub const fn seam_not_available(seam: &'static str, owner: &'static str) -> Self {
        Self::SeamNotAvailable { seam, owner }
    }
}

/// Reject a mutation whose generation is behind the row's current generation.
///
/// Fencing runs before any write, so a stale caller changes nothing (DOMAIN.md §1.2).
pub(crate) fn fence(current: Generation, received: Generation) -> Result<(), RuntimeError> {
    current
        .accept(received)
        .map(|_| ())
        .map_err(|_| RuntimeError::FencedStaleGeneration {
            received: received.get(),
            current: current.get(),
        })
}
