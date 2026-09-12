//! End-to-end scheduler dispatch against the canonical graph store (CORE-008).
//!
//! `crates/server` cannot depend on `quansio-graph` (the graph store depends on
//! `quansio-server` for `control::schema`, and the workspace conformance gate forbids the
//! package cycle), so the scheduler talks to Runs through its `RunDispatch` port. This
//! suite supplies the graph-backed implementation and proves that scheduler-driven work
//! moves through the canonical Run state machine and the canonical Objective/Run creation
//! path in the graph store — not through a second state machine.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use quansio_core::CanonicalId;
use quansio_events::EventStore;
use quansio_server::control::schema;
use quansio_server::scheduler::{
    DispatchOutcome, NewTimer, RoutineFireOutcome, RoutineFireRequest, RunDispatch,
    RunDispatchRequest, RunResumeRequest, Scheduler, SchedulerConfig, SchedulerError, TimerStore,
};
use serde_json::json;
use sqlx::PgPool;

use quansio_graph::{
    AgentKind, GraphError, GraphStore, NewAgentThread, NewRun, NewWorkNode, RunStatus,
    RunTriggerKind, WorkNodeKind, WorkOrigin,
};

mod common;
use common::{blocked_marker, drop_pool, prepare, TENANT, USER, WORKSPACE};

/// Graph-backed dispatch port: the only implementation that touches Run/WorkGraph state.
struct GraphDispatch {
    pool: PgPool,
}

impl GraphDispatch {
    fn store(&self) -> GraphStore {
        GraphStore::new(self.pool.clone(), TENANT).expect("graph store")
    }
}

fn failure(run_id: &str, error: GraphError) -> SchedulerError {
    SchedulerError::RunDispatch {
        run_id: run_id.to_string(),
        reason: error.to_string(),
    }
}

#[async_trait]
impl RunDispatch for GraphDispatch {
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        let store = self.store();
        let run_id = run_id(&request.run_id)?;
        match store
            .transition_run(&run_id, RunStatus::Running, None)
            .await
        {
            Ok(_) => Ok(DispatchOutcome::Dispatched),
            Err(GraphError::IllegalTransition { .. }) => Ok(DispatchOutcome::NotDispatchable),
            Err(error) => Err(failure(&request.run_id, error)),
        }
    }

    async fn resume_timer_wait(
        &self,
        request: RunResumeRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        let store = self.store();
        let run_id = run_id(&request.run_id)?;
        let run = store
            .get_run(&run_id)
            .await
            .map_err(|error| failure(&request.run_id, error))?;
        if run.generation.get() != request.generation {
            return Ok(DispatchOutcome::Fenced);
        }
        if run.status == RunStatus::Running {
            return Ok(DispatchOutcome::AlreadyRunning);
        }
        if !run.status.is_waiting() {
            return Ok(DispatchOutcome::NotDispatchable);
        }
        store
            .transition_run(&run_id, RunStatus::Running, None)
            .await
            .map_err(|error| failure(&request.run_id, error))?;
        Ok(DispatchOutcome::Dispatched)
    }

    async fn fire_routine(
        &self,
        request: RoutineFireRequest,
    ) -> Result<RoutineFireOutcome, SchedulerError> {
        let store = self.store();
        let title = request
            .objective_template
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Routine objective")
            .to_string();
        let mut node = NewWorkNode::new(
            &request.workspace_id,
            WorkNodeKind::Objective,
            title,
            json!({ "kind": "routine", "id": request.routine_id }),
        );
        node.origin = WorkOrigin::Routine;
        let objective = store
            .create_node(node)
            .await
            .map_err(|error| failure(&request.routine_id, error))?;
        let thread = store
            .create_agent_thread(NewAgentThread::new(
                &request.workspace_id,
                AgentKind::Teammate,
            ))
            .await
            .map_err(|error| failure(&request.routine_id, error))?;
        let mut run = NewRun::new(
            &request.workspace_id,
            objective.id,
            thread.id,
            RunTriggerKind::Routine,
        );
        run.trigger_ref = Some(request.routine_id.clone());
        let run = store
            .create_run(run)
            .await
            .map_err(|error| failure(&request.routine_id, error))?;
        // Enter the scheduler's dispatch path through the canonical state machine.
        store
            .transition_run(&run.id, RunStatus::Queued, None)
            .await
            .map_err(|error| failure(&run.id.to_string(), error))?;
        Ok(RoutineFireOutcome {
            objective_id: objective.id.to_string(),
            run_id: run.id.to_string(),
        })
    }
}

fn run_id(value: &str) -> Result<CanonicalId, SchedulerError> {
    CanonicalId::parse_typed(value, quansio_core::Prefix::Run).map_err(|error| {
        SchedulerError::RunDispatch {
            run_id: value.to_string(),
            reason: error.to_string(),
        }
    })
}

fn scheduler(pool: PgPool) -> Scheduler {
    Scheduler::new(
        pool.clone(),
        TENANT,
        SchedulerConfig::new("graph-test"),
        Arc::new(GraphDispatch { pool }),
    )
    .expect("scheduler")
}

/// Create a Run through the graph store and move it to `status`.
async fn run_in_status(pool: &PgPool, status: RunStatus) -> CanonicalId {
    let store = GraphStore::new(pool.clone(), TENANT).expect("graph store");
    let node = store
        .create_node(NewWorkNode::new(
            WORKSPACE,
            WorkNodeKind::Task,
            "scheduled work",
            json!({ "kind": "user", "id": USER }),
        ))
        .await
        .expect("work node");
    let thread = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Teammate))
        .await
        .expect("agent thread");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE,
            node.id,
            thread.id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let path: &[RunStatus] = match status {
        RunStatus::Queued => &[RunStatus::Queued],
        RunStatus::WaitingTimer => &[
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::WaitingTimer,
        ],
        RunStatus::Running => &[RunStatus::Queued, RunStatus::Running],
        RunStatus::Created => &[],
        other => panic!("unsupported test status {other:?}"),
    };
    for step in path {
        store
            .transition_run(&run.id, *step, None)
            .await
            .expect("legal run transition");
    }
    run.id
}

async fn event_count(pool: &PgPool, event_type: &str) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = $2",
    )
    .bind(TENANT)
    .bind(event_type)
    .fetch_one(&mut *tx)
    .await
    .expect("count events");
    tx.commit().await.expect("commit");
    count
}

#[tokio::test]
async fn timer_fire_resumes_the_run_through_the_graph_state_machine() {
    let Some((name, pool)) = prepare("sched_graph_timer").await else {
        blocked_marker();
        return;
    };
    let run = run_in_status(&pool, RunStatus::WaitingTimer).await;
    let timers = TimerStore::new(pool.clone(), TENANT).expect("timer store");
    timers
        .schedule(NewTimer {
            run_id: run.to_string(),
            workspace_id: Some(WORKSPACE.to_string()),
            step_id: None,
            wait_key: "timer:graph".to_string(),
            due_at: Utc::now() - ChronoDuration::seconds(1),
            generation: quansio_core::Generation::INITIAL,
            expires_at: None,
        })
        .await
        .expect("schedule timer");

    let report = scheduler(pool.clone()).tick().await.expect("tick");
    assert_eq!(report.timers_fired, 1);
    assert_eq!(event_count(&pool, "run.resumed").await, 1);

    let store = GraphStore::new(pool.clone(), TENANT).expect("graph store");
    let resumed = store.get_run(&run).await.expect("run");
    assert_eq!(
        resumed.status,
        RunStatus::Running,
        "the timer must resume the run through the graph state machine"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn routine_firing_creates_its_objective_through_the_command_path() {
    let Some((name, pool)) = prepare("sched_graph_routine").await else {
        blocked_marker();
        return;
    };
    let routine_id = format!("rtn_{}", quansio_core::UlidGenerator::new().generate());
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO routines (id, tenant_id, workspace_id, owner_user_id, trigger, \
         objective_template, absence_policy, status, next_due_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'catch_up_once', 'active', $7)",
    )
    .bind(&routine_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(USER)
    .bind(json!({ "kind": "cron", "spec": "* * * * *", "timezone": "UTC" }))
    .bind(json!({ "title": "nightly digest" }))
    .bind(Utc::now() - ChronoDuration::minutes(2))
    .execute(&mut *tx)
    .await
    .expect("routine");
    tx.commit().await.expect("commit");

    let report = scheduler(pool.clone()).tick().await.expect("tick");
    assert_eq!(report.routines_fired, 1);
    assert_eq!(event_count(&pool, "routine.fired").await, 1);

    let store = GraphStore::new(pool.clone(), TENANT).expect("graph store");
    let nodes = store.list_nodes(WORKSPACE).await.expect("nodes");
    let objective = nodes
        .iter()
        .find(|node| node.origin == WorkOrigin::Routine)
        .expect("a Routine Objective was created through the graph store");
    assert_eq!(objective.kind, WorkNodeKind::Objective);
    assert_eq!(objective.title, "nightly digest");

    // The Run was created by the same firing and queued through the state machine.
    let runs = store.list_runs(WORKSPACE).await.expect("runs");
    let created = runs
        .iter()
        .find(|run| run.trigger_kind == RunTriggerKind::Routine)
        .expect("a Routine Run was created");
    assert_eq!(created.trigger_ref.as_deref(), Some(routine_id.as_str()));
    assert_eq!(created.work_node_id, objective.id);
    assert_eq!(created.status, RunStatus::Queued);

    // The next tick dispatches that queued Run to RUNNING.
    let report = scheduler(pool.clone()).tick().await.expect("dispatch tick");
    assert_eq!(report.runs_dispatched, 1);
    let dispatched = store.get_run(&created.id).await.expect("run");
    assert_eq!(dispatched.status, RunStatus::Running);

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn queued_run_is_dispatched_through_the_graph_state_machine() {
    let Some((name, pool)) = prepare("sched_graph_dispatch").await else {
        blocked_marker();
        return;
    };
    let run = run_in_status(&pool, RunStatus::Queued).await;
    let report = scheduler(pool.clone()).tick().await.expect("tick");
    assert_eq!(report.runs_dispatched, 1);
    let store = GraphStore::new(pool.clone(), TENANT).expect("graph store");
    assert_eq!(
        store.get_run(&run).await.expect("run").status,
        RunStatus::Running
    );
    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn an_idle_tenant_tick_commits_no_event() {
    let Some((name, pool)) = prepare("sched_graph_idle").await else {
        blocked_marker();
        return;
    };
    let report = scheduler(pool.clone()).tick().await.expect("tick");
    assert_eq!(report, Default::default());
    let store = EventStore::new(pool.clone());
    let events = store
        .read_events_after(TENANT, None, 10)
        .await
        .expect("events");
    assert!(events.is_empty());

    drop_pool(&pool, &name).await;
}
