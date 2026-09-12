//! Illegal state transitions fail closed and leave the row unchanged (CORE-004).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, WORKSPACE};
use quansio_graph::{
    AgentKind, AttemptStatus, GraphError, GraphStore, NewAgentThread, NewRun, NewStep, NewTurn,
    RunStatus, RunTriggerKind, StepKind, StepStatus, TurnStatus, WorkNodeKind, WorkNodeStatus,
};

fn actor() -> serde_json::Value {
    serde_json::json!({"kind": "user", "id": "usr_seed"})
}

#[tokio::test]
async fn work_node_transitions_fail_closed() {
    let Some((name, pool)) = prepare("work_transition").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let node = store
        .create_node(quansio_graph::NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "transition",
            actor(),
        ))
        .await
        .expect("create node");
    assert_eq!(node.status, WorkNodeStatus::Draft);

    // draft -> in_progress is not in the state machine.
    let error = store
        .transition_node(&node.id, node.revision, WorkNodeStatus::InProgress)
        .await
        .expect_err("illegal transition");
    match &error {
        GraphError::IllegalTransition { entity, from, to } => {
            assert_eq!(entity.as_str(), "work_node");
            assert_eq!(from, "draft");
            assert_eq!(to, "in_progress");
        }
        other => panic!("unexpected error {other:?}"),
    }
    assert_eq!(error.code(), "CONFLICT_STATE");
    let unchanged = store.get_node(&node.id).await.expect("node");
    assert_eq!(unchanged.status, WorkNodeStatus::Draft);
    assert_eq!(unchanged.revision, node.revision);

    // draft -> done is refused by the generic transition (verification path only).
    let error = store
        .transition_node(&node.id, node.revision, WorkNodeStatus::Done)
        .await
        .expect_err("direct done");
    assert!(
        matches!(error, GraphError::VerificationRequired { .. }),
        "{error:?}"
    );

    // The legal path reaches done only through mark_verification_passed.
    let ready = store
        .transition_node(&node.id, node.revision, WorkNodeStatus::Ready)
        .await
        .expect("ready");
    let running = store
        .transition_node(&ready.id, ready.revision, WorkNodeStatus::InProgress)
        .await
        .expect("in_progress");
    let verifying = store
        .transition_node(&running.id, running.revision, WorkNodeStatus::Verifying)
        .await
        .expect("verifying");
    let done = store
        .mark_verification_passed(&verifying.id, verifying.revision)
        .await
        .expect("verification passed");
    assert_eq!(done.status, WorkNodeStatus::Done);

    // done is terminal.
    let error = store
        .transition_node(&done.id, done.revision, WorkNodeStatus::InProgress)
        .await
        .expect_err("terminal");
    assert!(
        matches!(error, GraphError::IllegalTransition { .. }),
        "{error:?}"
    );

    drop_pool(&pool, &name).await;
}

async fn runtime_fixture(
    store: &GraphStore,
) -> (quansio_graph::Run, quansio_graph::Turn, quansio_graph::Step) {
    let node = store
        .create_node(quansio_graph::NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "runtime",
            actor(),
        ))
        .await
        .expect("create node");
    let thread = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("create thread");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE,
            node.id,
            thread.id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("create run");
    let turn = store
        .create_turn(&run.id, NewTurn::new("manual"))
        .await
        .expect("create turn");
    let step = store
        .create_step(&turn.id, NewStep::new(StepKind::ModelCall))
        .await
        .expect("create step");
    (run, turn, step)
}

#[tokio::test]
async fn run_transitions_fail_closed_and_leave_the_row_unchanged() {
    let Some((name, pool)) = prepare("run_transition").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let (run, _turn, _step) = runtime_fixture(&store).await;
    assert_eq!(run.status, RunStatus::Created);

    // CREATED -> RUNNING skips QUEUED.
    let error = store
        .transition_run(&run.id, RunStatus::Running, None)
        .await
        .expect_err("illegal run transition");
    assert!(
        matches!(error, GraphError::IllegalTransition { .. }),
        "{error:?}"
    );
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");
    let unchanged = store.get_run(&run.id).await.expect("run");
    assert_eq!(unchanged.status, RunStatus::Created);

    // A run cannot succeed without verifying.
    let error = store
        .transition_run(&run.id, RunStatus::Succeeded, None)
        .await
        .expect_err("no direct success");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    let queued = store
        .transition_run(&run.id, RunStatus::Queued, None)
        .await
        .expect("queued");
    let running = store
        .transition_run(&queued.id, RunStatus::Running, None)
        .await
        .expect("running");
    assert!(running.started_at.is_some());
    let waiting = store
        .transition_run(&running.id, RunStatus::WaitingApproval, None)
        .await
        .expect("waiting");
    let resumed = store
        .transition_run(&waiting.id, RunStatus::Running, None)
        .await
        .expect("resumed");
    let verifying = store
        .transition_run(&resumed.id, RunStatus::Verifying, None)
        .await
        .expect("verifying");
    let succeeded = store
        .transition_run(&verifying.id, RunStatus::Succeeded, None)
        .await
        .expect("succeeded");
    assert!(succeeded.ended_at.is_some());

    // Terminal runs never transition again.
    let error = store
        .transition_run(&succeeded.id, RunStatus::Running, None)
        .await
        .expect_err("terminal run");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    // SUSPENDED is reachable from RUNNING and returns to RUNNING.
    let second = store
        .create_run(NewRun::new(
            WORKSPACE,
            succeeded.work_node_id,
            succeeded.agent_thread_id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("second run");
    let second = store
        .transition_run(&second.id, RunStatus::Queued, None)
        .await
        .expect("queued");
    let second = store
        .transition_run(&second.id, RunStatus::Running, None)
        .await
        .expect("running");
    let second = store
        .transition_run(&second.id, RunStatus::Suspended, None)
        .await
        .expect("suspended");
    store
        .transition_run(&second.id, RunStatus::Running, None)
        .await
        .expect("resumed from suspended");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn step_transitions_fail_closed_and_leave_the_row_unchanged() {
    let Some((name, pool)) = prepare("step_transition").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let (_run, _turn, step) = runtime_fixture(&store).await;
    assert_eq!(step.status, StepStatus::Pending);

    // pending -> completed skips dispatch.
    let error = store
        .transition_step(&step.id, StepStatus::Completed)
        .await
        .expect_err("illegal step transition");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");
    let unchanged = store.get_step(&step.id).await.expect("step");
    assert_eq!(unchanged.status, StepStatus::Pending);

    let dispatched = store
        .transition_step(&step.id, StepStatus::Dispatched)
        .await
        .expect("dispatched");
    let completed = store
        .transition_step(&dispatched.id, StepStatus::Completed)
        .await
        .expect("completed");
    assert_eq!(completed.status, StepStatus::Completed);

    // A completed step is terminal.
    let error = store
        .transition_step(&completed.id, StepStatus::Dispatched)
        .await
        .expect_err("terminal step");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn turn_transitions_fail_closed() {
    let Some((name, pool)) = prepare("turn_transition").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let (_run, turn, _step) = runtime_fixture(&store).await;
    let completed = store
        .transition_turn(&turn.id, TurnStatus::Completed)
        .await
        .expect("completed");
    let error = store
        .transition_turn(&completed.id, TurnStatus::Aborted)
        .await
        .expect_err("terminal turn");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    // Attempts can only be finished once.
    let retry_turn = store
        .create_turn(&turn.run_id, NewTurn::new("retry"))
        .await
        .expect("turn");
    let attempt_step = store
        .create_step(&retry_turn.id, NewStep::new(StepKind::ModelCall))
        .await
        .expect("step");
    let attempt = store
        .start_attempt(&attempt_step.id, quansio_core::Generation::INITIAL)
        .await
        .expect("attempt");
    store
        .finish_attempt(&attempt.id, AttemptStatus::Succeeded, None)
        .await
        .expect("finish");
    let error = store
        .finish_attempt(&attempt.id, AttemptStatus::Failed, None)
        .await
        .expect_err("double finish");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    drop_pool(&pool, &name).await;
}
