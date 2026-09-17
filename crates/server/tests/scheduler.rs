//! Scheduler, durable timer, wait-registry and routine tests (CORE-008).
//!
//! These tests exercise real PostgreSQL boundaries: durable timers in `durable_timers`,
//! waits in `protocol_states.waits` and canonical events through the real
//! `quansio_events::EventStore`.
//!
//! Run transitions go through the scheduler's [`RunDispatch`] port. `crates/server`
//! cannot depend on `quansio-graph` (the graph store depends on `quansio-server` for
//! `control::schema`, and the workspace conformance gate forbids the package cycle), so
//! these tests use a recording port: they assert exactly which transitions the scheduler
//! requests, and `crates/graph/tests/scheduler_dispatch.rs` proves the graph-backed
//! implementation performs those transitions through the canonical Run state machine.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, SubsecRound, Utc};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::protocol_state::{Wait, WaitKind};
use quansio_server::scheduler::{
    AbsencePolicy, DispatchOutcome, DurableTimer, FireOutcome, NewTimer, RoutineFireOutcome,
    RoutineFireRequest, RunDispatch, RunDispatchRequest, RunResumeRequest, Scheduler,
    SchedulerConfig, SchedulerError, TimerStatus, TimerStore, WaitError, WaitRegistry,
};
use serde_json::json;
use sqlx::PgPool;

mod common;
use common::{
    admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, scratch_url, seed_tenant,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";

/// Records every transition the scheduler requests, so a test can assert the exact
/// dispatch contract without owning the Run state machine.
#[derive(Default)]
struct RecordingDispatch {
    queued: Mutex<Vec<RunDispatchRequest>>,
    resumed: Mutex<Vec<RunResumeRequest>>,
    routines: Mutex<Vec<RoutineFireRequest>>,
    /// Generation the recording run is currently in; a resume older than this is fenced.
    current_generation: Mutex<u64>,
}

impl RecordingDispatch {
    fn new(generation: u64) -> Self {
        Self {
            current_generation: Mutex::new(generation),
            ..Self::default()
        }
    }

    fn resumed(&self) -> Vec<RunResumeRequest> {
        self.resumed.lock().expect("resume mutex").clone()
    }

    fn queued(&self) -> Vec<RunDispatchRequest> {
        self.queued.lock().expect("queued mutex").clone()
    }

    fn routines(&self) -> Vec<RoutineFireRequest> {
        self.routines.lock().expect("routine mutex").clone()
    }
}

#[async_trait]
impl RunDispatch for RecordingDispatch {
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        self.queued.lock().expect("queued mutex").push(request);
        Ok(DispatchOutcome::Dispatched)
    }

    async fn resume_timer_wait(
        &self,
        request: RunResumeRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        let current = *self.current_generation.lock().expect("generation mutex");
        let outcome = if request.generation < current {
            DispatchOutcome::Fenced
        } else {
            DispatchOutcome::Dispatched
        };
        self.resumed.lock().expect("resume mutex").push(request);
        Ok(outcome)
    }

    async fn fire_routine(
        &self,
        request: RoutineFireRequest,
    ) -> Result<RoutineFireOutcome, SchedulerError> {
        let mut generator = UlidGenerator::new();
        let objective = CanonicalId::generate(Prefix::WorkNode, &mut generator).to_string();
        let run = CanonicalId::generate(Prefix::Run, &mut generator).to_string();
        self.routines.lock().expect("routine mutex").push(request);
        Ok(RoutineFireOutcome {
            objective_id: objective,
            run_id: run,
        })
    }
}

struct Fixture {
    name: String,
    url: String,
    pool: PgPool,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    Some(Fixture {
        url: scratch_url(&name),
        name,
        pool,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

fn scheduler(pool: PgPool, instance: &str, dispatch: Arc<RecordingDispatch>) -> Scheduler {
    Scheduler::new(
        pool,
        TENANT,
        SchedulerConfig::new(instance),
        dispatch as Arc<dyn RunDispatch>,
    )
    .expect("scheduler")
}

/// Seed an AgentThread and a Run directly, in the given state and generation.
///
/// Run state here is fixture data (CORE-006's protocol-state tests do the same); the Run
/// state machine itself is exercised through the graph store in
/// `crates/graph/tests/scheduler_dispatch.rs`.
async fn seed_run(pool: &PgPool, status: &str, generation: i64) -> String {
    let mut generator = UlidGenerator::new();
    let thread_id = CanonicalId::generate(Prefix::AgentThread, &mut generator).to_string();
    let run_id = CanonicalId::generate(Prefix::Run, &mut generator).to_string();
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(&thread_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    sqlx::query(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, \
         status, trigger_kind) VALUES ($1, $2, $3, $4, $5, $6, $7, 'manual')",
    )
    .bind(&run_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(WORK_NODE)
    .bind(&thread_id)
    .bind(generation)
    .bind(status)
    .execute(&mut *tx)
    .await
    .expect("run");
    tx.commit().await.expect("commit");
    run_id
}

async fn schedule_timer(
    pool: &PgPool,
    run_id: &str,
    wait_key: &str,
    due_at: DateTime<Utc>,
    generation: u64,
) -> DurableTimer {
    let timers = TimerStore::new(pool.clone(), TENANT).expect("timer store");
    timers
        .schedule(NewTimer {
            run_id: run_id.to_string(),
            workspace_id: Some(WORKSPACE.to_string()),
            step_id: None,
            wait_key: wait_key.to_string(),
            due_at,
            generation: quansio_core::Generation::new(generation).expect("generation"),
            expires_at: None,
        })
        .await
        .expect("schedule timer")
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

async fn timer_status(pool: &PgPool, timer_id: &str) -> TimerStatus {
    let timers = TimerStore::new(pool.clone(), TENANT).expect("timer store");
    timers.get(timer_id).await.expect("timer row").status
}

async fn waits_for(pool: &PgPool, run_id: &str) -> Vec<Wait> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let waits = WaitRegistry::pending(&mut tx, TENANT, run_id)
        .await
        .expect("pending waits");
    tx.commit().await.expect("commit");
    waits
}

#[tokio::test]
async fn due_timer_fires_once_clears_the_wait_and_emits_one_event() {
    let Some(fixture) = prepare("sched_timer").await else {
        blocked_marker();
        return;
    };
    let run = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    let timer = schedule_timer(
        &fixture.pool,
        &run,
        "timer:once",
        Utc::now() - ChronoDuration::seconds(1),
        1,
    )
    .await;
    assert_eq!(timer.status, TimerStatus::Scheduled);
    assert_eq!(waits_for(&fixture.pool, &run).await.len(), 1);

    let dispatch = Arc::new(RecordingDispatch::new(1));
    let report = scheduler(fixture.pool.clone(), "inst-a", Arc::clone(&dispatch))
        .tick()
        .await
        .expect("tick");
    assert_eq!(report.timers_fired, 1);
    assert_eq!(
        timer_status(&fixture.pool, &timer.id).await,
        TimerStatus::Fired
    );
    assert!(waits_for(&fixture.pool, &run).await.is_empty());
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 1);

    // The resume went to the graph port exactly once, with the fenced generation.
    let resumed = dispatch.resumed();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].run_id, run);
    assert_eq!(resumed[0].generation, 1);
    assert_eq!(resumed[0].wait_key, "timer:once");
    assert_eq!(resumed[0].timer_id, timer.id);

    // A second tick must not fire the same timer again.
    let second = scheduler(fixture.pool.clone(), "inst-a", Arc::clone(&dispatch))
        .tick()
        .await
        .expect("second tick");
    assert_eq!(second.timers_fired, 0);
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 1);
    assert_eq!(dispatch.resumed().len(), 1);

    finish(fixture).await;
}

#[tokio::test]
async fn durable_timer_survives_a_pool_drop_and_still_fires_at_its_due_time() {
    let Some(fixture) = prepare("sched_restart").await else {
        blocked_marker();
        return;
    };
    let run = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    let timer = schedule_timer(
        &fixture.pool,
        &run,
        "timer:restart",
        Utc::now() + ChronoDuration::milliseconds(400),
        1,
    )
    .await;

    // Simulate a process restart: drop the pool and reconnect to the same database.
    fixture.pool.close().await;
    let pool = PgPool::connect(&fixture.url).await.expect("reconnect");
    let dispatch = Arc::new(RecordingDispatch::new(1));
    let restarted = scheduler(pool.clone(), "inst-restart", Arc::clone(&dispatch));
    let early = restarted.tick().await.expect("early tick");
    assert_eq!(
        early.timers_fired, 0,
        "the due time must come from PostgreSQL"
    );

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let report = restarted.tick().await.expect("due tick");
    assert_eq!(report.timers_fired, 1);
    assert_eq!(event_count(&pool, "run.resumed").await, 1);
    assert_eq!(timer_status(&pool, &timer.id).await, TimerStatus::Fired);
    assert_eq!(dispatch.resumed().len(), 1);

    drop_pool(&pool, &fixture.name).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_concurrent_loops_fire_a_timer_exactly_once() {
    let Some(fixture) = prepare("sched_contend").await else {
        blocked_marker();
        return;
    };
    let run = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    schedule_timer(
        &fixture.pool,
        &run,
        "timer:contended",
        Utc::now() - ChronoDuration::seconds(1),
        1,
    )
    .await;

    // Two scheduler instances with separate pools, racing on the same due timer.
    let pool_a = PgPool::connect(&fixture.url).await.expect("pool a");
    let pool_b = PgPool::connect(&fixture.url).await.expect("pool b");
    let dispatch = Arc::new(RecordingDispatch::new(1));
    let scheduler_a = Arc::new(scheduler(pool_a.clone(), "inst-a", Arc::clone(&dispatch)));
    let scheduler_b = Arc::new(scheduler(pool_b.clone(), "inst-b", Arc::clone(&dispatch)));
    let task_a = tokio::spawn(async move { scheduler_a.tick().await });
    let task_b = tokio::spawn(async move { scheduler_b.tick().await });
    let report_a = task_a.await.expect("task a").expect("tick a");
    let report_b = task_b.await.expect("task b").expect("tick b");

    assert_eq!(
        report_a.timers_fired + report_b.timers_fired,
        1,
        "exactly one loop may fire the timer"
    );
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 1);
    assert_eq!(
        dispatch.resumed().len(),
        1,
        "exactly one wake was dispatched"
    );

    pool_a.close().await;
    pool_b.close().await;
    finish(fixture).await;
}

#[tokio::test]
async fn stale_generation_timer_is_fenced_and_never_wakes_the_run() {
    let Some(fixture) = prepare("sched_fence").await else {
        blocked_marker();
        return;
    };
    let run = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    let timer = schedule_timer(
        &fixture.pool,
        &run,
        "timer:fenced",
        Utc::now() - ChronoDuration::seconds(1),
        1,
    )
    .await;

    // A newer controller generation owns the run now; the timer is obsolete.
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE runs SET generation = 2 WHERE id = $1 AND tenant_id = $2")
        .bind(&run)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("bump generation");
    tx.commit().await.expect("commit");

    let dispatch = Arc::new(RecordingDispatch::new(2));
    let report = scheduler(fixture.pool.clone(), "inst-fence", Arc::clone(&dispatch))
        .tick()
        .await
        .expect("tick");
    assert_eq!(report.timers_fenced, 1);
    assert_eq!(report.timers_fired, 0);
    assert_eq!(
        timer_status(&fixture.pool, &timer.id).await,
        TimerStatus::Cancelled
    );
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 0);
    assert!(dispatch.resumed().is_empty());

    finish(fixture).await;
}

#[tokio::test]
async fn resolving_wait_a_cannot_resume_run_b() {
    let Some(fixture) = prepare("sched_wait").await else {
        blocked_marker();
        return;
    };
    let run_a = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    let run_b = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    schedule_timer(
        &fixture.pool,
        &run_a,
        "timer:a",
        Utc::now() + ChronoDuration::hours(1),
        1,
    )
    .await;

    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let wrong_run = WaitRegistry::resolve(&mut tx, TENANT, &run_b, WaitKind::Timer, "timer:a")
        .await
        .expect_err("a wait registered by run A must not resolve run B");
    assert!(matches!(wrong_run, WaitError::NotRegistered { .. }));
    let wrong_key = WaitRegistry::resolve(
        &mut tx,
        TENANT,
        &run_a,
        WaitKind::Timer,
        "timer:not-registered",
    )
    .await
    .expect_err("an unknown key must not resolve");
    assert!(matches!(wrong_key, WaitError::NotRegistered { .. }));
    tx.commit().await.expect("commit");

    // Run B was never touched; run A still holds its wait.
    assert!(waits_for(&fixture.pool, &run_b).await.is_empty());
    assert_eq!(waits_for(&fixture.pool, &run_a).await.len(), 1);

    // The matching resolution succeeds and clears exactly that wait.
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let resolved = WaitRegistry::resolve(&mut tx, TENANT, &run_a, WaitKind::Timer, "timer:a")
        .await
        .expect("matching resolution");
    assert_eq!(resolved.key, "timer:a");
    tx.commit().await.expect("commit");
    assert!(waits_for(&fixture.pool, &run_a).await.is_empty());

    finish(fixture).await;
}

async fn seed_routine(pool: &PgPool, policy: AbsencePolicy, next_due_at: DateTime<Utc>) -> String {
    let id = format!("rtn_{}", UlidGenerator::new().generate());
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO routines (id, tenant_id, workspace_id, owner_user_id, trigger, \
         objective_template, absence_policy, status, next_due_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'active', $8)",
    )
    .bind(&id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(USER)
    .bind(json!({ "kind": "cron", "spec": "* * * * *", "timezone": "UTC" }))
    .bind(json!({ "title": format!("routine {id}") }))
    .bind(policy.as_db_str())
    .bind(next_due_at)
    .execute(&mut *tx)
    .await
    .expect("routine");
    tx.commit().await.expect("commit");
    id
}

async fn routine_next_due(pool: &PgPool, routine_id: &str) -> Option<DateTime<Utc>> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let next: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT next_due_at FROM routines WHERE id = $1 AND tenant_id = $2")
            .bind(routine_id)
            .bind(TENANT)
            .fetch_one(&mut *tx)
            .await
            .expect("next due");
    tx.commit().await.expect("commit");
    next
}

#[tokio::test]
async fn absence_policy_applies_skip_queue_and_catch_up_once() {
    let Some(fixture) = prepare("sched_routine").await else {
        blocked_marker();
        return;
    };
    let now = Utc::now();
    // Truncated to microseconds before it is used at all: `seed_routine` persists
    // this same value into a TIMESTAMPTZ column (Postgres's own precision), and
    // `tick()`'s production code correctly hands back exactly what was stored --
    // the DB round-trip, not tick(), decides the value `fire_window` carries. An
    // un-truncated `missed` here only ever matched that round-tripped value by
    // coincidence of this host's own clock resolution (see RUN-006's identical fix
    // in crates/server/src/policy/store.rs for the full mechanism).
    let missed = (now - ChronoDuration::minutes(3)).trunc_subsecs(6);
    let skip = seed_routine(&fixture.pool, AbsencePolicy::Skip, missed).await;
    let queue = seed_routine(&fixture.pool, AbsencePolicy::Queue, missed).await;
    let catch_up = seed_routine(&fixture.pool, AbsencePolicy::CatchUpOnce, missed).await;

    let dispatch = Arc::new(RecordingDispatch::new(1));
    let report = scheduler(fixture.pool.clone(), "inst-routines", Arc::clone(&dispatch))
        .tick()
        .await
        .expect("tick");
    assert_eq!(report.routines_skipped, 1);
    assert_eq!(report.routines_fired, 2);
    assert_eq!(report.routines_queued, 1);

    // `skip`: no firing, and the next window is in the future.
    assert!(routine_next_due(&fixture.pool, &skip).await.expect("next") > now);
    // `queue`: one window fired, the next missed window stays due so the backlog drains.
    let queued_next = routine_next_due(&fixture.pool, &queue).await.expect("next");
    assert!(queued_next <= now);
    // `catch_up_once`: one catch-up firing, then the next window is in the future.
    assert!(
        routine_next_due(&fixture.pool, &catch_up)
            .await
            .expect("next")
            > now
    );

    let fired = dispatch.routines();
    assert_eq!(fired.len(), 2);
    assert!(fired.iter().all(|request| request.fire_window == missed));
    assert!(fired.iter().all(|request| !request.fire_key.is_empty()));

    // The queued backlog drains on the next tick without collapsing into one firing.
    let second = scheduler(fixture.pool.clone(), "inst-routines", Arc::clone(&dispatch))
        .tick()
        .await
        .expect("second tick");
    assert_eq!(second.routines_queued, 1);
    assert_eq!(dispatch.routines().len(), 3);
    assert_eq!(event_count(&fixture.pool, "routine.skipped").await, 1);
    assert_eq!(event_count(&fixture.pool, "routine.fired").await, 3);

    finish(fixture).await;
}

#[tokio::test]
async fn saturated_due_queue_returns_a_typed_error_and_drops_nothing() {
    let Some(fixture) = prepare("sched_backpressure").await else {
        blocked_marker();
        return;
    };
    let due = Utc::now() - ChronoDuration::seconds(1);
    let mut timers = Vec::new();
    for index in 0..3 {
        let run = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
        timers
            .push(schedule_timer(&fixture.pool, &run, &format!("timer:bp-{index}"), due, 1).await);
    }

    let dispatch = Arc::new(RecordingDispatch::new(1));
    let mut config = SchedulerConfig::new("inst-bp");
    config.due_queue_capacity = 2;
    let constrained = Scheduler::new(
        fixture.pool.clone(),
        TENANT,
        config,
        Arc::clone(&dispatch) as Arc<dyn RunDispatch>,
    )
    .expect("scheduler");
    let error = constrained.tick().await.expect_err("saturated");
    match error {
        SchedulerError::QueueSaturated { pending, capacity } => {
            assert_eq!(pending, 3);
            assert_eq!(capacity, 2);
        }
        other => panic!("expected QueueSaturated, got {other:?}"),
    }

    // Nothing was dropped and nothing fired.
    for timer in &timers {
        assert_eq!(
            timer_status(&fixture.pool, &timer.id).await,
            TimerStatus::Scheduled
        );
    }
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 0);
    assert!(dispatch.resumed().is_empty());

    let report = scheduler(fixture.pool.clone(), "inst-bp", Arc::clone(&dispatch))
        .tick()
        .await
        .expect("unconstrained tick");
    assert_eq!(report.timers_fired, 3);
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 3);

    finish(fixture).await;
}

#[tokio::test]
async fn queued_run_is_offered_to_the_graph_dispatch_port() {
    let Some(fixture) = prepare("sched_dispatch").await else {
        blocked_marker();
        return;
    };
    let run = seed_run(&fixture.pool, "QUEUED", 1).await;
    let dispatch = Arc::new(RecordingDispatch::new(1));
    let report = scheduler(fixture.pool.clone(), "inst-dispatch", Arc::clone(&dispatch))
        .tick()
        .await
        .expect("tick");
    assert_eq!(report.runs_dispatched, 1);
    let queued = dispatch.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].run_id, run);
    assert_eq!(queued[0].generation, 1);

    finish(fixture).await;
}

#[tokio::test]
async fn fire_reports_a_lost_claim_without_emitting() {
    let Some(fixture) = prepare("sched_claim").await else {
        blocked_marker();
        return;
    };
    let run = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    let timer = schedule_timer(
        &fixture.pool,
        &run,
        "timer:claim",
        Utc::now() - ChronoDuration::seconds(1),
        1,
    )
    .await;
    let timers = TimerStore::new(fixture.pool.clone(), TENANT).expect("timer store");
    let claimed = timers.claim_due("owner-a", 30.0, 10).await.expect("claim");
    assert_eq!(claimed.len(), 1);

    // A forged/stale claim owner matches no row, so no event is committed.
    let mut forged = claimed[0].clone();
    forged.claim_owner = "owner-b".to_string();
    assert_eq!(
        timers.fire(&forged).await.expect("forged fire"),
        FireOutcome::ClaimLost
    );
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 0);

    let outcome = timers.fire(&claimed[0]).await.expect("fire");
    assert!(matches!(outcome, FireOutcome::Fired { .. }));
    assert_eq!(
        timer_status(&fixture.pool, &timer.id).await,
        TimerStatus::Fired
    );
    assert_eq!(event_count(&fixture.pool, "run.resumed").await, 1);

    finish(fixture).await;
}

#[tokio::test]
async fn scheduled_timer_registers_a_wait_and_emits_run_waiting() {
    let Some(fixture) = prepare("sched_waiting").await else {
        blocked_marker();
        return;
    };
    let run = seed_run(&fixture.pool, "WAITING_TIMER", 1).await;
    let timer = schedule_timer(
        &fixture.pool,
        &run,
        "timer:waiting",
        Utc::now() + ChronoDuration::hours(1),
        1,
    )
    .await;
    let waits = waits_for(&fixture.pool, &run).await;
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].kind, WaitKind::Timer);
    assert_eq!(waits[0].key, "timer:waiting");
    assert_eq!(event_count(&fixture.pool, "run.waiting").await, 1);

    // Cancelling the timer clears the wait it registered.
    let timers = TimerStore::new(fixture.pool.clone(), TENANT).expect("timer store");
    assert!(timers.cancel(&timer.id, "test").await.expect("cancel"));
    assert_eq!(
        timer_status(&fixture.pool, &timer.id).await,
        TimerStatus::Cancelled
    );
    assert!(waits_for(&fixture.pool, &run).await.is_empty());

    finish(fixture).await;
}
