//! Turn-loop routing, step budgets, parking and recovery paths (QA-001 coverage of RUN-001).
//!
//! These suites complement `tests/runtime_state_machine.rs` and `tests/turn_loop.rs`: they drive
//! `RuntimeEngine` against a real scratch PostgreSQL database with scripted ports (model,
//! tools, delegation, budgets) and assert what the runtime commits for the proposal families
//! and outcomes those suites leave out — plan and memory proposals refused with their owner,
//! a joined delegation, every family stopped by the step budget, a budget spent by the model call,
//! parked tool outcomes, an unknown outcome without an effect, resumed pending calls, and
//! recovery of takeover and typed waits. The ports are conformance doubles for the seams; the
//! Run/Turn/Step/Attempt rows, ProtocolState and RuntimeEvents are real.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{EventStore, RuntimeEvent};
use serde_json::json;
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::runtime::budgets::{Meter, MeterUsage};
use quansio_server::runtime::protocol_state::{
    BrowserControl, BrowserControlHolder, PendingToolCall, ProtocolState, Wait, WaitKind,
};
use quansio_server::runtime::state_machine::{
    AttemptStatus, Budget, BudgetPort, CompletionClaim, DelegationContext, DelegationOutcome,
    DelegationPort, DelegationRequest, ModelCallRequest, ModelProposal, ModelProposalSource,
    ModelUsage, NewRun, ProposedQuestion, ProposedToolCall, RecoveryOutcome, Run, RunStatus,
    RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, StepKind, StepStatus,
    ToolDispatchOutcome, ToolDispatchPort, ToolDispatchRequest, TurnInput, TurnOutcome, TurnStatus,
    MEMORY_OWNER, PLAN_VALIDATION_OWNER, QUESTION_OWNER, TOOL_DISPATCH_OWNER,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const CHILD: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0NNNNN";
const APPROVAL: &str = "apr_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const EFFECT: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";

// ------------------------------------------------------------------------------ scripted ports

#[derive(Default)]
struct ScriptedModel {
    proposals: Mutex<VecDeque<ModelProposal>>,
    calls: AtomicUsize,
}

impl ScriptedModel {
    fn new(proposals: Vec<ModelProposal>) -> Arc<Self> {
        Arc::new(Self {
            proposals: Mutex::new(proposals.into()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelProposalSource for ScriptedModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .proposals
            .lock()
            .expect("proposals")
            .pop_front()
            .unwrap_or_default())
    }
}

/// Tools whose dispatch and resume answers are scripted, recording what they were asked.
#[derive(Default)]
struct ScriptedTools {
    dispatch_outcomes: Mutex<VecDeque<ToolDispatchOutcome>>,
    resume_outcome: Option<ToolDispatchOutcome>,
    dispatched: Mutex<Vec<String>>,
    resumed: Mutex<Vec<String>>,
}

impl ScriptedTools {
    fn dispatching(outcomes: Vec<ToolDispatchOutcome>) -> Arc<Self> {
        Arc::new(Self {
            dispatch_outcomes: Mutex::new(outcomes.into()),
            ..Self::default()
        })
    }

    fn resuming(outcome: Option<ToolDispatchOutcome>) -> Arc<Self> {
        Arc::new(Self {
            resume_outcome: outcome,
            ..Self::default()
        })
    }

    fn dispatched(&self) -> Vec<String> {
        self.dispatched.lock().expect("dispatched").clone()
    }

    fn resumed(&self) -> Vec<String> {
        self.resumed.lock().expect("resumed").clone()
    }
}

#[async_trait]
impl ToolDispatchPort for ScriptedTools {
    async fn dispatch(
        &self,
        call: ProposedToolCall,
        _request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        self.dispatched
            .lock()
            .expect("dispatched")
            .push(call.call_id);
        Ok(self
            .dispatch_outcomes
            .lock()
            .expect("outcomes")
            .pop_front()
            .unwrap_or(ToolDispatchOutcome::Completed {
                evidence_ids: Vec::new(),
                effect_id: None,
            }))
    }

    async fn resume(
        &self,
        pending: PendingToolCall,
        _request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        self.resumed
            .lock()
            .expect("resumed")
            .push(pending.tool_call_id.clone());
        self.resume_outcome
            .clone()
            .ok_or_else(|| RuntimeError::seam_not_available("tool resume", TOOL_DISPATCH_OWNER))
    }
}

/// A delegation port whose child finishes inline.
#[derive(Default)]
struct InlineDelegation {
    calls: AtomicUsize,
}

#[async_trait]
impl DelegationPort for InlineDelegation {
    async fn delegate(
        &self,
        request: DelegationRequest,
        _context: DelegationContext,
    ) -> Result<DelegationOutcome, RuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(DelegationOutcome::Joined {
            result: json!({ "request_id": request.request_id, "summary": "done inline" }),
        })
    }
}

/// A budget that is spent as soon as any tokens were charged against it.
#[derive(Default)]
struct TokenBudget {
    charged_tokens: Mutex<u64>,
}

#[async_trait]
impl BudgetPort for TokenBudget {
    async fn exhausted(&self, _run: &Run) -> Result<Option<String>, RuntimeError> {
        let charged = *self.charged_tokens.lock().expect("charged");
        Ok((charged > 0).then(|| "tokens".to_string()))
    }

    async fn charge(&self, _run: &Run, usage: &MeterUsage) -> Result<(), RuntimeError> {
        *self.charged_tokens.lock().expect("charged") += usage.get(Meter::Tokens);
        Ok(())
    }
}

// ------------------------------------------------------------------------------------ fixture

struct Fixture {
    name: String,
    pool: PgPool,
    engine: RuntimeEngine,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(AGENT)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    tx.commit().await.expect("commit");
    let engine = RuntimeEngine::new(
        pool.clone(),
        RuntimeIdentity::system(
            TENANT,
            "state-machine-engine-test",
            CorrelationId::generate(&mut UlidGenerator::new()),
        ),
    )
    .expect("engine");
    Some(Fixture { name, pool, engine })
}

async fn running_run(f: &Fixture, max_steps: u32) -> Run {
    let run = f
        .engine
        .create_run(
            NewRun::new(
                WORKSPACE,
                CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
                CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
                RunTriggerKind::Manual,
            )
            .with_budget(Budget::new(max_steps)),
        )
        .await
        .expect("create run");
    let run = f
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    f.engine
        .start(&run.id, run.generation)
        .await
        .expect("start")
}

async fn store_state(f: &Fixture, run: &Run, modify: impl FnOnce(&mut ProtocolState)) {
    let mut state = ProtocolState::new(run.id.to_string(), run.generation.get() as i64);
    modify(&mut state);
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("store state");
}

async fn events(f: &Fixture) -> Vec<RuntimeEvent> {
    EventStore::new(f.pool.clone())
        .read_events_after(TENANT, None, 10_000)
        .await
        .expect("events")
}

async fn run_status(f: &Fixture, run: &Run) -> RunStatus {
    f.engine
        .store()
        .load_run(&run.id)
        .await
        .expect("run")
        .status
}

/// The single turn of a run and its steps, in order.
async fn only_turn(f: &Fixture, run: &Run) -> (TurnStatus, Vec<(StepKind, StepStatus)>) {
    let turns = f.engine.store().list_turns(&run.id).await.expect("turns");
    assert_eq!(turns.len(), 1, "exactly one turn");
    let steps = f
        .engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps")
        .into_iter()
        .map(|step| (step.kind, step.status))
        .collect();
    (turns[0].status, steps)
}

async fn turn(engine: &RuntimeEngine, run: &Run) -> Result<TurnOutcome, RuntimeError> {
    engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Message),
        )
        .await
}

fn tool(call_id: &str, path: &str) -> ProposedToolCall {
    ProposedToolCall::new(call_id, "fs.read", json!({ "path": path }))
}

fn delegation() -> DelegationRequest {
    DelegationRequest {
        request_id: "req_1".to_string(),
        work_node_id: None,
        instruction: "summarise".to_string(),
    }
}

fn question() -> ProposedQuestion {
    ProposedQuestion {
        question_id: "q_01J8Z3K6F1N8VQ2X5W9Y0MMMMM".to_string(),
        prompt: "which file?".to_string(),
        kind: "free_text".to_string(),
        options: Vec::new(),
        required: true,
    }
}

fn claim() -> CompletionClaim {
    CompletionClaim {
        claim_id: "clm_1".to_string(),
        summary: "done".to_string(),
        evidence_ids: Vec::new(),
    }
}

// --------------------------------------------------------------------------------------- suites

#[tokio::test]
async fn recovery_reports_takeover_and_typed_waits_and_never_releases_an_unnamed_park() {
    let Some(f) = prepare("qa001_engine_recover").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;

    store_state(&f, &run, |state| {
        state.browser_control = Some(BrowserControl {
            session_id: "bs_held".to_string(),
            holder: BrowserControlHolder::User,
            since: None,
        });
    })
    .await;
    assert_eq!(
        f.engine.recover(&run.id).await.expect("recover"),
        RecoveryOutcome::Waiting {
            state: RunStatus::WaitingTakeover,
            key: "bs_held".to_string(),
        }
    );

    // Control held by the agent itself is no wait.
    store_state(&f, &run, |state| {
        state.browser_control = Some(BrowserControl {
            session_id: "bs_agent".to_string(),
            holder: BrowserControlHolder::Agent,
            since: None,
        });
    })
    .await;
    assert_eq!(
        f.engine.recover(&run.id).await.expect("recover"),
        RecoveryOutcome::Continue {
            run_id: run.id.to_string()
        }
    );

    for (kind, state) in [
        (WaitKind::Timer, RunStatus::WaitingTimer),
        (WaitKind::Event, RunStatus::WaitingEvent),
        (WaitKind::Approval, RunStatus::WaitingApproval),
        (WaitKind::Question, RunStatus::WaitingQuestion),
        (WaitKind::Child, RunStatus::WaitingChild),
        (WaitKind::Takeover, RunStatus::WaitingTakeover),
    ] {
        store_state(&f, &run, |protocol| {
            protocol.waits = vec![Wait {
                kind: kind.clone(),
                key: format!("key-{kind:?}"),
                expires_at: None,
            }];
        })
        .await;
        assert_eq!(
            f.engine.recover(&run.id).await.expect("recover"),
            RecoveryOutcome::Waiting {
                state,
                key: format!("key-{kind:?}"),
            },
            "{kind:?}"
        );
    }

    // A parked run whose state names no wait is still only released by a matching resolution.
    let parked = running_run(&f, 8).await;
    f.engine
        .transition_run(&parked.id, parked.generation, RunStatus::WaitingEvent, None)
        .await
        .expect("park");
    let before = events(&f).await.len();
    assert_eq!(
        f.engine.recover(&parked.id).await.expect("recover"),
        RecoveryOutcome::Waiting {
            state: RunStatus::WaitingEvent,
            key: "unknown".to_string(),
        }
    );
    assert_eq!(run_status(&f, &parked).await, RunStatus::WaitingEvent);
    assert_eq!(run_status(&f, &run).await, RunStatus::Running);
    assert_eq!(
        events(&f).await.len(),
        before,
        "recovery decisions write nothing"
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn run_turn_refuses_a_run_that_is_not_running_before_opening_a_turn() {
    let Some(f) = prepare("qa001_engine_not_running").await else {
        blocked_marker();
        return;
    };
    let model = ScriptedModel::new(Vec::new());
    let engine = f.engine.clone().with_model_source(model.clone());
    let run = running_run(&f, 8).await;
    let suspended = engine
        .suspend(&run.id, run.generation, "operator")
        .await
        .expect("suspend");
    let refused = turn(&engine, &suspended).await.expect_err("suspended");
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { from, .. } if from == "SUSPENDED"),
        "{refused:?}"
    );
    assert_eq!(model.calls(), 0);
    assert!(engine
        .store()
        .list_turns(&run.id)
        .await
        .expect("turns")
        .is_empty());

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn plan_and_memory_proposals_abort_the_turn_and_name_their_owner() {
    let Some(f) = prepare("qa001_engine_seams").await else {
        blocked_marker();
        return;
    };
    for (proposal, owner) in [
        (
            ModelProposal {
                plan_proposal: Some(json!({ "steps": ["a"] })),
                ..ModelProposal::default()
            },
            PLAN_VALIDATION_OWNER,
        ),
        (
            ModelProposal {
                memory_candidates: vec![json!({ "fact": "the sky is blue" })],
                ..ModelProposal::default()
            },
            MEMORY_OWNER,
        ),
    ] {
        let run = running_run(&f, 8).await;
        let engine = f
            .engine
            .clone()
            .with_model_source(ScriptedModel::new(vec![proposal]));
        let refused = turn(&engine, &run).await.expect_err("seam not available");
        assert!(
            matches!(&refused, RuntimeError::SeamNotAvailable { owner: named, .. } if *named == owner),
            "{refused:?}"
        );
        let (status, steps) = only_turn(&f, &run).await;
        assert_eq!(status, TurnStatus::Aborted);
        assert_eq!(
            steps,
            vec![(StepKind::ModelCall, StepStatus::Completed)],
            "nothing past the model call was executed"
        );
        assert_eq!(run_status(&f, &run).await, RunStatus::Running);
    }

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_delegation_that_joins_inline_completes_its_step_and_the_turn_continues() {
    let Some(f) = prepare("qa001_engine_joined").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let delegation_port = Arc::new(InlineDelegation::default());
    let model = ScriptedModel::new(vec![
        ModelProposal {
            delegate_requests: vec![delegation()],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("summary delivered".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let engine = f
        .engine
        .clone()
        .with_model_source(model.clone())
        .with_delegation(delegation_port.clone());
    let outcome = turn(&engine, &run).await.expect("turn");
    assert!(
        matches!(&outcome, TurnOutcome::Completed { assistant_text: Some(text), .. } if text == "summary delivered"),
        "{outcome:?}"
    );
    assert_eq!(delegation_port.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        model.calls(),
        2,
        "the loop asked the model again after the join"
    );
    let (status, steps) = only_turn(&f, &run).await;
    assert_eq!(status, TurnStatus::Completed);
    assert_eq!(
        steps,
        vec![
            (StepKind::ModelCall, StepStatus::Completed),
            (StepKind::Delegate, StepStatus::Completed),
            (StepKind::ModelCall, StepStatus::Completed),
        ]
    );
    assert_eq!(run_status(&f, &run).await, RunStatus::Running);
    // A join is a success: its result is not recorded as the attempt's error.
    let turns = f.engine.store().list_turns(&run.id).await.expect("turns");
    let delegate = f
        .engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps")
        .into_iter()
        .find(|step| step.kind == StepKind::Delegate)
        .expect("delegate step");
    let attempts = f
        .engine
        .store()
        .list_attempts(&delegate.id)
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, AttemptStatus::Succeeded);
    assert_eq!(attempts[0].error, None, "{:?}", attempts[0].error);

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn every_proposal_family_stops_at_the_step_budget_without_reaching_its_port() {
    let Some(f) = prepare("qa001_engine_budget").await else {
        blocked_marker();
        return;
    };
    let cases = [
        ModelProposal {
            delegate_requests: vec![delegation()],
            ..ModelProposal::default()
        },
        ModelProposal {
            question: Some(question()),
            ..ModelProposal::default()
        },
        ModelProposal {
            completion_claim: Some(claim()),
            ..ModelProposal::default()
        },
    ];
    for proposal in cases {
        let run = running_run(&f, 1).await;
        let delegation_port = Arc::new(InlineDelegation::default());
        let engine = f
            .engine
            .clone()
            .with_model_source(ScriptedModel::new(vec![proposal.clone()]))
            .with_delegation(delegation_port.clone());
        let outcome = turn(&engine, &run).await.expect("turn");
        assert!(
            matches!(
                outcome,
                TurnOutcome::BudgetExhausted {
                    steps_used: 1,
                    max_steps: 1,
                    ..
                }
            ),
            "{proposal:?} -> {outcome:?}"
        );
        assert_eq!(delegation_port.calls.load(Ordering::SeqCst), 0);
        let (status, steps) = only_turn(&f, &run).await;
        assert_eq!(status, TurnStatus::Completed);
        assert_eq!(steps, vec![(StepKind::ModelCall, StepStatus::Completed)]);
        assert_eq!(run_status(&f, &run).await, RunStatus::Suspended);
    }

    // Two independent tool calls with room for one: the first is dispatched and settled, the
    // second is never recorded, so no step is left dispatched for a call no tool received.
    let run = running_run(&f, 2).await;
    let tools = ScriptedTools::dispatching(Vec::new());
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![ModelProposal {
            tool_calls: vec![tool("tc_a", "a.md"), tool("tc_b", "b.md")],
            ..ModelProposal::default()
        }]))
        .with_tool_dispatch(tools.clone());
    let outcome = turn(&engine, &run).await.expect("turn");
    assert!(
        matches!(
            outcome,
            TurnOutcome::BudgetExhausted {
                steps_used: 2,
                max_steps: 2,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        tools.dispatched(),
        vec!["tc_a".to_string()],
        "only the call the budget pays for reaches the tool port"
    );
    let (_, steps) = only_turn(&f, &run).await;
    assert_eq!(
        steps,
        vec![
            (StepKind::ModelCall, StepStatus::Completed),
            (StepKind::ToolCall, StepStatus::Completed),
        ],
        "the unpaid call has no step"
    );
    assert_eq!(run_status(&f, &run).await, RunStatus::Suspended);

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_question_the_protocol_cannot_ask_fails_its_step_and_aborts_the_turn() {
    let Some(f) = prepare("qa001_engine_question").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![ModelProposal {
            question: Some(question()),
            ..ModelProposal::default()
        }]));
    let refused = turn(&engine, &run).await.expect_err("no question port");
    assert!(
        matches!(
            refused,
            RuntimeError::SeamNotAvailable {
                owner: QUESTION_OWNER,
                ..
            }
        ),
        "{refused:?}"
    );
    let (status, steps) = only_turn(&f, &run).await;
    assert_eq!(status, TurnStatus::Aborted);
    assert_eq!(
        steps,
        vec![
            (StepKind::ModelCall, StepStatus::Completed),
            (StepKind::Wait, StepStatus::Failed),
        ]
    );
    assert_eq!(run_status(&f, &run).await, RunStatus::Running);
    assert!(
        f.engine
            .store()
            .load_protocol_state(&run.id)
            .await
            .expect("state")
            .is_none_or(|state| state.open_questions.is_empty()),
        "an unasked question is not recorded as open"
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_budget_the_model_call_spent_dispatches_no_tool_call() {
    let Some(f) = prepare("qa001_engine_spent").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let tools = ScriptedTools::dispatching(Vec::new());
    let budget = Arc::new(TokenBudget::default());
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![ModelProposal {
            tool_calls: vec![tool("tc_a", "a.md")],
            usage: Some(ModelUsage {
                input_tokens: 40,
                output_tokens: 2,
            }),
            ..ModelProposal::default()
        }]))
        .with_tool_dispatch(tools.clone())
        .with_budgets(budget.clone());
    let outcome = turn(&engine, &run).await.expect("turn");
    assert!(
        matches!(outcome, TurnOutcome::BudgetExhausted { steps_used: 1, .. }),
        "{outcome:?}"
    );
    assert_eq!(*budget.charged_tokens.lock().expect("charged"), 42);
    assert!(tools.dispatched().is_empty());
    let (_, steps) = only_turn(&f, &run).await;
    assert_eq!(steps, vec![(StepKind::ModelCall, StepStatus::Completed)]);
    assert_eq!(run_status(&f, &run).await, RunStatus::Suspended);

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn parked_tool_outcomes_record_the_approval_or_child_they_wait_for() {
    let Some(f) = prepare("qa001_engine_parked").await else {
        blocked_marker();
        return;
    };
    for (state, key) in [
        (RunStatus::WaitingApproval, APPROVAL),
        (RunStatus::WaitingChild, CHILD),
    ] {
        let run = running_run(&f, 8).await;
        let tools = ScriptedTools::dispatching(vec![ToolDispatchOutcome::Parked {
            state,
            wait_key: key.to_string(),
            effect_id: None,
        }]);
        let engine = f
            .engine
            .clone()
            .with_model_source(ScriptedModel::new(vec![ModelProposal {
                tool_calls: vec![tool("tc_a", "a.md")],
                ..ModelProposal::default()
            }]))
            .with_tool_dispatch(tools);
        let outcome = turn(&engine, &run).await.expect("turn");
        assert_eq!(
            outcome,
            TurnOutcome::Parked {
                run_id: run.id.to_string(),
                turn_id: f.engine.store().list_turns(&run.id).await.expect("turns")[0]
                    .id
                    .to_string(),
                state,
                wait_key: key.to_string(),
            }
        );
        assert_eq!(run_status(&f, &run).await, state);
        let recorded = f
            .engine
            .store()
            .load_protocol_state(&run.id)
            .await
            .expect("load")
            .expect("state");
        match state {
            RunStatus::WaitingApproval => {
                assert_eq!(recorded.pending_approvals, vec![APPROVAL.to_string()]);
            }
            _ => assert_eq!(recorded.child_agent_threads, vec![CHILD.to_string()]),
        }
        // Recovery sees exactly that wait.
        assert_eq!(
            f.engine.recover(&run.id).await.expect("recover"),
            RecoveryOutcome::Waiting {
                state,
                key: key.to_string(),
            }
        );
    }

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn an_unknown_outcome_without_an_effect_suspends_without_inventing_a_pending_call() {
    let Some(f) = prepare("qa001_engine_unknown").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let tools = ScriptedTools::dispatching(vec![ToolDispatchOutcome::OutcomeUnknown {
        effect_id: None,
    }]);
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![ModelProposal {
            tool_calls: vec![tool("tc_a", "a.md")],
            ..ModelProposal::default()
        }]))
        .with_tool_dispatch(tools);
    let outcome = turn(&engine, &run).await.expect("turn");
    assert!(
        matches!(
            &outcome,
            TurnOutcome::EffectUnsettled {
                effect_id: None,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(run_status(&f, &run).await, RunStatus::Suspended);
    let state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("load")
        .expect("state");
    assert!(
        state.pending_tool_calls.is_empty(),
        "no effect id, so no pending call is fabricated"
    );
    let (_, steps) = only_turn(&f, &run).await;
    assert_eq!(steps[1], (StepKind::ToolCall, StepStatus::Unknown));

    drop_pool(&f.pool, &f.name).await;
}

fn pending(effect_status: &str) -> PendingToolCall {
    PendingToolCall {
        tool_call_id: "tc_pending".to_string(),
        tool_name: "fs.read".to_string(),
        dispatch_token: String::new(),
        effect_id: EFFECT.to_string(),
        effect_status: effect_status.to_string(),
    }
}

#[tokio::test]
async fn a_resumed_run_settles_its_recorded_call_before_the_model_is_asked_again() {
    let Some(f) = prepare("qa001_engine_resume").await else {
        blocked_marker();
        return;
    };

    // An unsettled recorded effect is reconciled, never re-dispatched, and the model is not asked.
    let run = running_run(&f, 8).await;
    store_state(&f, &run, |state| {
        state.pending_tool_calls = vec![pending("OUTCOME_UNKNOWN")];
    })
    .await;
    let model = ScriptedModel::new(Vec::new());
    let tools = ScriptedTools::resuming(None);
    let engine = f
        .engine
        .clone()
        .with_model_source(model.clone())
        .with_tool_dispatch(tools.clone());
    let outcome = turn(&engine, &run).await.expect("turn");
    assert!(
        matches!(&outcome, TurnOutcome::EffectUnsettled { effect_id: Some(effect), .. } if effect == EFFECT),
        "{outcome:?}"
    );
    assert_eq!(model.calls(), 0);
    assert!(tools.resumed().is_empty() && tools.dispatched().is_empty());
    assert_eq!(run_status(&f, &run).await, RunStatus::Suspended);
    let (_, steps) = only_turn(&f, &run).await;
    assert!(
        steps.is_empty(),
        "no step was recorded for the unsettled call"
    );

    // A settled recorded call is resumed; a resume that parks ends the turn without the model.
    let run = running_run(&f, 8).await;
    store_state(&f, &run, |state| {
        state.pending_tool_calls = vec![pending("AUTHORIZED")];
    })
    .await;
    let model = ScriptedModel::new(Vec::new());
    let tools = ScriptedTools::resuming(Some(ToolDispatchOutcome::Parked {
        state: RunStatus::WaitingApproval,
        wait_key: APPROVAL.to_string(),
        effect_id: Some(EFFECT.to_string()),
    }));
    let engine = f
        .engine
        .clone()
        .with_model_source(model.clone())
        .with_tool_dispatch(tools.clone());
    let outcome = turn(&engine, &run).await.expect("turn");
    assert!(
        matches!(&outcome, TurnOutcome::Parked { state: RunStatus::WaitingApproval, wait_key, .. } if wait_key == APPROVAL),
        "{outcome:?}"
    );
    assert_eq!(tools.resumed(), vec!["tc_pending".to_string()]);
    assert_eq!(model.calls(), 0);
    let (_, steps) = only_turn(&f, &run).await;
    assert_eq!(steps, vec![(StepKind::ToolCall, StepStatus::Completed)]);

    // A resume the port cannot perform fails the step, aborts the turn and surfaces the error.
    let run = running_run(&f, 8).await;
    store_state(&f, &run, |state| {
        state.pending_tool_calls = vec![pending("AUTHORIZED")];
    })
    .await;
    let model = ScriptedModel::new(Vec::new());
    let engine = f
        .engine
        .clone()
        .with_model_source(model.clone())
        .with_tool_dispatch(ScriptedTools::resuming(None));
    let refused = turn(&engine, &run).await.expect_err("resume unavailable");
    assert!(
        matches!(
            refused,
            RuntimeError::SeamNotAvailable {
                owner: TOOL_DISPATCH_OWNER,
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(model.calls(), 0);
    let (status, steps) = only_turn(&f, &run).await;
    assert_eq!(status, TurnStatus::Aborted);
    assert_eq!(steps, vec![(StepKind::ToolCall, StepStatus::Failed)]);
    let turns = f.engine.store().list_turns(&run.id).await.expect("turns");
    let step = &f
        .engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps")[0];
    let attempts = f
        .engine
        .store()
        .list_attempts(&step.id)
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, AttemptStatus::Failed);

    // With no step budget left, a settled recorded call is not resumed at all.
    let run = running_run(&f, 0).await;
    store_state(&f, &run, |state| {
        state.pending_tool_calls = vec![pending("AUTHORIZED")];
    })
    .await;
    let tools = ScriptedTools::resuming(None);
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(Vec::new()))
        .with_tool_dispatch(tools.clone());
    let outcome = turn(&engine, &run).await.expect("turn");
    assert!(
        matches!(
            outcome,
            TurnOutcome::BudgetExhausted {
                steps_used: 0,
                max_steps: 0,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(tools.resumed().is_empty());
    assert_eq!(run_status(&f, &run).await, RunStatus::Suspended);

    drop_pool(&f.pool, &f.name).await;
}
