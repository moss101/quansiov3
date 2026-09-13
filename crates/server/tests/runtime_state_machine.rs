//! Authoritative runtime state machine tests (RUN-001).
//!
//! These tests drive the Run/Turn/Step/Attempt lifecycle through
//! [`quansio_server::runtime::state_machine::RuntimeEngine`] against a real scratch
//! PostgreSQL database. They assert DOMAIN.md §5.2–§5.5 legality, that every accepted
//! transition is durable and observable as a `run.*`/`turn.*`/`step.*` RuntimeEvent with
//! contiguous sequences, generation fencing, budget-bounded iteration, wait-matched
//! recovery that is idempotent, and the canonical ownership chain
//! run → turn → step → attempt.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Generation, Prefix, UlidGenerator};
use quansio_events::{EventStore, RuntimeEvent};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::runtime::protocol_state::{PendingToolCall, ProtocolState, ProtocolStateStore};
use quansio_server::runtime::state_machine::{
    AttemptStatus, Budget, CancellationWon, ModelCallRequest, ModelProposal, ModelProposalSource,
    NewRun, NewStep, ProposedToolCall, RecoveryOutcome, Run, RunStatus, RunTriggerKind,
    RuntimeEngine, RuntimeError, RuntimeIdentity, StepKind, StepStatus, ToolDispatchOutcome,
    ToolDispatchPort, ToolDispatchRequest, TurnInput, TurnOutcome, TurnStatus, VerificationContext,
    VerificationOutcome, VerificationPort, WaitResolution, DELEGATION_OWNER, TOOL_DISPATCH_OWNER,
    VERIFICATION_OWNER,
};

mod common;
use common::{
    admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, scratch_url, seed_tenant,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const USER_B: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORKSPACE_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORK_NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const EVIDENCE: &str = "evd_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";

struct Fixture {
    name: String,
    url: String,
    pool: PgPool,
    engine: RuntimeEngine,
    agent_thread: CanonicalId,
}

async fn engine_for(pool: &PgPool, tenant: &str) -> RuntimeEngine {
    let mut generator = UlidGenerator::new();
    let correlation = CorrelationId::generate(&mut generator);
    RuntimeEngine::new(
        pool.clone(),
        RuntimeIdentity::system(tenant, "runtime-state-machine-test", correlation),
    )
    .expect("engine")
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;

    let mut generator = UlidGenerator::new();
    let agent_thread = CanonicalId::generate(Prefix::AgentThread, &mut generator);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(agent_thread.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    tx.commit().await.expect("commit");

    let engine = engine_for(&pool, TENANT).await;
    Some(Fixture {
        url: scratch_url(&name),
        name,
        pool,
        engine,
        agent_thread,
    })
}

fn node_id() -> CanonicalId {
    CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("work node id")
}

async fn create_run(fixture: &Fixture, max_steps: u32) -> Run {
    fixture
        .engine
        .create_run(
            NewRun::new(
                WORKSPACE,
                node_id(),
                fixture.agent_thread,
                RunTriggerKind::Manual,
            )
            .with_budget(Budget::new(max_steps)),
        )
        .await
        .expect("create run")
}

async fn running_run(fixture: &Fixture, max_steps: u32) -> Run {
    let run = create_run(fixture, max_steps).await;
    let run = fixture
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    fixture
        .engine
        .start(&run.id, run.generation)
        .await
        .expect("start")
}

async fn events_of(fixture: &Fixture, tenant: &str) -> Vec<RuntimeEvent> {
    EventStore::new(fixture.pool.clone())
        .read_events_after(tenant, None, 500)
        .await
        .expect("read events")
}

async fn event_types(fixture: &Fixture) -> Vec<String> {
    events_of(fixture, TENANT)
        .await
        .into_iter()
        .map(|event| event.event_type.to_string())
        .collect()
}

fn tool_call(call_id: &str) -> ModelProposal {
    ModelProposal {
        tool_calls: vec![ProposedToolCall::new(
            call_id,
            "fs.read",
            serde_json::json!({ "path": "report.md" }),
        )],
        ..ModelProposal::default()
    }
}

fn completion(claim_id: &str) -> ModelProposal {
    ModelProposal {
        completion_claim: Some(quansio_server::runtime::state_machine::CompletionClaim {
            claim_id: claim_id.to_string(),
            summary: "the report exists".to_string(),
            evidence_ids: vec![EVIDENCE.to_string()],
        }),
        ..ModelProposal::default()
    }
}

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
            .expect("model proposals")
            .pop_front()
            .unwrap_or_default())
    }
}

#[derive(Default)]
struct RecordingTools {
    calls: Mutex<Vec<ProposedToolCall>>,
}

#[async_trait]
impl ToolDispatchPort for RecordingTools {
    async fn dispatch(
        &self,
        call: ProposedToolCall,
        _request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        self.calls.lock().expect("tool calls").push(call);
        Ok(ToolDispatchOutcome::Completed {
            evidence_ids: vec![EVIDENCE.to_string()],
            effect_id: None,
        })
    }
}

/// Tool dispatch that reports an unknown external outcome, standing in for RUN-011.
struct UnknownOutcomeTools;

#[async_trait]
impl ToolDispatchPort for UnknownOutcomeTools {
    async fn dispatch(
        &self,
        _call: ProposedToolCall,
        _request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        Ok(ToolDispatchOutcome::OutcomeUnknown {
            effect_id: Some("eff_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string()),
        })
    }
}

/// Verification that accepts every claim, standing in for RUN-008.
struct AcceptingVerification;
#[async_trait]
impl VerificationPort for AcceptingVerification {
    async fn verify(
        &self,
        _claim: quansio_server::runtime::state_machine::CompletionClaim,
        _context: VerificationContext,
    ) -> Result<VerificationOutcome, RuntimeError> {
        Ok(VerificationOutcome::Verified {
            evidence_ids: vec![EVIDENCE.to_string()],
        })
    }
}

#[tokio::test]
async fn happy_path_emits_every_event_in_order_with_contiguous_sequences() {
    let Some(f) = prepare("runtime_happy").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;

    let model = ScriptedModel::new(vec![
        tool_call("tc_01J8Z3K6F1N8VQ2X5W9Y0EEEEE"),
        completion("clm_01J8Z3K6F1N8VQ2X5W9Y0EEEEE"),
    ]);
    let tools = Arc::new(RecordingTools::default());
    let engine = f
        .engine
        .clone()
        .with_model_source(model.clone())
        .with_tool_dispatch(tools.clone())
        .with_verification(Arc::new(AcceptingVerification));

    let outcome = engine
        .run_turn(&run.id, generation, TurnInput::new(RunTriggerKind::Message))
        .await
        .expect("turn loop");

    assert!(
        matches!(outcome, TurnOutcome::Succeeded { .. }),
        "{outcome:?}"
    );
    assert_eq!(model.calls(), 2, "one model call per proposal");
    assert_eq!(tools.calls.lock().expect("tool calls").len(), 1);
    assert_eq!(
        engine.store().load_run(&run.id).await.expect("run").status,
        RunStatus::Succeeded
    );

    let events = events_of(&f, TENANT).await;
    let types: Vec<String> = events
        .iter()
        .map(|event| event.event_type.to_string())
        .collect();
    assert_eq!(
        types,
        vec![
            "run.created",
            "run.queued",
            "run.started",
            "turn.started",
            "step.created",
            "step.dispatched",
            "step.completed",
            "step.created",
            "step.dispatched",
            "step.completed",
            "step.created",
            "step.dispatched",
            "step.completed",
            "step.created",
            "step.dispatched",
            "run.verifying",
            "step.completed",
            "run.succeeded",
            "turn.completed",
        ]
    );
    for pair in events.windows(2) {
        assert_eq!(
            pair[1].sequence.get(),
            pair[0].sequence.get() + 1,
            "sequences must be contiguous"
        );
    }
    let run_versions: Vec<u64> = events
        .iter()
        .filter(|event| event.aggregate_type == "run")
        .map(|event| event.aggregate_version)
        .collect();
    assert_eq!(run_versions, vec![1, 2, 3, 4, 5]);
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn illegal_transitions_of_each_family_leave_state_and_events_untouched() {
    let Some(f) = prepare("runtime_illegal").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;
    let before = event_types(&f).await;

    // Run: RUNNING → QUEUED is not in DOMAIN.md §5.2.
    let error = f
        .engine
        .transition_run(&run.id, generation, RunStatus::Queued, None)
        .await
        .expect_err("illegal run transition");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Running
    );

    // Turn: an active turn cannot transition to active again.
    let turn = f
        .engine
        .begin_turn(&run.id, generation, TurnInput::new(RunTriggerKind::Message))
        .await
        .expect("turn");
    let turn_error = f
        .engine
        .finish_turn(&turn.id, generation, TurnStatus::Active)
        .await
        .expect_err("illegal turn transition");
    assert_eq!(turn_error.code(), "RUNTIME_ILLEGAL_TRANSITION");
    assert_eq!(
        f.engine
            .store()
            .load_turn(&turn.id)
            .await
            .expect("turn")
            .status,
        TurnStatus::Active
    );

    // Step: a pending step cannot be completed without being dispatched.
    let step = f
        .engine
        .store()
        .record_step(&turn.id, generation, NewStep::new(StepKind::ToolCall))
        .await
        .expect("step");
    let (dispatched, attempt) = f
        .engine
        .store()
        .dispatch_step(&step.id, generation)
        .await
        .expect("dispatch");
    let step_error = f
        .engine
        .store()
        .complete_step(
            &dispatched.id,
            generation,
            &attempt.id,
            StepStatus::Pending,
            AttemptStatus::Succeeded,
            None,
            Vec::new(),
        )
        .await
        .expect_err("illegal step transition");
    assert_eq!(step_error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    // Attempt: a finished attempt is never re-finished.
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE attempts SET status = 'succeeded' WHERE id = $1")
        .bind(attempt.id.to_string())
        .execute(&mut *tx)
        .await
        .expect("seed finished attempt");
    tx.commit().await.expect("commit");
    let attempt_error = f
        .engine
        .store()
        .complete_step(
            &dispatched.id,
            generation,
            &attempt.id,
            StepStatus::Completed,
            AttemptStatus::Failed,
            None,
            Vec::new(),
        )
        .await
        .expect_err("illegal attempt transition");
    assert_eq!(attempt_error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    assert_eq!(event_types(&f).await.len(), before.len() + 3);
    assert_eq!(
        f.engine
            .store()
            .load_step(&dispatched.id)
            .await
            .expect("step")
            .status,
        StepStatus::Dispatched
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn budget_exhaustion_stops_the_loop_without_further_model_calls() {
    let Some(f) = prepare("runtime_budget").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 2).await;
    let model = ScriptedModel::new(vec![tool_call("tc_01J8Z3K6F1N8VQ2X5W9Y0EEEEE")]);
    let engine = f
        .engine
        .clone()
        .with_model_source(model.clone())
        .with_tool_dispatch(Arc::new(RecordingTools::default()));

    let outcome = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn loop");
    match &outcome {
        TurnOutcome::BudgetExhausted {
            steps_used,
            max_steps,
            ..
        } => {
            assert_eq!(*steps_used, 2);
            assert_eq!(*max_steps, 2);
        }
        other => panic!("expected budget exhaustion, got {other:?}"),
    }
    assert_eq!(
        model.calls(),
        1,
        "the budget stops the loop before a second call"
    );
    let reloaded = engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(reloaded.status, RunStatus::Suspended);
    assert_eq!(
        reloaded.terminal_reason.as_deref(),
        Some("BUDGET_EXHAUSTED")
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn unknown_tool_outcome_suspends_for_reconciliation_never_retry() {
    let Some(f) = prepare("runtime_unknown_tool").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![tool_call(
            "tc_01J8Z3K6F1N8VQ2X5W9Y0EEEEE",
        )]))
        .with_tool_dispatch(Arc::new(UnknownOutcomeTools));

    let outcome = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Message),
        )
        .await
        .expect("turn loop");
    match outcome {
        TurnOutcome::EffectUnsettled { effect_id, .. } => {
            assert_eq!(effect_id.as_deref(), Some("eff_01J8Z3K6F1N8VQ2X5W9Y0EEEEE"));
        }
        other => panic!("expected an unsettled effect, got {other:?}"),
    }
    let reloaded = engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(reloaded.status, RunStatus::Suspended);
    assert_eq!(
        reloaded.terminal_reason.as_deref(),
        Some("EFFECT_UNSETTLED")
    );

    // Recovery reconciles the unsettled effect instead of re-dispatching it.
    assert_eq!(
        engine.recover(&run.id).await.expect("recover"),
        RecoveryOutcome::ReconcileEffect {
            effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
            tool_call_id: "tc_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
        }
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn recovery_waits_for_the_matching_approval_and_is_idempotent() {
    let Some(f) = prepare("runtime_recovery").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;
    f.engine
        .transition_run(
            &run.id,
            generation,
            RunStatus::WaitingApproval,
            Some("approval".to_string()),
        )
        .await
        .expect("park");

    let mut state = ProtocolState::new(run.id.to_string(), generation.get() as i64);
    state.pending_approvals = vec!["apr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string()];
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("protocol state");

    let before = event_types(&f).await;

    // Reconnect: a new pool and engine reading only durable state.
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&f.url)
        .await
        .expect("reconnect");
    let engine = engine_for(&pool, TENANT).await;

    let first = engine.recover(&run.id).await.expect("recover");
    assert_eq!(
        first,
        RecoveryOutcome::Waiting {
            state: RunStatus::WaitingApproval,
            key: "apr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
        }
    );
    let second = engine.recover(&run.id).await.expect("recover twice");
    assert_eq!(first, second);
    assert_eq!(
        event_types(&f).await.len(),
        before.len(),
        "recovery neither transitioned nor emitted"
    );

    // A non-matching resolution is refused and changes nothing.
    let mismatch = engine
        .resolve_wait(
            &run.id,
            generation,
            WaitResolution::Approval {
                approval_id: "apr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string(),
            },
        )
        .await
        .expect_err("mismatched approval");
    assert_eq!(mismatch.code(), "CONFLICT_STATE");
    assert!(matches!(mismatch, RuntimeError::WaitMismatch { .. }));
    assert_eq!(event_types(&f).await.len(), before.len());

    let resumed = engine
        .resolve_wait(
            &run.id,
            generation,
            WaitResolution::Approval {
                approval_id: "apr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
            },
        )
        .await
        .expect("matching approval");
    assert_eq!(resumed.status, RunStatus::Running);
    let after = event_types(&f).await;
    assert_eq!(after.last().map(String::as_str), Some("run.resumed"));

    let third = engine.recover(&run.id).await.expect("recover after resume");
    assert_eq!(
        third,
        RecoveryOutcome::Continue {
            run_id: run.id.to_string()
        }
    );
    assert_eq!(event_types(&f).await.len(), after.len());
    pool.close().await;
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn recovery_never_retries_an_unsettled_effect() {
    let Some(f) = prepare("runtime_unsettled").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;
    let mut state = ProtocolState::new(run.id.to_string(), generation.get() as i64);
    state.pending_tool_calls.push(PendingToolCall {
        tool_call_id: "tc_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
        tool_name: "fs.write".to_string(),
        dispatch_token: "dispatch-token".to_string(),
        effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
        effect_status: "OUTCOME_UNKNOWN".to_string(),
    });
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("protocol state");

    let before = event_types(&f).await;
    let outcome = f.engine.recover(&run.id).await.expect("recover");
    assert_eq!(
        outcome,
        RecoveryOutcome::ReconcileEffect {
            effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
            tool_call_id: "tc_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
        }
    );
    assert_eq!(
        f.engine.recover(&run.id).await.expect("recover twice"),
        outcome
    );
    assert_eq!(event_types(&f).await.len(), before.len());
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Running
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn recovery_honours_a_cancellation_requested_before_the_crash_once() {
    let Some(f) = prepare("runtime_cancel_recover").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;
    let mut state = ProtocolState::new(run.id.to_string(), generation.get() as i64);
    state.cancellation_requested = true;
    state.cancellation_at = Some("2026-09-12T00:00:00Z".to_string());
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("protocol state");

    let before = event_types(&f).await;
    assert_eq!(
        f.engine.recover(&run.id).await.expect("recover"),
        RecoveryOutcome::Cancelled {
            run_id: run.id.to_string()
        }
    );
    let after = event_types(&f).await;
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after.last().map(String::as_str), Some("run.cancelled"));

    assert_eq!(
        f.engine.recover(&run.id).await.expect("recover twice"),
        RecoveryOutcome::Terminal {
            status: RunStatus::Cancelled
        }
    );
    assert_eq!(event_types(&f).await.len(), after.len());
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn stale_generation_is_fenced_before_any_write() {
    let Some(f) = prepare("runtime_fence").await else {
        blocked_marker();
        return;
    };
    let run = create_run(&f, 8).await;
    let before = event_types(&f).await;

    // The run advanced to generation 2 (a resumed controller), the caller still holds 1.
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE runs SET generation = 2 WHERE id = $1")
        .bind(run.id.to_string())
        .execute(&mut *tx)
        .await
        .expect("bump generation");
    tx.commit().await.expect("commit");

    let error = f
        .engine
        .enqueue(&run.id, Generation::new(1).expect("generation"))
        .await
        .expect_err("stale generation");
    assert_eq!(error.code(), "FENCED_STALE_GENERATION");
    assert!(matches!(
        error,
        RuntimeError::FencedStaleGeneration {
            received: 1,
            current: 2
        }
    ));
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Created
    );
    assert_eq!(event_types(&f).await.len(), before.len());

    // The current generation is accepted.
    let queued = f
        .engine
        .enqueue(&run.id, Generation::new(2).expect("generation"))
        .await
        .expect("current generation");
    assert_eq!(queued.status, RunStatus::Queued);
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn only_the_cancellation_that_did_the_work_reports_that_it_cancelled_the_run() {
    // A caller that counts its own cancellations needs to tell "I cancelled it" from "it was already
    // cancelled": under a storm, every caller sees the same resulting Run, so the resulting state
    // alone cannot distinguish them and each would claim the same cancellation.
    let Some(f) = prepare("runtime_cancel_won").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 9).await;

    let (first_run, first) = f
        .engine
        .store()
        .cancel_once(&run.id, run.generation)
        .await
        .expect("cancel");
    assert_eq!(first, CancellationWon::Cancelled);
    assert_eq!(first_run.status, RunStatus::Cancelled);

    let (second_run, second) = f
        .engine
        .store()
        .cancel_once(&run.id, run.generation)
        .await
        .expect("cancel twice");
    assert_eq!(
        second,
        CancellationWon::AlreadyCancelled,
        "the second call cancelled nothing"
    );
    // The state is identical either way, which is exactly why the distinction has to be reported.
    assert_eq!(second_run.status, first_run.status);
    assert_eq!(second_run.generation, first_run.generation);

    // `cancel` keeps its old shape for a caller that only wants the resulting state.
    let plain = f
        .engine
        .cancel(&run.id, run.generation)
        .await
        .expect("cancel");
    assert_eq!(plain.status, RunStatus::Cancelled);
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn cancellation_during_running_ends_cancelled_with_one_event() {
    let Some(f) = prepare("runtime_cancel_running").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let before = event_types(&f).await;
    let cancelled = f
        .engine
        .cancel(&run.id, run.generation)
        .await
        .expect("cancel");
    assert_eq!(cancelled.status, RunStatus::Cancelled);

    // Idempotent: a second cancel emits nothing.
    let again = f
        .engine
        .cancel(&run.id, run.generation)
        .await
        .expect("cancel twice");
    assert_eq!(again.status, RunStatus::Cancelled);

    let after = event_types(&f).await;
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after.last().map(String::as_str), Some("run.cancelled"));

    // A cancelled run never transitions to RUNNING again.
    let error = f
        .engine
        .transition_run(&run.id, run.generation, RunStatus::Running, None)
        .await
        .expect_err("cancelled run cannot run");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Cancelled
    );
    assert_eq!(event_types(&f).await.len(), after.len());
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn cancellation_during_waiting_ends_cancelled_with_one_event() {
    let Some(f) = prepare("runtime_cancel_waiting").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;
    f.engine
        .transition_run(
            &run.id,
            generation,
            RunStatus::WaitingTimer,
            Some("timer".to_string()),
        )
        .await
        .expect("park");
    let before = event_types(&f).await;
    let cancelled = f.engine.cancel(&run.id, generation).await.expect("cancel");
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    let after = event_types(&f).await;
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after.last().map(String::as_str), Some("run.cancelled"));
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn suspended_run_resumes_only_with_the_current_generation() {
    let Some(f) = prepare("runtime_suspend").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let suspended = f
        .engine
        .suspend(&run.id, run.generation, "TARGET_LOST")
        .await
        .expect("suspend");
    assert_eq!(suspended.status, RunStatus::Suspended);
    assert_eq!(suspended.terminal_reason.as_deref(), Some("TARGET_LOST"));

    // Generation advanced by a later controller generation.
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE runs SET generation = 3 WHERE id = $1")
        .bind(run.id.to_string())
        .execute(&mut *tx)
        .await
        .expect("bump generation");
    tx.commit().await.expect("commit");

    let stale = f
        .engine
        .resume(&run.id, run.generation)
        .await
        .expect_err("stale resume");
    assert_eq!(stale.code(), "FENCED_STALE_GENERATION");
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Suspended
    );

    let resumed = f
        .engine
        .resume(&run.id, Generation::new(3).expect("generation"))
        .await
        .expect("current generation resume");
    assert_eq!(resumed.status, RunStatus::Running);
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn attempts_are_appended_never_overwritten() {
    let Some(f) = prepare("runtime_attempts").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;
    let turn = f
        .engine
        .begin_turn(&run.id, generation, TurnInput::new(RunTriggerKind::Manual))
        .await
        .expect("turn");
    let step = f
        .engine
        .store()
        .record_step(&turn.id, generation, NewStep::new(StepKind::ToolCall))
        .await
        .expect("step");
    let (_, first) = f
        .engine
        .store()
        .dispatch_step(&step.id, generation)
        .await
        .expect("first dispatch");
    let (_, second) = f
        .engine
        .store()
        .dispatch_step(&step.id, generation)
        .await
        .expect("retry dispatch");

    assert_ne!(first.id, second.id, "a retry is a new identity");
    assert_eq!((first.seq, second.seq), (1, 2));
    let attempts = f
        .engine
        .store()
        .list_attempts(&step.id)
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0].status,
        AttemptStatus::Started,
        "the earlier attempt is untouched"
    );
    assert_eq!(attempts[1].generation, generation);
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn canonical_ownership_and_tenant_isolation_hold() {
    let Some(f) = prepare("runtime_ownership").await else {
        blocked_marker();
        return;
    };
    let run = create_run(&f, 8).await;

    // A step cannot exist without its turn, and a turn without its run: the schema's
    // foreign keys refuse both, so no gap can be written.
    let orphan_turn =
        CanonicalId::parse_typed("trn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE", Prefix::Turn).expect("turn id");
    let error = f
        .engine
        .store()
        .record_step(
            &orphan_turn,
            run.generation,
            NewStep::new(StepKind::ModelCall),
        )
        .await
        .expect_err("orphan step");
    assert_eq!(error.code(), "NOT_FOUND");

    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let orphan_step = sqlx::query(
        "INSERT INTO steps (id, tenant_id, turn_id, seq, kind, status) \
         VALUES ($1, $2, $3, 1, 'model_call', 'pending')",
    )
    .bind("stp_01J8Z3K6F1N8VQ2X5W9Y0EEEEE")
    .bind(TENANT)
    .bind(orphan_turn.to_string())
    .execute(&mut *tx)
    .await;
    let _ = tx.rollback().await;
    assert!(orphan_step.is_err(), "no step row without its turn");

    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let orphan_turn_row = sqlx::query(
        "INSERT INTO turns (id, tenant_id, run_id, seq, input_kind) \
         VALUES ($1, $2, $3, 1, 'manual')",
    )
    .bind("trn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE")
    .bind(TENANT)
    .bind("run_01J8Z3K6F1N8VQ2X5W9Y0EEEEE")
    .execute(&mut *tx)
    .await;
    let _ = tx.rollback().await;
    assert!(orphan_turn_row.is_err(), "no turn row without its run");

    // Tenant isolation: tenant B cannot read tenant A's run or events.
    seed_tenant(&f.pool, TENANT_B, USER_B, WORKSPACE_B, WORK_NODE_B).await;
    let other = engine_for(&f.pool, TENANT_B).await;
    let error = other
        .store()
        .load_run(&run.id)
        .await
        .expect_err("cross-tenant run");
    assert_eq!(error.code(), "NOT_FOUND");
    let other_events = EventStore::new(f.pool.clone())
        .read_events_after(TENANT_B, None, 100)
        .await
        .expect("events");
    assert!(other_events.is_empty(), "{other_events:?}");
    assert!(!events_of(&f, TENANT).await.is_empty());
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn unavailable_seams_name_the_task_that_owns_them() {
    let Some(f) = prepare("runtime_seams").await else {
        blocked_marker();
        return;
    };

    // Tool dispatch → RUN-011.
    let run = running_run(&f, 8).await;
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![tool_call(
            "tc_01J8Z3K6F1N8VQ2X5W9Y0EEEEE",
        )]));
    let error = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Message),
        )
        .await
        .expect_err("tool dispatch is not available yet");
    assert!(matches!(
        error,
        RuntimeError::SeamNotAvailable {
            owner: TOOL_DISPATCH_OWNER,
            ..
        }
    ));

    // Delegation → RUN-002.
    let delegation_run = running_run(&f, 8).await;
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![ModelProposal {
            delegate_requests: vec![quansio_server::runtime::state_machine::DelegationRequest {
                request_id: "req_1".to_string(),
                work_node_id: None,
                instruction: "summarise the report".to_string(),
            }],
            ..ModelProposal::default()
        }]));
    let error = engine
        .run_turn(
            &delegation_run.id,
            delegation_run.generation,
            TurnInput::new(RunTriggerKind::Message),
        )
        .await
        .expect_err("delegation is not available yet");
    assert!(matches!(
        error,
        RuntimeError::SeamNotAvailable {
            owner: DELEGATION_OWNER,
            ..
        }
    ));

    // Completion verification → RUN-008.
    let verification_run = running_run(&f, 8).await;
    let engine = f
        .engine
        .clone()
        .with_model_source(ScriptedModel::new(vec![completion(
            "clm_01J8Z3K6F1N8VQ2X5W9Y0EEEEE",
        )]));
    let error = engine
        .run_turn(
            &verification_run.id,
            verification_run.generation,
            TurnInput::new(RunTriggerKind::Message),
        )
        .await
        .expect_err("verification is not available yet");
    assert!(matches!(
        error,
        RuntimeError::SeamNotAvailable {
            owner: VERIFICATION_OWNER,
            ..
        }
    ));
    // A model completion claim alone never marks work succeeded.
    assert_eq!(
        engine
            .store()
            .load_run(&verification_run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Running
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn protocol_state_round_trips_for_recovery() {
    let Some(f) = prepare("runtime_protocol").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let mut state = ProtocolState::new(run.id.to_string(), run.generation.get() as i64);
    state.open_questions = vec!["q_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string()];
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("store");
    let loaded = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("load")
        .expect("state");
    assert_eq!(loaded, state);

    // The raw store agrees (CORE-006 owns the same row).
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let direct = ProtocolStateStore::load(&mut tx, TENANT, &run.id.to_string())
        .await
        .expect("direct load")
        .expect("state");
    assert_eq!(direct, state);
    tx.commit().await.expect("commit");
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn parked_run_is_not_released_by_a_mismatched_wait_state() {
    let Some(f) = prepare("runtime_wait_mismatch").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f, 8).await;
    let generation = run.generation;
    f.engine
        .transition_run(
            &run.id,
            generation,
            RunStatus::WaitingQuestion,
            Some("question".to_string()),
        )
        .await
        .expect("park");
    let mut state = ProtocolState::new(run.id.to_string(), generation.get() as i64);
    state.open_questions = vec!["q_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string()];
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("protocol state");
    let before = event_types(&f).await;

    // A timer resolution cannot release a run waiting on a question.
    let error = f
        .engine
        .resolve_wait(
            &run.id,
            generation,
            WaitResolution::Timer {
                key: "wait_1".to_string(),
            },
        )
        .await
        .expect_err("wrong wait family");
    assert!(matches!(error, RuntimeError::WaitMismatch { .. }));
    assert_eq!(event_types(&f).await.len(), before.len());

    // The matching question answer releases it.
    let resumed = f
        .engine
        .resolve_wait(
            &run.id,
            generation,
            WaitResolution::Question {
                question_id: "q_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string(),
            },
        )
        .await
        .expect("matching question");
    assert_eq!(resumed.status, RunStatus::Running);
    drop_pool(&f.pool, &f.name).await;
}
