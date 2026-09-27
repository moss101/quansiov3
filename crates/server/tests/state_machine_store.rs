//! Run/Turn/Step store refusals, wait families and takeover fencing (QA-001 coverage of RUN-001).
//!
//! These suites complement `tests/runtime_state_machine.rs`. The pure tests pin the DOMAIN.md
//! §5.2–§5.5 state tables and the budget snapshot parser; the database tests drive
//! `RuntimeStore` against a real scratch PostgreSQL database through every `WAITING_*` family
//! `resolve_wait` releases (event, timer, child, takeover), the refusals of `resume`,
//! `begin_turn`, `record_step`, `dispatch_step` and `complete_step`, protocol-state writes for an
//! unknown run, the takeover park/release paths that must not move a run, and the causation
//! stamped on run events. Every refusal is checked to write nothing.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use quansio_core::{CanonicalId, CausationId, CorrelationId, Generation, Prefix, UlidGenerator};
use quansio_events::{EventBatch, EventStore, RuntimeEvent};
use serde_json::json;
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::runtime::protocol_state::{
    BrowserControl, BrowserControlHolder, ProtocolState, Wait, WaitKind,
};
use quansio_server::runtime::state_machine::{
    AttemptStatus, Budget, NewRun, NewStep, Run, RunStatus, RunTriggerKind, RuntimeEngine,
    RuntimeError, RuntimeIdentity, RuntimeStore, StepKind, StepStatus, TurnInput, TurnStatus,
    WaitResolution, DEFAULT_MAX_STEPS,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const MISSING_RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const MISSING_TURN: &str = "trn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const MISSING_STEP: &str = "stp_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const CAUSING_EVENT: &str = "evt_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";

// ---------------------------------------------------------------------------------- pure tables

#[test]
fn budget_snapshot_defaults_a_missing_or_null_max_steps_and_refuses_malformed_ones() {
    assert_eq!(
        Budget::from_snapshot(&json!({})).expect("missing"),
        Budget::new(DEFAULT_MAX_STEPS)
    );
    assert_eq!(
        Budget::from_snapshot(&json!({ "max_steps": null })).expect("null"),
        Budget::new(DEFAULT_MAX_STEPS)
    );
    assert_eq!(
        Budget::from_snapshot(&json!({ "max_steps": 3 })).expect("three"),
        Budget::new(3)
    );
    for malformed in [json!({ "max_steps": "4" }), json!({ "max_steps": -1 })] {
        assert!(
            matches!(
                Budget::from_snapshot(&malformed),
                Err(RuntimeError::InvalidArgument(message)) if message.contains("not an integer")
            ),
            "{malformed}"
        );
    }
    assert!(matches!(
        Budget::from_snapshot(&json!({ "max_steps": u64::from(u32::MAX) + 1 })),
        Err(RuntimeError::InvalidArgument(message)) if message.contains("out of range")
    ));
}

#[test]
fn every_wait_resolution_names_its_waiting_state_and_key() {
    let cases = [
        (
            WaitResolution::Approval {
                approval_id: "apr_1".to_string(),
            },
            RunStatus::WaitingApproval,
            "apr_1",
        ),
        (
            WaitResolution::Question {
                question_id: "q_1".to_string(),
            },
            RunStatus::WaitingQuestion,
            "q_1",
        ),
        (
            WaitResolution::Event {
                key: "evt-key".to_string(),
            },
            RunStatus::WaitingEvent,
            "evt-key",
        ),
        (
            WaitResolution::Timer {
                key: "timer-key".to_string(),
            },
            RunStatus::WaitingTimer,
            "timer-key",
        ),
        (
            WaitResolution::Child {
                agent_thread_id: AGENT.to_string(),
            },
            RunStatus::WaitingChild,
            AGENT,
        ),
        (
            WaitResolution::Takeover {
                session_id: "bs_1".to_string(),
            },
            RunStatus::WaitingTakeover,
            "bs_1",
        ),
    ];
    for (resolution, state, key) in cases {
        assert_eq!(resolution.waiting_state(), state, "{resolution:?}");
        assert!(state.is_waiting());
        assert_eq!(resolution.key(), key);
    }
}

#[test]
fn state_enums_refuse_unknown_stored_values_and_follow_the_domain_tables() {
    assert!(matches!(
        RunTriggerKind::from_db_str("cron"),
        Err(RuntimeError::UnknownState { .. })
    ));
    assert!(matches!(
        TurnStatus::from_db_str("paused"),
        Err(RuntimeError::UnknownState { .. })
    ));
    assert!(matches!(
        StepKind::from_db_str("teleport"),
        Err(RuntimeError::UnknownState { .. })
    ));
    assert!(matches!(
        StepStatus::from_db_str("running"),
        Err(RuntimeError::UnknownState { .. })
    ));
    assert!(matches!(
        AttemptStatus::from_db_str("retrying"),
        Err(RuntimeError::UnknownState { .. })
    ));
    for kind in RunTriggerKind::ALL {
        assert_eq!(
            RunTriggerKind::from_db_str(kind.as_db_str()).expect("round trip"),
            kind
        );
    }

    // A step is terminal once completed, failed or cancelled; pending and unknown are not.
    assert!(StepStatus::Completed.is_terminal());
    assert!(StepStatus::Failed.is_terminal());
    assert!(StepStatus::Cancelled.is_terminal());
    assert!(!StepStatus::Pending.is_terminal());
    assert!(!StepStatus::Unknown.is_terminal());
    assert!(!StepStatus::Dispatched.is_terminal());

    // A pending step may be dispatched, failed or cancelled, never completed directly.
    assert!(StepStatus::Pending.can_transition_to(StepStatus::Dispatched));
    assert!(StepStatus::Pending.can_transition_to(StepStatus::Cancelled));
    assert!(!StepStatus::Pending.can_transition_to(StepStatus::Completed));
    // An unknown outcome settles by reconciliation, never back to dispatched.
    assert!(StepStatus::Unknown.can_transition_to(StepStatus::Completed));
    assert!(StepStatus::Unknown.can_transition_to(StepStatus::Failed));
    assert!(!StepStatus::Unknown.can_transition_to(StepStatus::Dispatched));
    for to in [
        StepStatus::Pending,
        StepStatus::Dispatched,
        StepStatus::Completed,
        StepStatus::Unknown,
    ] {
        assert!(!StepStatus::Completed.can_transition_to(to));
        assert!(!StepStatus::Cancelled.can_transition_to(to));
    }

    // A started attempt ends exactly once, in one of its four outcomes.
    for to in [
        AttemptStatus::Succeeded,
        AttemptStatus::Failed,
        AttemptStatus::TimedOut,
        AttemptStatus::Fenced,
    ] {
        assert!(AttemptStatus::Started.can_transition_to(to));
        assert!(!to.can_transition_to(AttemptStatus::Started));
    }
    assert!(!AttemptStatus::Started.can_transition_to(AttemptStatus::Started));

    // A parked run leaves its wait only by resuming, cancelling, failing or suspending.
    for waiting in [
        RunStatus::WaitingEvent,
        RunStatus::WaitingTimer,
        RunStatus::WaitingChild,
        RunStatus::WaitingTakeover,
    ] {
        assert!(waiting.can_transition_to(RunStatus::Running));
        assert!(waiting.can_transition_to(RunStatus::Cancelled));
        assert!(!waiting.can_transition_to(RunStatus::Succeeded));
        assert!(!waiting.can_transition_to(RunStatus::WaitingApproval));
    }
}

// ----------------------------------------------------------------------------------- fixture

struct Fixture {
    name: String,
    pool: PgPool,
    engine: RuntimeEngine,
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity::system(
        TENANT,
        "state-machine-store-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
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
    let engine = RuntimeEngine::new(pool.clone(), identity()).expect("engine");
    Some(Fixture { name, pool, engine })
}

fn id(value: &str, prefix: Prefix) -> CanonicalId {
    CanonicalId::parse_typed(value, prefix).expect("canonical id")
}

async fn created_run(store: &RuntimeStore) -> Run {
    store
        .create_run(NewRun::new(
            WORKSPACE,
            id(WORK_NODE, Prefix::WorkNode),
            id(AGENT, Prefix::AgentThread),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("create run")
}

async fn running_run(store: &RuntimeStore) -> Run {
    let run = created_run(store).await;
    let run = store
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    store.start(&run.id, run.generation).await.expect("start")
}

async fn parked_run(store: &RuntimeStore, state: RunStatus, protocol: ProtocolState) -> Run {
    let run = running_run(store).await;
    let mut protocol = protocol;
    protocol.run_id = run.id.to_string();
    protocol.generation = run.generation.get() as i64;
    store
        .store_protocol_state(&protocol)
        .await
        .expect("protocol state");
    store
        .transition_run(&run.id, run.generation, state, Some("parked".to_string()))
        .await
        .expect("park")
}

async fn events(pool: &PgPool) -> Vec<RuntimeEvent> {
    EventStore::new(pool.clone())
        .read_events_after(TENANT, None, 10_000)
        .await
        .expect("events")
}

fn blank_state() -> ProtocolState {
    ProtocolState::new(MISSING_RUN, 1)
}

fn wait(kind: WaitKind, key: &str) -> Wait {
    Wait {
        kind,
        key: key.to_string(),
        expires_at: None,
    }
}

fn user_control(session: &str) -> BrowserControl {
    BrowserControl {
        session_id: session.to_string(),
        holder: BrowserControlHolder::User,
        since: Some("2026-09-27T00:00:00Z".to_string()),
    }
}

/// Resolve a parked run with a mismatched key of the right family (refused, nothing written),
/// then with the matching key (released to RUNNING, the wait gone from the protocol state).
async fn assert_wait_family(
    f: &Fixture,
    run: &Run,
    wrong: WaitResolution,
    right: WaitResolution,
    expected_pending: &str,
    still_pending: impl Fn(&ProtocolState) -> bool,
) {
    let store = f.engine.store();
    let before = events(&f.pool).await.len();
    let refused = store
        .resolve_wait(&run.id, run.generation, wrong.clone())
        .await
        .expect_err("a mismatched key");
    assert!(
        matches!(&refused, RuntimeError::WaitMismatch { expected, received, .. }
            if expected == expected_pending && received == wrong.key()),
        "{refused:?}"
    );
    assert_eq!(
        events(&f.pool).await.len(),
        before,
        "a mismatch writes nothing"
    );
    assert_eq!(
        store.load_run(&run.id).await.expect("run").status,
        right.waiting_state()
    );

    let released = store
        .resolve_wait(&run.id, run.generation, right)
        .await
        .expect("the matching resolution");
    assert_eq!(released.status, RunStatus::Running);
    let state = store
        .load_protocol_state(&run.id)
        .await
        .expect("load")
        .expect("state");
    assert!(!still_pending(&state), "the released wait is removed");
    let last = events(&f.pool).await.pop().expect("event");
    assert_eq!(last.event_type.to_string(), "run.resumed");
    assert_eq!(last.aggregate_id, run.id.to_string());
}

// -------------------------------------------------------------------------------- DB suites

#[tokio::test]
async fn resolve_wait_releases_event_timer_child_and_takeover_waits_only_on_their_key() {
    let Some(f) = prepare("qa001_sm_waits").await else {
        blocked_marker();
        return;
    };
    let store = f.engine.store();

    let mut state = blank_state();
    state.waits = vec![wait(WaitKind::Event, "deploy-finished")];
    let run = parked_run(store, RunStatus::WaitingEvent, state).await;
    assert_wait_family(
        &f,
        &run,
        WaitResolution::Event {
            key: "deploy-started".to_string(),
        },
        WaitResolution::Event {
            key: "deploy-finished".to_string(),
        },
        "deploy-finished",
        |state| !state.waits.is_empty(),
    )
    .await;

    // A timer key does not release an event wait of the same name.
    let mut state = blank_state();
    state.waits = vec![
        wait(WaitKind::Event, "nightly"),
        wait(WaitKind::Timer, "nightly"),
    ];
    let run = parked_run(store, RunStatus::WaitingTimer, state).await;
    assert_wait_family(
        &f,
        &run,
        WaitResolution::Timer {
            key: "hourly".to_string(),
        },
        WaitResolution::Timer {
            key: "nightly".to_string(),
        },
        "nightly",
        |state| state.waits.iter().any(|wait| wait.kind == WaitKind::Timer),
    )
    .await;
    let remaining = store
        .load_protocol_state(&run.id)
        .await
        .expect("load")
        .expect("state");
    assert_eq!(
        remaining.waits,
        vec![wait(WaitKind::Event, "nightly")],
        "only the timer wait was released"
    );

    let mut state = blank_state();
    state.child_agent_threads = vec![AGENT.to_string()];
    let run = parked_run(store, RunStatus::WaitingChild, state).await;
    assert_wait_family(
        &f,
        &run,
        WaitResolution::Child {
            agent_thread_id: "ath_01J8Z3K6F1N8VQ2X5W9Y0KKKKK".to_string(),
        },
        WaitResolution::Child {
            agent_thread_id: AGENT.to_string(),
        },
        AGENT,
        |state| !state.child_agent_threads.is_empty(),
    )
    .await;

    // A takeover is keyed by the browser session holding control; nothing else is pending, so
    // the mismatch names that session.
    let mut state = blank_state();
    state.browser_control = Some(user_control("bs_held"));
    let run = parked_run(store, RunStatus::WaitingTakeover, state).await;
    assert_wait_family(
        &f,
        &run,
        WaitResolution::Takeover {
            session_id: "bs_other".to_string(),
        },
        WaitResolution::Takeover {
            session_id: "bs_held".to_string(),
        },
        "bs_held",
        |state| state.browser_control.is_some(),
    )
    .await;

    // A question wait with an unknown question id names the open question.
    let mut state = blank_state();
    state.open_questions = vec!["q_open".to_string()];
    let run = parked_run(store, RunStatus::WaitingQuestion, state).await;
    assert_wait_family(
        &f,
        &run,
        WaitResolution::Question {
            question_id: "q_closed".to_string(),
        },
        WaitResolution::Question {
            question_id: "q_open".to_string(),
        },
        "q_open",
        |state| !state.open_questions.is_empty(),
    )
    .await;

    // A parked run with no protocol state at all is never released.
    let run = running_run(store).await;
    let run = store
        .transition_run(&run.id, run.generation, RunStatus::WaitingEvent, None)
        .await
        .expect("park without state");
    let refused = store
        .resolve_wait(
            &run.id,
            run.generation,
            WaitResolution::Event {
                key: "anything".to_string(),
            },
        )
        .await
        .expect_err("no durable state");
    assert!(
        matches!(&refused, RuntimeError::WaitMismatch { received, .. } if received == "no durable protocol state"),
        "{refused:?}"
    );
    // A pending state with nothing named reports that no wait is pending.
    store
        .store_protocol_state(&ProtocolState::new(
            run.id.to_string(),
            run.generation.get() as i64,
        ))
        .await
        .expect("empty state");
    let refused = store
        .resolve_wait(
            &run.id,
            run.generation,
            WaitResolution::Event {
                key: "anything".to_string(),
            },
        )
        .await
        .expect_err("nothing pending");
    assert!(
        matches!(&refused, RuntimeError::WaitMismatch { expected, .. } if expected == "no pending wait"),
        "{refused:?}"
    );
    assert_eq!(
        store.load_run(&run.id).await.expect("run").status,
        RunStatus::WaitingEvent
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn resume_begin_turn_and_the_step_writes_refuse_rows_in_the_wrong_state() {
    let Some(f) = prepare("qa001_sm_refusals").await else {
        blocked_marker();
        return;
    };
    let store = f.engine.store();
    assert_eq!(store.identity().tenant_id, TENANT);
    assert_eq!(f.engine.identity().tenant_id, TENANT);

    // resume only releases a SUSPENDED run.
    let run = running_run(store).await;
    let before = events(&f.pool).await.len();
    let refused = store
        .resume(&run.id, run.generation)
        .await
        .expect_err("a running run is not suspended");
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { from, to, .. } if from == "RUNNING" && to == "RUNNING"),
        "{refused:?}"
    );
    // An unknown run is NotFound, on the locked path too.
    let missing = id(MISSING_RUN, Prefix::Run);
    assert!(matches!(
        store.resume(&missing, Generation::INITIAL).await,
        Err(RuntimeError::NotFound { entity: "run", .. })
    ));
    assert_eq!(
        events(&f.pool).await.len(),
        before,
        "refusals write nothing"
    );

    // begin_turn needs a RUNNING run.
    let queued = created_run(store).await;
    let queued = store
        .enqueue(&queued.id, queued.generation)
        .await
        .expect("enqueue");
    let refused = store
        .begin_turn(
            &queued.id,
            queued.generation,
            TurnInput::new(RunTriggerKind::Message),
        )
        .await
        .expect_err("a queued run takes no turn");
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { from, .. } if from == "QUEUED"),
        "{refused:?}"
    );
    assert!(store
        .list_turns(&queued.id)
        .await
        .expect("turns")
        .is_empty());

    // A finished turn takes no more steps.
    let turn = store
        .begin_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Message).with_reference("msg_1"),
        )
        .await
        .expect("turn");
    let step = store
        .record_step(&turn.id, run.generation, NewStep::new(StepKind::ModelCall))
        .await
        .expect("step");
    let (_, attempt) = store
        .dispatch_step(&step.id, run.generation)
        .await
        .expect("dispatch");
    let completed = store
        .complete_step(
            &step.id,
            run.generation,
            &attempt.id,
            StepStatus::Completed,
            AttemptStatus::Succeeded,
            None,
            Vec::new(),
        )
        .await
        .expect("complete");
    assert_eq!(completed.status, StepStatus::Completed);

    // A completed step cannot be dispatched again.
    let refused = store
        .dispatch_step(&step.id, run.generation)
        .await
        .expect_err("completed steps are never re-dispatched");
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { entity: "step", from, .. } if from == "completed"),
        "{refused:?}"
    );
    assert_eq!(
        store.list_attempts(&step.id).await.expect("attempts").len(),
        1
    );

    // An attempt of another step does not settle this one.
    let other = store
        .record_step(&turn.id, run.generation, NewStep::new(StepKind::ToolCall))
        .await
        .expect("other step");
    let third = store
        .record_step(&turn.id, run.generation, NewStep::new(StepKind::ToolCall))
        .await
        .expect("third step");
    let (_, third_attempt) = store
        .dispatch_step(&third.id, run.generation)
        .await
        .expect("dispatch third");
    store
        .dispatch_step(&other.id, run.generation)
        .await
        .expect("dispatch other");
    let refused = store
        .complete_step(
            &other.id,
            run.generation,
            &third_attempt.id,
            StepStatus::Completed,
            AttemptStatus::Succeeded,
            None,
            Vec::new(),
        )
        .await
        .expect_err("the attempt belongs to another step");
    assert!(
        matches!(&refused, RuntimeError::NotFound { entity: "attempt", id, .. } if *id == third_attempt.id.to_string()),
        "{refused:?}"
    );
    assert_eq!(
        store.load_step(&other.id).await.expect("other").status,
        StepStatus::Dispatched
    );

    // A cancelled step emits step.cancelled.
    let cancelled = store
        .complete_step(
            &third.id,
            run.generation,
            &third_attempt.id,
            StepStatus::Cancelled,
            AttemptStatus::Fenced,
            None,
            Vec::new(),
        )
        .await
        .expect("cancel step");
    assert_eq!(cancelled.status, StepStatus::Cancelled);
    assert_eq!(
        events(&f.pool)
            .await
            .pop()
            .expect("event")
            .event_type
            .to_string(),
        "step.cancelled"
    );

    store
        .finish_turn(&turn.id, run.generation, TurnStatus::Completed)
        .await
        .expect("finish turn");
    let refused = store
        .record_step(&turn.id, run.generation, NewStep::new(StepKind::ModelCall))
        .await
        .expect_err("a completed turn takes no steps");
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { entity: "turn", from, .. } if from == "completed"),
        "{refused:?}"
    );
    assert_eq!(store.list_steps(&turn.id).await.expect("steps").len(), 3);
    let refused = store
        .finish_turn(&turn.id, run.generation, TurnStatus::Active)
        .await
        .expect_err("a finished turn never reopens");
    assert!(matches!(
        refused,
        RuntimeError::IllegalTransition { entity: "turn", .. }
    ));

    // Unknown turns and steps are NotFound.
    let missing_turn = id(MISSING_TURN, Prefix::Turn);
    let missing_step = id(MISSING_STEP, Prefix::Step);
    assert!(matches!(
        store.load_turn(&missing_turn).await,
        Err(RuntimeError::NotFound { entity: "turn", .. })
    ));
    assert!(matches!(
        store
            .record_step(
                &missing_turn,
                run.generation,
                NewStep::new(StepKind::ModelCall)
            )
            .await,
        Err(RuntimeError::NotFound { entity: "turn", .. })
    ));
    assert!(matches!(
        store.load_step(&missing_step).await,
        Err(RuntimeError::NotFound { entity: "step", .. })
    ));
    assert!(matches!(
        store.dispatch_step(&missing_step, run.generation).await,
        Err(RuntimeError::NotFound { entity: "step", .. })
    ));

    // None of the refusals moved the run.
    assert_eq!(
        store.load_run(&run.id).await.expect("run").status,
        RunStatus::Running
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn protocol_state_writes_refuse_a_run_this_tenant_cannot_see() {
    let Some(f) = prepare("qa001_sm_protocol").await else {
        blocked_marker();
        return;
    };
    let store = f.engine.store();
    let refused = store
        .store_protocol_state(&ProtocolState::new(MISSING_RUN, 1))
        .await
        .expect_err("unknown run");
    assert!(
        matches!(&refused, RuntimeError::NotFound { entity: "run", id, .. } if id == MISSING_RUN),
        "{refused:?}"
    );
    let refused = store
        .update_protocol_state(
            &id(MISSING_RUN, Prefix::Run),
            Generation::INITIAL,
            |state| {
                state.open_questions.push("q_never".to_string());
            },
        )
        .await
        .expect_err("unknown run");
    assert!(
        matches!(&refused, RuntimeError::NotFound { entity: "run", .. }),
        "{refused:?}"
    );
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM protocol_states")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    assert_eq!(rows, 0, "no state is written for an unknown run");

    // A malformed tenant id is refused at construction.
    assert!(RuntimeStore::new(
        f.pool.clone(),
        RuntimeIdentity::system(
            "tenant-1",
            "test",
            CorrelationId::generate(&mut UlidGenerator::new())
        )
    )
    .is_err());
    assert!(!store.pool().is_closed());

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn takeover_park_and_release_never_move_a_run_that_is_not_theirs_to_move() {
    let Some(f) = prepare("qa001_sm_takeover").await else {
        blocked_marker();
        return;
    };
    let store = f.engine.store();
    let events_store = EventStore::new(f.pool.clone());

    // A terminal run has nothing to fence: park and release return None and stage nothing.
    let finished = running_run(store).await;
    let finished = store
        .cancel(&finished.id, finished.generation)
        .await
        .expect("cancel");
    let mut tx = events_store
        .begin_tenant_transaction(TENANT)
        .await
        .expect("tx");
    let mut batch = EventBatch::default();
    assert_eq!(
        store
            .park_for_takeover_in(&mut tx, &mut batch, &finished.id, "bs_1", "now")
            .await
            .expect("park terminal"),
        None
    );
    assert_eq!(
        store
            .release_takeover_in(&mut tx, &mut batch, &finished.id, "bs_1")
            .await
            .expect("release terminal"),
        None
    );
    assert!(batch.is_empty());
    tx.commit().await.expect("commit");

    // A run parked on a question keeps that wait; the takeover fence goes into its state only.
    let mut state = blank_state();
    state.open_questions = vec!["q_1".to_string()];
    let asking = parked_run(store, RunStatus::WaitingQuestion, state).await;
    let mut tx = events_store
        .begin_tenant_transaction(TENANT)
        .await
        .expect("tx");
    let mut batch = EventBatch::default();
    let parked = store
        .park_for_takeover_in(
            &mut tx,
            &mut batch,
            &asking.id,
            "bs_1",
            "2026-09-27T00:00:00Z",
        )
        .await
        .expect("park")
        .expect("live run");
    assert_eq!(parked.status, RunStatus::WaitingQuestion);
    assert!(batch.is_empty(), "no run transition is staged");
    tx.commit().await.expect("commit");
    let fenced = store
        .load_protocol_state(&asking.id)
        .await
        .expect("load")
        .expect("state");
    assert_eq!(fenced.browser_control, Some(user_control("bs_1")));
    assert_eq!(fenced.open_questions, vec!["q_1".to_string()]);

    // Handing back another session releases nothing.
    let mut tx = events_store
        .begin_tenant_transaction(TENANT)
        .await
        .expect("tx");
    let mut batch = EventBatch::default();
    let untouched = store
        .release_takeover_in(&mut tx, &mut batch, &asking.id, "bs_2")
        .await
        .expect("release other")
        .expect("live run");
    assert_eq!(untouched.status, RunStatus::WaitingQuestion);
    assert!(batch.is_empty());
    tx.commit().await.expect("commit");
    assert_eq!(
        store
            .load_protocol_state(&asking.id)
            .await
            .expect("load")
            .expect("state")
            .browser_control,
        Some(user_control("bs_1")),
        "the fence for bs_1 is still held"
    );

    // Handing back the held session clears the fence but leaves the question wait alone.
    let mut tx = events_store
        .begin_tenant_transaction(TENANT)
        .await
        .expect("tx");
    let mut batch = EventBatch::default();
    let released = store
        .release_takeover_in(&mut tx, &mut batch, &asking.id, "bs_1")
        .await
        .expect("release held")
        .expect("live run");
    assert_eq!(released.status, RunStatus::WaitingQuestion);
    assert!(batch.is_empty());
    tx.commit().await.expect("commit");
    let cleared = store
        .load_protocol_state(&asking.id)
        .await
        .expect("load")
        .expect("state");
    assert_eq!(cleared.browser_control, None);
    assert_eq!(cleared.open_questions, vec!["q_1".to_string()]);
    assert_eq!(
        store.load_run(&asking.id).await.expect("run").status,
        RunStatus::WaitingQuestion
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn run_events_carry_causation_and_an_unrecoverable_block_is_run_blocked() {
    let Some(f) = prepare("qa001_sm_causation").await else {
        blocked_marker();
        return;
    };
    let causation: CausationId = CAUSING_EVENT.parse().expect("causation");
    let store = RuntimeStore::new(
        f.pool.clone(),
        identity().with_causation_id(causation.clone()),
    )
    .expect("store");
    let run = running_run(&store).await;
    let blocked = store
        .transition_run(
            &run.id,
            run.generation,
            RunStatus::BlockedUnrecoverable,
            Some("TARGET_LOST".to_string()),
        )
        .await
        .expect("block");
    assert_eq!(blocked.status, RunStatus::BlockedUnrecoverable);
    assert_eq!(blocked.terminal_reason.as_deref(), Some("TARGET_LOST"));
    assert!(blocked.ended_at.is_some());
    let run_events: Vec<RuntimeEvent> = events(&f.pool)
        .await
        .into_iter()
        .filter(|event| event.aggregate_id == run.id.to_string())
        .collect();
    let last = run_events.last().expect("event");
    assert_eq!(last.event_type.to_string(), "run.blocked");
    assert_eq!(last.payload["terminal_reason"], "TARGET_LOST");
    assert!(
        run_events
            .iter()
            .all(|event| event.causation_id.as_ref() == Some(&causation)),
        "every run event carries the causing event"
    );

    drop_pool(&f.pool, &f.name).await;
}
