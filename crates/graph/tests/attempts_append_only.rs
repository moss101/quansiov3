//! Attempts are append-only: a retry creates a new row at the next seq (CORE-004).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, WORKSPACE};
use quansio_core::Generation;
use quansio_graph::{
    AgentKind, AttemptStatus, GraphStore, NewAgentThread, NewRun, NewStep, NewTurn, RunTriggerKind,
    StepKind, WorkNodeKind,
};

fn actor() -> serde_json::Value {
    serde_json::json!({"kind": "user", "id": "usr_seed"})
}

#[tokio::test]
async fn retries_append_attempts_and_never_overwrite_earlier_ones() {
    let Some((name, pool)) = prepare("attempts").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let node = store
        .create_node(quansio_graph::NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "attempts",
            actor(),
        ))
        .await
        .expect("create node");
    let thread = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("thread");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE,
            node.id,
            thread.id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let turn = store
        .create_turn(&run.id, NewTurn::new("manual"))
        .await
        .expect("turn");
    let step = store
        .create_step(&turn.id, NewStep::new(StepKind::ModelCall))
        .await
        .expect("step");

    let first = store
        .start_attempt(&step.id, Generation::INITIAL)
        .await
        .expect("first attempt");
    assert_eq!(first.seq, 1);
    assert_eq!(first.status, AttemptStatus::Started);
    let first = store
        .finish_attempt(
            &first.id,
            AttemptStatus::Failed,
            Some(serde_json::json!({"code": "TOOL_TIMEOUT"})),
        )
        .await
        .expect("finish first");

    // A retry appends a new attempt; the earlier row is not reused or overwritten.
    let second = store
        .start_attempt(&step.id, Generation::INITIAL.next())
        .await
        .expect("retry");
    assert_eq!(second.seq, 2);
    assert_ne!(first.id, second.id);
    let second = store
        .finish_attempt(&second.id, AttemptStatus::Succeeded, None)
        .await
        .expect("finish second");

    let attempts = store.list_attempts(&step.id).await.expect("attempts");
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    assert_eq!(attempts[0].id, first.id);
    assert_eq!(attempts[0].seq, 1);
    assert_eq!(attempts[0].status, AttemptStatus::Failed);
    assert_eq!(
        attempts[0]
            .error
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&serde_json::json!("TOOL_TIMEOUT")),
        "the finished attempt keeps its own outcome"
    );
    assert!(attempts[0].finished_at.is_some());
    assert_eq!(attempts[1].id, second.id);
    assert_eq!(attempts[1].seq, 2);
    assert_eq!(attempts[1].status, AttemptStatus::Succeeded);

    drop_pool(&pool, &name).await;
}
