//! The authoritative runtime engine: Run lifecycle, turn-loop entry points and recovery.
//!
//! A [`RuntimeEngine`] binds the durable [`RuntimeStore`] to the injected model, tool,
//! delegation and verification seams. Nothing advances executable work except a call
//! here: the model proposes through a seam, the engine validates against DOMAIN.md §5.2
//! and commits state plus its RuntimeEvent in one transaction.
//!
//! `run_turn` implements the *shape* of DOMAIN.md §5.6 — input, model call, typed
//! routing of the proposal, bounded iteration — and stops at the run's step budget with
//! [`TurnOutcome::BudgetExhausted`]. `recover` applies [`next_safe_action`] semantics:
//! cancellation is honoured, a `WAITING_*` run is only released by its matching
//! resolution, and an effect with an unknown outcome is reported for reconciliation
//! rather than retried. Recovery writes only through a transition, so calling it twice
//! cannot duplicate a transition or an event.

use std::sync::Arc;

use quansio_core::{CanonicalId, Generation};
use serde_json::json;
use sqlx::PgPool;

use crate::runtime::protocol_state::{
    is_unsettled, next_safe_action, NextAction, ProtocolState, WaitKind,
};

use super::state::{AttemptStatus, RunStatus, StepKind, StepStatus, TurnStatus};
use super::store::{
    Attempt, Budget, NewRun, NewStep, Run, RuntimeStore, Step, Turn, TurnInput, WaitResolution,
};
use super::turn_loop::{
    CompletionClaim, DelegationContext, DelegationOutcome, DelegationPort, ModelCallRequest,
    ModelProposalSource, ProposedToolCall, QuestionContext, QuestionOutcome, QuestionPort,
    ToolDispatchOutcome, ToolDispatchPort, ToolDispatchRequest, TurnOutcome, UnavailableDelegation,
    UnavailableModelProposalSource, UnavailableQuestions, UnavailableToolDispatch,
    UnavailableVerification, VerificationContext, VerificationOutcome, VerificationPort,
    MEMORY_OWNER, PLAN_VALIDATION_OWNER,
};
use super::{fence, RuntimeError, RuntimeIdentity};
use crate::runtime::turn_loop::parallel;

/// What `recover` concluded from durable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// The run is already terminal; recovery changed nothing.
    Terminal {
        /// Terminal state the run holds.
        status: RunStatus,
    },
    /// A cancellation requested before the crash was honoured.
    Cancelled {
        /// Run that was cancelled.
        run_id: String,
    },
    /// A dispatched effect has an unknown outcome; it must be reconciled, never retried.
    ReconcileEffect {
        /// EffectRecord awaiting reconciliation.
        effect_id: String,
        /// Tool call that reserved it.
        tool_call_id: String,
    },
    /// The run is parked and is only released by the resolution matching `key`.
    Waiting {
        /// `WAITING_*` state the run holds.
        state: RunStatus,
        /// Key the matching resolution must carry.
        key: String,
    },
    /// Nothing is pending; the turn loop may continue.
    Continue {
        /// Run that may continue.
        run_id: String,
    },
}

/// The runtime authority for one tenant.
#[derive(Clone)]
pub struct RuntimeEngine {
    store: RuntimeStore,
    model: Arc<dyn ModelProposalSource>,
    tools: Arc<dyn ToolDispatchPort>,
    delegation: Arc<dyn DelegationPort>,
    verification: Arc<dyn VerificationPort>,
    questions: Arc<dyn QuestionPort>,
}

impl RuntimeEngine {
    /// Build an engine with the default seams, which report the task that owns each
    /// missing behaviour instead of inventing a result.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the identity's tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        Ok(Self {
            store: RuntimeStore::new(pool, identity)?,
            model: Arc::new(UnavailableModelProposalSource),
            tools: Arc::new(UnavailableToolDispatch),
            delegation: Arc::new(UnavailableDelegation),
            verification: Arc::new(UnavailableVerification),
            questions: Arc::new(UnavailableQuestions),
        })
    }

    /// Install the model proposal source.
    #[must_use]
    pub fn with_model_source(mut self, source: Arc<dyn ModelProposalSource>) -> Self {
        self.model = source;
        self
    }

    /// Install the tool dispatch seam.
    #[must_use]
    pub fn with_tool_dispatch(mut self, port: Arc<dyn ToolDispatchPort>) -> Self {
        self.tools = port;
        self
    }

    /// Install the delegation seam.
    #[must_use]
    pub fn with_delegation(mut self, port: Arc<dyn DelegationPort>) -> Self {
        self.delegation = port;
        self
    }

    /// Install the completion-verification seam.
    #[must_use]
    pub fn with_verification(mut self, port: Arc<dyn VerificationPort>) -> Self {
        self.verification = port;
        self
    }

    /// Install the human question protocol (DOMAIN.md §3.4).
    #[must_use]
    pub fn with_question_port(mut self, port: Arc<dyn QuestionPort>) -> Self {
        self.questions = port;
        self
    }

    /// The durable store every transition goes through.
    #[must_use]
    pub fn store(&self) -> &RuntimeStore {
        &self.store
    }

    /// The event identity this engine stamps on its events.
    #[must_use]
    pub fn identity(&self) -> &RuntimeIdentity {
        self.store.identity()
    }

    /// Create a Run in `CREATED` (DOMAIN.md §5.2).
    ///
    /// # Errors
    /// Returns a database error when the workspace, WorkNode or AgentThread is missing.
    pub async fn create_run(&self, run: NewRun) -> Result<Run, RuntimeError> {
        self.store.create_run(run).await
    }

    /// Move a Run `CREATED → QUEUED`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] for any other source state.
    pub async fn enqueue(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        self.store.enqueue(run_id, generation).await
    }

    /// Move a Run `QUEUED → RUNNING`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] for any other source state.
    pub async fn start(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        self.store.start(run_id, generation).await
    }

    /// Apply one legal Run transition.
    ///
    /// # Errors
    /// Returns [`RuntimeError::FencedStaleGeneration`] or
    /// [`RuntimeError::IllegalTransition`]; nothing is written on error.
    pub async fn transition_run(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        to: RunStatus,
        terminal_reason: Option<String>,
    ) -> Result<Run, RuntimeError> {
        self.store
            .transition_run(run_id, generation, to, terminal_reason)
            .await
    }

    /// Cancel a Run authoritatively (DOMAIN.md §5.2).
    ///
    /// # Errors
    /// Returns [`RuntimeError::FencedStaleGeneration`] or
    /// [`RuntimeError::IllegalTransition`]; nothing is written on error.
    pub async fn cancel(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        self.store.cancel(run_id, generation).await
    }

    /// Suspend a Run with a typed reason.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] unless the Run is `RUNNING` or waiting.
    pub async fn suspend(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        reason: impl Into<String>,
    ) -> Result<Run, RuntimeError> {
        self.store.suspend(run_id, generation, reason).await
    }

    /// Resume a suspended Run; only the current generation may do so.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] when the Run is not `SUSPENDED`.
    pub async fn resume(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
    ) -> Result<Run, RuntimeError> {
        self.store.resume(run_id, generation).await
    }

    /// Release a parked Run with its matching resolution only.
    ///
    /// # Errors
    /// Returns [`RuntimeError::WaitMismatch`] when the resolution does not match.
    pub async fn resolve_wait(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        resolution: WaitResolution,
    ) -> Result<Run, RuntimeError> {
        self.store
            .resolve_wait(run_id, generation, resolution)
            .await
    }

    /// Start a Turn and emit `turn.started`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] when the Run is not `RUNNING`.
    pub async fn begin_turn(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        input: TurnInput,
    ) -> Result<Turn, RuntimeError> {
        self.store.begin_turn(run_id, generation, input).await
    }

    /// Finish a Turn and emit `turn.completed`/`turn.aborted`.
    ///
    /// # Errors
    /// Returns [`RuntimeError::IllegalTransition`] when the Turn is already finished.
    pub async fn finish_turn(
        &self,
        turn_id: &CanonicalId,
        generation: Generation,
        status: TurnStatus,
    ) -> Result<Turn, RuntimeError> {
        self.store.finish_turn(turn_id, generation, status).await
    }

    /// Recover after a crash or restart from durable state alone (DOSSIER.md §8).
    ///
    /// # Errors
    /// Returns a database error or [`RuntimeError::NotFound`] when the Run is not visible.
    pub async fn recover(&self, run_id: &CanonicalId) -> Result<RecoveryOutcome, RuntimeError> {
        let run = self.store.load_run(run_id).await?;
        if run.status.is_terminal() {
            return Ok(RecoveryOutcome::Terminal { status: run.status });
        }
        let state = self.store.load_protocol_state(run_id).await?;
        let action = match &state {
            Some(state) => next_safe_action(state),
            None => NextAction::Continue,
        };
        match action {
            NextAction::Cancel => {
                self.store.cancel(run_id, run.generation).await?;
                Ok(RecoveryOutcome::Cancelled {
                    run_id: run.id.to_string(),
                })
            }
            NextAction::ReconcileEffect {
                effect_id,
                tool_call_id,
            } => Ok(RecoveryOutcome::ReconcileEffect {
                effect_id,
                tool_call_id,
            }),
            NextAction::WaitApproval { approval_id } => Ok(RecoveryOutcome::Waiting {
                state: RunStatus::WaitingApproval,
                key: approval_id,
            }),
            NextAction::WaitQuestion { question_id } => Ok(RecoveryOutcome::Waiting {
                state: RunStatus::WaitingQuestion,
                key: question_id,
            }),
            NextAction::WaitChild {
                child_agent_thread_id,
            } => Ok(RecoveryOutcome::Waiting {
                state: RunStatus::WaitingChild,
                key: child_agent_thread_id,
            }),
            NextAction::AwaitHandback { session_id } => Ok(RecoveryOutcome::Waiting {
                state: RunStatus::WaitingTakeover,
                key: session_id,
            }),
            NextAction::WaitFor { wait } => Ok(RecoveryOutcome::Waiting {
                state: match wait.kind {
                    WaitKind::Timer => RunStatus::WaitingTimer,
                    WaitKind::Event => RunStatus::WaitingEvent,
                    WaitKind::Approval => RunStatus::WaitingApproval,
                    WaitKind::Question => RunStatus::WaitingQuestion,
                    WaitKind::Child => RunStatus::WaitingChild,
                    WaitKind::Takeover => RunStatus::WaitingTakeover,
                },
                key: wait.key,
            }),
            NextAction::Continue => {
                if run.status.is_waiting() {
                    // A parked run is only released by its matching resolution, even when
                    // the protocol state no longer names the wait explicitly.
                    Ok(RecoveryOutcome::Waiting {
                        state: run.status,
                        key: "unknown".to_string(),
                    })
                } else {
                    Ok(RecoveryOutcome::Continue {
                        run_id: run.id.to_string(),
                    })
                }
            }
        }
    }

    /// Run one turn: input → model proposal → typed routing → bounded iteration
    /// (DOMAIN.md §5.6).
    ///
    /// # Errors
    /// Returns [`RuntimeError::SeamNotAvailable`] when a proposal needs a seam whose
    /// owning task is not merged yet, [`RuntimeError::IllegalTransition`] when the Run is
    /// not `RUNNING`, and [`RuntimeError::FencedStaleGeneration`] for a stale generation.
    pub async fn run_turn(
        &self,
        run_id: &CanonicalId,
        generation: Generation,
        input: TurnInput,
    ) -> Result<TurnOutcome, RuntimeError> {
        let run = self.store.load_run(run_id).await?;
        fence(run.generation, generation)?;
        if run.status != RunStatus::Running {
            return Err(RuntimeError::IllegalTransition {
                entity: "run",
                from: run.status.as_db_str().to_string(),
                to: RunStatus::Running.as_db_str().to_string(),
            });
        }
        let budget = Budget::from_snapshot(&run.budget_snapshot)?;
        let turn = self
            .store
            .begin_turn(run_id, generation, input.clone())
            .await?;
        let mut steps_used: u32 = 0;

        // A run released from a WAITING_* state continues its recorded call instead of
        // asking the model to propose it again.
        if let Some(outcome) = self
            .resume_pending_calls(&run, &turn, generation, budget, &mut steps_used)
            .await?
        {
            return Ok(outcome);
        }

        loop {
            if steps_used >= budget.max_steps {
                return self
                    .exhaust_budget(&run, &turn, generation, steps_used, budget)
                    .await;
            }

            let model_step = self
                .store
                .record_step(&turn.id, generation, NewStep::new(StepKind::ModelCall))
                .await?;
            steps_used += 1;
            let (_, model_attempt) = self.store.dispatch_step(&model_step.id, generation).await?;
            let request = ModelCallRequest {
                run_id: run.id.to_string(),
                turn_id: turn.id.to_string(),
                step_id: model_step.id.to_string(),
                input_kind: input.kind,
                step_seq: model_step.seq,
            };
            let proposal = match self.model.propose(request).await {
                Ok(proposal) => {
                    self.store
                        .complete_step(
                            &model_step.id,
                            generation,
                            &model_attempt.id,
                            StepStatus::Completed,
                            AttemptStatus::Succeeded,
                            None,
                            Vec::new(),
                        )
                        .await?;
                    proposal
                }
                Err(error) => {
                    self.fail_step(&model_step.id, &model_attempt.id, generation, &error)
                        .await?;
                    self.abort_turn(&turn.id, generation).await?;
                    return Err(error);
                }
            };

            // Plan proposals belong to RUN-003; memory candidates to INT-007; the human
            // question protocol to RUN-011. None may be executed here.
            if proposal.plan_proposal.is_some() {
                self.abort_turn(&turn.id, generation).await?;
                return Err(RuntimeError::seam_not_available(
                    "plan validation",
                    PLAN_VALIDATION_OWNER,
                ));
            }
            if !proposal.memory_candidates.is_empty() {
                self.abort_turn(&turn.id, generation).await?;
                return Err(RuntimeError::seam_not_available(
                    "memory proposal",
                    MEMORY_OWNER,
                ));
            }

            if let Some(outcome) = self
                .dispatch_tool_calls(
                    &run,
                    &turn,
                    generation,
                    &proposal.tool_calls,
                    budget,
                    &mut steps_used,
                )
                .await?
            {
                return Ok(outcome);
            }

            for delegation in &proposal.delegate_requests {
                if steps_used >= budget.max_steps {
                    return self
                        .exhaust_budget(&run, &turn, generation, steps_used, budget)
                        .await;
                }
                let delegate_step = self
                    .store
                    .record_step(
                        &turn.id,
                        generation,
                        NewStep::new(StepKind::Delegate).with_ref(delegation.request_id.clone()),
                    )
                    .await?;
                steps_used += 1;
                let (_, attempt) = self
                    .store
                    .dispatch_step(&delegate_step.id, generation)
                    .await?;
                let context = DelegationContext {
                    run_id: run.id.to_string(),
                    generation: generation.get(),
                };
                match self.delegation.delegate(delegation.clone(), context).await {
                    Ok(DelegationOutcome::Spawned {
                        child_agent_thread_id,
                        ..
                    }) => {
                        self.store
                            .complete_step(
                                &delegate_step.id,
                                generation,
                                &attempt.id,
                                StepStatus::Completed,
                                AttemptStatus::Succeeded,
                                None,
                                Vec::new(),
                            )
                            .await?;
                        return self
                            .park_child(&run, &turn, generation, &child_agent_thread_id)
                            .await;
                    }
                    Ok(DelegationOutcome::Joined { result }) => {
                        self.store
                            .complete_step(
                                &delegate_step.id,
                                generation,
                                &attempt.id,
                                StepStatus::Completed,
                                AttemptStatus::Succeeded,
                                Some(result),
                                Vec::new(),
                            )
                            .await?;
                    }
                    Err(error) => {
                        self.fail_step(&delegate_step.id, &attempt.id, generation, &error)
                            .await?;
                        self.abort_turn(&turn.id, generation).await?;
                        return Err(error);
                    }
                }
            }

            if let Some(question) = &proposal.question {
                if steps_used >= budget.max_steps {
                    return self
                        .exhaust_budget(&run, &turn, generation, steps_used, budget)
                        .await;
                }
                let question_step = self
                    .store
                    .record_step(
                        &turn.id,
                        generation,
                        NewStep::new(StepKind::Wait).with_ref(question.question_id.clone()),
                    )
                    .await?;
                let (_, attempt) = self
                    .store
                    .dispatch_step(&question_step.id, generation)
                    .await?;
                let context = QuestionContext {
                    run_id: run.id.to_string(),
                    turn_id: turn.id.to_string(),
                    step_id: question_step.id.to_string(),
                    generation: generation.get(),
                    thread_id: None,
                };
                match self.questions.ask(question.clone(), context).await {
                    Ok(QuestionOutcome::Asked { question_id }) => {
                        self.store
                            .complete_step(
                                &question_step.id,
                                generation,
                                &attempt.id,
                                StepStatus::Completed,
                                AttemptStatus::Succeeded,
                                None,
                                Vec::new(),
                            )
                            .await?;
                        return self
                            .park_question(&run, &turn, generation, &question_id)
                            .await;
                    }
                    Err(error) => {
                        self.fail_step(&question_step.id, &attempt.id, generation, &error)
                            .await?;
                        self.abort_turn(&turn.id, generation).await?;
                        return Err(error);
                    }
                }
            }

            if let Some(claim) = &proposal.completion_claim {
                if steps_used >= budget.max_steps {
                    return self
                        .exhaust_budget(&run, &turn, generation, steps_used, budget)
                        .await;
                }
                return self
                    .verify_claim(&run, &turn, generation, claim, &mut steps_used)
                    .await;
            }

            if proposal.tool_calls.is_empty() && proposal.delegate_requests.is_empty() {
                self.store
                    .finish_turn(&turn.id, generation, TurnStatus::Completed)
                    .await?;
                return Ok(TurnOutcome::Completed {
                    turn_id: turn.id.to_string(),
                    assistant_text: proposal.assistant_text,
                });
            }
        }
    }

    /// Record, dispatch and settle the turn's proposed tool calls (DOMAIN.md §7.4).
    ///
    /// Calls the runtime judges independent are dispatched concurrently and their results
    /// are applied together in proposal order; every call gets its own durable Step and
    /// Attempt before dispatch, so a crash leaves them recoverable. Returns `Some` when the
    /// turn must end (parked, unknown outcome or budget exhausted).
    async fn dispatch_tool_calls(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        calls: &[ProposedToolCall],
        budget: Budget,
        steps_used: &mut u32,
    ) -> Result<Option<TurnOutcome>, RuntimeError> {
        for round in parallel::plan_rounds(calls) {
            let mut pending = Vec::with_capacity(round.len());
            for index in round {
                if *steps_used >= budget.max_steps {
                    let outcome = self
                        .exhaust_budget(run, turn, generation, *steps_used, budget)
                        .await?;
                    return Ok(Some(outcome));
                }
                let call = &calls[index];
                let tool_step = self
                    .store
                    .record_step(
                        &turn.id,
                        generation,
                        NewStep::new(StepKind::ToolCall).with_ref(call.call_id.clone()),
                    )
                    .await?;
                *steps_used += 1;
                let (_, attempt) = self.store.dispatch_step(&tool_step.id, generation).await?;
                let request = ToolDispatchRequest {
                    run_id: run.id.to_string(),
                    turn_id: turn.id.to_string(),
                    step_id: tool_step.id.to_string(),
                    generation: generation.get(),
                };
                pending.push((tool_step, attempt, call.clone(), request));
            }

            let futures = pending
                .iter()
                .map(|(_, _, call, request)| {
                    Box::pin(self.tools.dispatch(call.clone(), request.clone()))
                        as std::pin::Pin<
                            Box<
                                dyn std::future::Future<
                                        Output = Result<ToolDispatchOutcome, RuntimeError>,
                                    > + Send
                                    + '_,
                            >,
                        >
                })
                .collect();
            let results = parallel::join_all(futures).await;

            for ((tool_step, attempt, call, _), result) in pending.into_iter().zip(results) {
                let outcome = match result {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.fail_step(&tool_step.id, &attempt.id, generation, &error)
                            .await?;
                        self.abort_turn(&turn.id, generation).await?;
                        return Err(error);
                    }
                };
                if let Some(turn_outcome) = self
                    .apply_tool_outcome(run, turn, generation, &tool_step, &attempt, &call, outcome)
                    .await?
                {
                    return Ok(Some(turn_outcome));
                }
            }
        }
        Ok(None)
    }

    /// Continue calls that parked for a human decision (DOMAIN.md §5.6 "on resume continue
    /// loop").
    ///
    /// The runtime never re-proposes a parked call: it reloads the recorded call by its
    /// effect and either finishes it or, when the outcome is unsettled, reports it for
    /// reconciliation without dispatching anything.
    async fn resume_pending_calls(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        budget: Budget,
        steps_used: &mut u32,
    ) -> Result<Option<TurnOutcome>, RuntimeError> {
        let state = self.pending_state(run).await?;
        if state.pending_tool_calls.is_empty() {
            return Ok(None);
        }
        for pending in state.pending_tool_calls.clone() {
            let call = ProposedToolCall::new(
                pending.tool_call_id.clone(),
                pending.tool_name.clone(),
                json!({}),
            );
            if is_unsettled(&pending.effect_status) {
                let outcome = self
                    .park_unsettled_effect(
                        run,
                        turn,
                        generation,
                        &call,
                        Some(pending.effect_id.clone()),
                    )
                    .await?;
                return Ok(Some(outcome));
            }
            if *steps_used >= budget.max_steps {
                let outcome = self
                    .exhaust_budget(run, turn, generation, *steps_used, budget)
                    .await?;
                return Ok(Some(outcome));
            }
            let step = self
                .store
                .record_step(
                    &turn.id,
                    generation,
                    NewStep::new(StepKind::ToolCall).with_ref(pending.tool_call_id.clone()),
                )
                .await?;
            *steps_used += 1;
            let (_, attempt) = self.store.dispatch_step(&step.id, generation).await?;
            let request = ToolDispatchRequest {
                run_id: run.id.to_string(),
                turn_id: turn.id.to_string(),
                step_id: step.id.to_string(),
                generation: generation.get(),
            };
            let outcome = match self.tools.resume(pending.clone(), request).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.fail_step(&step.id, &attempt.id, generation, &error)
                        .await?;
                    self.abort_turn(&turn.id, generation).await?;
                    return Err(error);
                }
            };
            if let Some(turn_outcome) = self
                .apply_tool_outcome(run, turn, generation, &step, &attempt, &call, outcome)
                .await?
            {
                return Ok(Some(turn_outcome));
            }
        }
        Ok(None)
    }

    /// Record what a tool dispatch produced on its Step and Attempt.
    ///
    /// Returns `Some` when the turn must end; `None` to continue the loop.
    #[allow(clippy::too_many_arguments)]
    async fn apply_tool_outcome(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        step: &Step,
        attempt: &Attempt,
        call: &ProposedToolCall,
        outcome: ToolDispatchOutcome,
    ) -> Result<Option<TurnOutcome>, RuntimeError> {
        match outcome {
            ToolDispatchOutcome::Completed { evidence_ids, .. } => {
                self.store
                    .complete_step(
                        &step.id,
                        generation,
                        &attempt.id,
                        StepStatus::Completed,
                        AttemptStatus::Succeeded,
                        None,
                        evidence_ids,
                    )
                    .await?;
                Ok(None)
            }
            ToolDispatchOutcome::Failed { error } => {
                self.store
                    .complete_step(
                        &step.id,
                        generation,
                        &attempt.id,
                        StepStatus::Failed,
                        AttemptStatus::Failed,
                        Some(error),
                        Vec::new(),
                    )
                    .await?;
                Ok(None)
            }
            ToolDispatchOutcome::Rejected { code, detail } => {
                // A refusal is a known, model-correctable outcome: the Step records it and
                // the turn continues within budget.
                self.store
                    .complete_step(
                        &step.id,
                        generation,
                        &attempt.id,
                        StepStatus::Failed,
                        AttemptStatus::Failed,
                        Some(json!({ "code": code, "detail": detail })),
                        Vec::new(),
                    )
                    .await?;
                Ok(None)
            }
            ToolDispatchOutcome::Parked {
                state,
                wait_key,
                effect_id,
            } => {
                self.store
                    .complete_step(
                        &step.id,
                        generation,
                        &attempt.id,
                        StepStatus::Completed,
                        AttemptStatus::Succeeded,
                        None,
                        Vec::new(),
                    )
                    .await?;
                self.park_wait(run, turn, generation, state, &wait_key, effect_id)
                    .await
                    .map(Some)
            }
            ToolDispatchOutcome::OutcomeUnknown { effect_id } => {
                self.store
                    .complete_step(
                        &step.id,
                        generation,
                        &attempt.id,
                        StepStatus::Unknown,
                        AttemptStatus::TimedOut,
                        None,
                        Vec::new(),
                    )
                    .await?;
                self.park_unsettled_effect(run, turn, generation, call, effect_id)
                    .await
                    .map(Some)
            }
        }
    }

    /// Park the run in a `WAITING_*` state the tool dispatch reported.
    async fn park_wait(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        state: RunStatus,
        wait_key: &str,
        _effect_id: Option<String>,
    ) -> Result<TurnOutcome, RuntimeError> {
        let mut record = self.pending_state(run).await?;
        match state {
            RunStatus::WaitingApproval
                if !record.pending_approvals.iter().any(|id| id == wait_key) =>
            {
                record.pending_approvals.push(wait_key.to_string());
            }
            RunStatus::WaitingQuestion
                if !record.open_questions.iter().any(|id| id == wait_key) =>
            {
                record.open_questions.push(wait_key.to_string());
            }
            RunStatus::WaitingChild
                if !record.child_agent_threads.iter().any(|id| id == wait_key) =>
            {
                record.child_agent_threads.push(wait_key.to_string());
            }
            _ => {}
        }
        self.store.store_protocol_state(&record).await?;
        self.store
            .transition_run(&run.id, generation, state, Some("tool wait".to_string()))
            .await?;
        self.store
            .finish_turn(&turn.id, generation, TurnStatus::Completed)
            .await?;
        Ok(TurnOutcome::Parked {
            run_id: run.id.to_string(),
            turn_id: turn.id.to_string(),
            state,
            wait_key: wait_key.to_string(),
        })
    }

    /// Park the run in `WAITING_QUESTION` for a durable Question (DOMAIN.md §3.4).
    async fn park_question(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        question_id: &str,
    ) -> Result<TurnOutcome, RuntimeError> {
        let mut state = self.pending_state(run).await?;
        if !state.open_questions.iter().any(|id| id == question_id) {
            state.open_questions.push(question_id.to_string());
        }
        self.store.store_protocol_state(&state).await?;
        self.store
            .transition_run(
                &run.id,
                generation,
                RunStatus::WaitingQuestion,
                Some("question asked".to_string()),
            )
            .await?;
        self.store
            .finish_turn(&turn.id, generation, TurnStatus::Completed)
            .await?;
        Ok(TurnOutcome::Parked {
            run_id: run.id.to_string(),
            turn_id: turn.id.to_string(),
            state: RunStatus::WaitingQuestion,
            wait_key: question_id.to_string(),
        })
    }

    async fn verify_claim(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        claim: &CompletionClaim,
        steps_used: &mut u32,
    ) -> Result<TurnOutcome, RuntimeError> {
        let verify_step = self
            .store
            .record_step(
                &turn.id,
                generation,
                NewStep::new(StepKind::Verify).with_ref(claim.claim_id.clone()),
            )
            .await?;
        *steps_used += 1;
        let (_, attempt) = self
            .store
            .dispatch_step(&verify_step.id, generation)
            .await?;
        self.store
            .transition_run(&run.id, generation, RunStatus::Verifying, None)
            .await?;
        let context = VerificationContext {
            run_id: run.id.to_string(),
            turn_id: turn.id.to_string(),
        };
        match self.verification.verify(claim.clone(), context).await {
            Ok(VerificationOutcome::Verified { evidence_ids }) => {
                self.store
                    .complete_step(
                        &verify_step.id,
                        generation,
                        &attempt.id,
                        StepStatus::Completed,
                        AttemptStatus::Succeeded,
                        None,
                        evidence_ids.clone(),
                    )
                    .await?;
                self.store
                    .transition_run(&run.id, generation, RunStatus::Succeeded, None)
                    .await?;
                self.store
                    .finish_turn(&turn.id, generation, TurnStatus::Completed)
                    .await?;
                Ok(TurnOutcome::Succeeded {
                    run_id: run.id.to_string(),
                    turn_id: turn.id.to_string(),
                    evidence_ids,
                })
            }
            Ok(VerificationOutcome::Rejected { feedback }) => {
                self.store
                    .complete_step(
                        &verify_step.id,
                        generation,
                        &attempt.id,
                        StepStatus::Failed,
                        AttemptStatus::Failed,
                        Some(json!({ "feedback": feedback })),
                        Vec::new(),
                    )
                    .await?;
                // Bounded verification feedback: back to the loop, which the step budget
                // bounds (DOMAIN.md §5.2 `VERIFYING → RUNNING`).
                self.store
                    .transition_run(&run.id, generation, RunStatus::Running, None)
                    .await?;
                Ok(TurnOutcome::Completed {
                    turn_id: turn.id.to_string(),
                    assistant_text: None,
                })
            }
            Err(error) => {
                self.fail_step(&verify_step.id, &attempt.id, generation, &error)
                    .await?;
                let _ = self
                    .store
                    .transition_run(&run.id, generation, RunStatus::Running, None)
                    .await;
                self.abort_turn(&turn.id, generation).await?;
                Err(error)
            }
        }
    }

    async fn exhaust_budget(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        steps_used: u32,
        budget: Budget,
    ) -> Result<TurnOutcome, RuntimeError> {
        self.store
            .finish_turn(&turn.id, generation, TurnStatus::Completed)
            .await?;
        self.store
            .suspend(&run.id, generation, "BUDGET_EXHAUSTED")
            .await?;
        Ok(TurnOutcome::BudgetExhausted {
            turn_id: turn.id.to_string(),
            steps_used,
            max_steps: budget.max_steps,
        })
    }

    async fn park_child(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        child_agent_thread_id: &str,
    ) -> Result<TurnOutcome, RuntimeError> {
        let mut state = self.pending_state(run).await?;
        if !state
            .child_agent_threads
            .iter()
            .any(|id| id == child_agent_thread_id)
        {
            state
                .child_agent_threads
                .push(child_agent_thread_id.to_string());
        }
        self.store.store_protocol_state(&state).await?;
        self.store
            .transition_run(
                &run.id,
                generation,
                RunStatus::WaitingChild,
                Some("child delegation".to_string()),
            )
            .await?;
        self.store
            .finish_turn(&turn.id, generation, TurnStatus::Completed)
            .await?;
        Ok(TurnOutcome::Parked {
            run_id: run.id.to_string(),
            turn_id: turn.id.to_string(),
            state: RunStatus::WaitingChild,
            wait_key: child_agent_thread_id.to_string(),
        })
    }

    async fn park_unsettled_effect(
        &self,
        run: &Run,
        turn: &Turn,
        generation: Generation,
        call: &ProposedToolCall,
        effect_id: Option<String>,
    ) -> Result<TurnOutcome, RuntimeError> {
        let mut state = self.pending_state(run).await?;
        if let Some(effect_id) = &effect_id {
            // The dispatch token and settlement belong to RUN-011's effect reservation;
            // the runtime records only what it knows so recovery reconciles instead of
            // retrying (DOMAIN.md §7.2).
            if !state
                .pending_tool_calls
                .iter()
                .any(|pending| &pending.effect_id == effect_id)
            {
                state
                    .pending_tool_calls
                    .push(crate::runtime::protocol_state::PendingToolCall {
                        tool_call_id: call.call_id.clone(),
                        tool_name: call.tool.clone(),
                        dispatch_token: String::new(),
                        effect_id: effect_id.clone(),
                        effect_status: "OUTCOME_UNKNOWN".to_string(),
                    });
            }
        }
        self.store.store_protocol_state(&state).await?;
        self.store
            .finish_turn(&turn.id, generation, TurnStatus::Completed)
            .await?;
        self.store
            .suspend(&run.id, generation, "EFFECT_UNSETTLED")
            .await?;
        Ok(TurnOutcome::EffectUnsettled {
            run_id: run.id.to_string(),
            turn_id: turn.id.to_string(),
            effect_id,
        })
    }

    async fn pending_state(&self, run: &Run) -> Result<ProtocolState, RuntimeError> {
        let mut state = self
            .store
            .load_protocol_state(&run.id)
            .await?
            .unwrap_or_else(|| ProtocolState::new(run.id.to_string(), run.generation.get() as i64));
        state.generation = run.generation.get() as i64;
        Ok(state)
    }

    async fn fail_step(
        &self,
        step_id: &CanonicalId,
        attempt_id: &CanonicalId,
        generation: Generation,
        error: &RuntimeError,
    ) -> Result<(), RuntimeError> {
        self.store
            .complete_step(
                step_id,
                generation,
                attempt_id,
                StepStatus::Failed,
                AttemptStatus::Failed,
                Some(json!({ "error": error.to_string() })),
                Vec::new(),
            )
            .await?;
        Ok(())
    }

    async fn abort_turn(
        &self,
        turn_id: &CanonicalId,
        generation: Generation,
    ) -> Result<(), RuntimeError> {
        let turn = self.store.load_turn(turn_id).await?;
        if turn.status == TurnStatus::Active {
            self.store
                .finish_turn(turn_id, generation, TurnStatus::Aborted)
                .await?;
        }
        Ok(())
    }
}
