//! The turn loop's injected seams and the proposal shape (DOMAIN.md §5.6, §7.4).
//!
//! The model proposes and the runtime commits: the loop parses a [`ModelProposal`] here
//! and routes every part through a typed port. Tool calls go to [`ToolDispatchPort`],
//! delegations to [`DelegationPort`] and completion claims to [`VerificationPort`]. The
//! default implementations of those ports answer with
//! [`RuntimeError::SeamNotAvailable`] naming the task that owns the missing behaviour
//! (RUN-002, RUN-008, RUN-011); they never fabricate a success. A conformance stub or a
//! later task supplies the real port and the same loop starts committing real steps.
//!
//! Model fulfilment itself belongs to the server-side model gateway (INT-002), so the
//! default [`ModelProposalSource`] reports that owner.

use async_trait::async_trait;
use serde_json::Value;

use super::{RunStatus, RunTriggerKind, RuntimeError};

/// Task that owns the server-side model gateway (DOMAIN.md §11.1).
pub const MODEL_GATEWAY_OWNER: &str = "INT-002";
/// Task that owns plan validation (DOMAIN.md §4.5).
pub const PLAN_VALIDATION_OWNER: &str = "RUN-003";
/// Task that owns the ToolCall protocol and dispatch (DOMAIN.md §7.4).
pub const TOOL_DISPATCH_OWNER: &str = "RUN-011";
/// Task that owns AgentThread delegation (DOMAIN.md §5.1).
pub const DELEGATION_OWNER: &str = "RUN-002";
/// Task that owns CompletionContract verification (DOMAIN.md §4.4).
pub const VERIFICATION_OWNER: &str = "RUN-008";
/// Task that owns the human question protocol (DOMAIN.md §3.4).
pub const QUESTION_OWNER: &str = "RUN-011";
/// Task that owns gated memory writes (DOMAIN.md §11.4).
pub const MEMORY_OWNER: &str = "INT-007";

/// What the runtime asks a model for, one model call per turn step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCallRequest {
    /// Run the call belongs to.
    pub run_id: String,
    /// Turn the call belongs to.
    pub turn_id: String,
    /// `model_call` Step the call is recorded under.
    pub step_id: String,
    /// Input kind that started the turn.
    pub input_kind: RunTriggerKind,
    /// Turn-monotonic step sequence of the model call.
    pub step_seq: i32,
}

/// One tool call proposed by the model (DOMAIN.md §7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct ProposedToolCall {
    /// Model-supplied call id.
    pub call_id: String,
    /// Registered tool name.
    pub tool: String,
    /// Proposed arguments; validated by the owning tool contract, never here.
    pub args: Value,
}

impl ProposedToolCall {
    /// A proposed call.
    #[must_use]
    pub fn new(call_id: impl Into<String>, tool: impl Into<String>, args: Value) -> Self {
        Self {
            call_id: call_id.into(),
            tool: tool.into(),
            args,
        }
    }
}

/// A completion claim the model proposes; only verification may accept it (DOMAIN.md §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionClaim {
    /// Model-supplied claim id.
    pub claim_id: String,
    /// Human-readable summary of what was done.
    pub summary: String,
    /// Evidence ids the model cites.
    pub evidence_ids: Vec<String>,
}

/// A delegation the model proposes (DOMAIN.md §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationRequest {
    /// Model-supplied request id.
    pub request_id: String,
    /// WorkNode the worker should execute, when the model names one.
    pub work_node_id: Option<String>,
    /// Scoped instruction for the worker.
    pub instruction: String,
}

/// A human question the model proposes (DOMAIN.md §3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedQuestion {
    /// Model-supplied question id.
    pub question_id: String,
    /// Question text shown to the user.
    pub prompt: String,
    /// Question kind (`free_text`, `single_choice`, `multi_choice`, `confirm`).
    pub kind: String,
}

/// A parsed model proposal (DOMAIN.md §5.6).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelProposal {
    /// Assistant text, if the model produced any.
    pub assistant_text: Option<String>,
    /// Tool calls to validate and dispatch.
    pub tool_calls: Vec<ProposedToolCall>,
    /// PlanProposal awaiting RUN-003 validation.
    pub plan_proposal: Option<Value>,
    /// Completion claim awaiting RUN-008 verification.
    pub completion_claim: Option<CompletionClaim>,
    /// Memory candidates awaiting INT-007 gated writes.
    pub memory_candidates: Vec<Value>,
    /// Human question awaiting the RUN-011 question protocol.
    pub question: Option<ProposedQuestion>,
    /// Delegations awaiting RUN-002.
    pub delegate_requests: Vec<DelegationRequest>,
}

/// What a tool dispatch produced (DOMAIN.md §7.4).
#[derive(Debug, Clone, PartialEq)]
pub enum ToolDispatchOutcome {
    /// The tool finished; evidence and the settled effect are recorded.
    Completed {
        /// Evidence ids captured by the tool host.
        evidence_ids: Vec<String>,
        /// EffectRecord that was settled, when the call was consequential.
        effect_id: Option<String>,
    },
    /// The tool ran and failed; the effect outcome is known.
    Failed {
        /// Typed tool error.
        error: Value,
    },
    /// The call was refused before any dispatch: unknown tool or field, an argument that
    /// failed its schema, or a capability/policy denial. The Step records the refusal and
    /// the turn continues so the model can correct itself within budget.
    Rejected {
        /// Quansio error code (DOMAIN.md §15).
        code: String,
        /// Why the call was refused.
        detail: String,
    },
    /// The run must park in a `WAITING_*` state before the call can proceed
    /// (approval, question, delegation or takeover). The dispatch has already persisted
    /// the protocol state that resolution matches.
    Parked {
        /// Waiting state the run holds.
        state: RunStatus,
        /// Key that resolution must match.
        wait_key: String,
        /// EffectRecord reserved for the parked call, when one exists.
        effect_id: Option<String>,
    },
    /// The tool was dispatched and its external outcome is unknown; it must be
    /// reconciled before any retry (DOMAIN.md §7.2).
    OutcomeUnknown {
        /// EffectRecord awaiting reconciliation.
        effect_id: Option<String>,
    },
}

/// What a delegation produced (DOMAIN.md §5.1).
#[derive(Debug, Clone, PartialEq)]
pub enum DelegationOutcome {
    /// A child AgentThread was spawned; the run parks in `WAITING_CHILD`.
    Spawned {
        /// Child AgentThread id (`ath_…`).
        child_agent_thread_id: String,
        /// WorkNode the child executes.
        work_node_id: Option<String>,
    },
    /// The delegation completed inline and its result is already available.
    Joined {
        /// Child result payload.
        result: Value,
    },
}

/// What verification of a completion claim produced (DOMAIN.md §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// The CompletionContract is satisfied.
    Verified {
        /// Evidence ids that support the accepted result.
        evidence_ids: Vec<String>,
    },
    /// Verification failed; the turn continues with actionable feedback.
    Rejected {
        /// What the deterministic or semantic checks found.
        feedback: String,
    },
}

/// Injected source of one model proposal (DOMAIN.md §5.6, §11.1).
#[async_trait]
pub trait ModelProposalSource: Send + Sync {
    /// Produce the next proposal for the request, or a typed error.
    async fn propose(&self, request: ModelCallRequest) -> Result<ModelProposal, RuntimeError>;
}

/// Injected tool dispatch seam (DOMAIN.md §7.4).
#[async_trait]
pub trait ToolDispatchPort: Send + Sync {
    /// Validate, authorize and dispatch one proposed tool call.
    async fn dispatch(
        &self,
        call: ProposedToolCall,
        request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError>;

    /// Continue a call that parked earlier instead of proposing it again.
    ///
    /// The default refuses: a port that cannot resume must not pretend the call happened.
    async fn resume(
        &self,
        _pending: crate::runtime::protocol_state::PendingToolCall,
        _request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "tool resume",
            TOOL_DISPATCH_OWNER,
        ))
    }
}

/// Identity the tool dispatch seam records on its effect (DOMAIN.md §7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDispatchRequest {
    /// Run that proposed the call.
    pub run_id: String,
    /// Turn that proposed the call.
    pub turn_id: String,
    /// Step the call is recorded under.
    pub step_id: String,
    /// Run generation the dispatch is fenced by.
    pub generation: u64,
}

/// Injected delegation seam (DOMAIN.md §5.1).
#[async_trait]
pub trait DelegationPort: Send + Sync {
    /// Spawn a worker with narrowed capability, or report the failure.
    async fn delegate(
        &self,
        request: DelegationRequest,
        context: DelegationContext,
    ) -> Result<DelegationOutcome, RuntimeError>;
}

/// Identity the delegation seam records on the child AgentThread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationContext {
    /// Delegating run.
    pub run_id: String,
    /// Delegating run generation.
    pub generation: u64,
}

/// Injected completion-verification seam (DOMAIN.md §4.4).
#[async_trait]
pub trait VerificationPort: Send + Sync {
    /// Verify a completion claim independently of the model that made it.
    async fn verify(
        &self,
        claim: CompletionClaim,
        context: VerificationContext,
    ) -> Result<VerificationOutcome, RuntimeError>;
}

/// Identity the verification seam records on the verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationContext {
    /// Run being verified.
    pub run_id: String,
    /// Turn the claim was made in.
    pub turn_id: String,
}

/// Identity the question seam records on the Question (DOMAIN.md §3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionContext {
    /// Run that asked.
    pub run_id: String,
    /// Turn that asked.
    pub turn_id: String,
    /// `question` Step the Question is recorded under.
    pub step_id: String,
    /// Run generation the ask is fenced by.
    pub generation: u64,
    /// Thread the Question is posted to, when the run has one.
    pub thread_id: Option<String>,
}

/// What the human question protocol produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionOutcome {
    /// A durable Question row exists and the run must park in `WAITING_QUESTION`.
    Asked {
        /// `q_…` question id.
        question_id: String,
    },
}

/// Injected human question protocol (DOMAIN.md §3.4, §5.6).
#[async_trait]
pub trait QuestionPort: Send + Sync {
    /// Persist the Question and return its id; the runtime parks the run.
    async fn ask(
        &self,
        question: ProposedQuestion,
        context: QuestionContext,
    ) -> Result<QuestionOutcome, RuntimeError>;
}

/// Default question seam: the question protocol is RUN-011.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableQuestions;

#[async_trait]
impl QuestionPort for UnavailableQuestions {
    async fn ask(
        &self,
        _question: ProposedQuestion,
        _context: QuestionContext,
    ) -> Result<QuestionOutcome, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "human question protocol",
            QUESTION_OWNER,
        ))
    }
}

/// Default model seam: the gateway is INT-002.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableModelProposalSource;

#[async_trait]
impl ModelProposalSource for UnavailableModelProposalSource {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "model gateway",
            MODEL_GATEWAY_OWNER,
        ))
    }
}

/// Default tool dispatch seam: the ToolCall protocol is RUN-011.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableToolDispatch;

#[async_trait]
impl ToolDispatchPort for UnavailableToolDispatch {
    async fn dispatch(
        &self,
        _call: ProposedToolCall,
        _request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "tool dispatch",
            TOOL_DISPATCH_OWNER,
        ))
    }
}

/// Default delegation seam: AgentThread delegation is RUN-002.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableDelegation;

#[async_trait]
impl DelegationPort for UnavailableDelegation {
    async fn delegate(
        &self,
        _request: DelegationRequest,
        _context: DelegationContext,
    ) -> Result<DelegationOutcome, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "delegation",
            DELEGATION_OWNER,
        ))
    }
}

/// Default verification seam: CompletionContract verification is RUN-008.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableVerification;

#[async_trait]
impl VerificationPort for UnavailableVerification {
    async fn verify(
        &self,
        _claim: CompletionClaim,
        _context: VerificationContext,
    ) -> Result<VerificationOutcome, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "completion verification",
            VERIFICATION_OWNER,
        ))
    }
}

/// What one turn-loop entry point committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Verification accepted the completion claim; the run reached `SUCCEEDED`.
    Succeeded {
        /// Run that succeeded.
        run_id: String,
        /// Turn the loop ran in.
        turn_id: String,
        /// Evidence that supported the accepted claim.
        evidence_ids: Vec<String>,
    },
    /// The turn finished without a completion claim.
    Completed {
        /// Turn the loop ran in.
        turn_id: String,
        /// Assistant text the model produced, when any.
        assistant_text: Option<String>,
    },
    /// The run parked in a `WAITING_*` state, released only by its matching resolution.
    Parked {
        /// Parked run.
        run_id: String,
        /// Turn the loop ran in.
        turn_id: String,
        /// Waiting state the run holds.
        state: RunStatus,
        /// Key that resolution must match.
        wait_key: String,
    },
    /// The turn hit the step budget; no further model call was made.
    BudgetExhausted {
        /// Turn the loop ran in.
        turn_id: String,
        /// Steps the turn had recorded.
        steps_used: u32,
        /// Budget the turn honoured.
        max_steps: u32,
    },
    /// A dispatched effect has an unknown outcome; the run is suspended for
    /// reconciliation and no retry is attempted (DOMAIN.md §7.2).
    EffectUnsettled {
        /// Run that was suspended.
        run_id: String,
        /// Turn the loop ran in.
        turn_id: String,
        /// EffectRecord awaiting reconciliation.
        effect_id: Option<String>,
    },
}
