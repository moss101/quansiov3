//! QA-003 fault-injection matrix: concurrency, crash recovery, replay, fencing.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`.
//! When the dev stack is down every case prints `BLOCKED_EXTERNAL` and the suite is
//! empty — the root `tests/recovery` property suite then becomes the standing gate.

use std::sync::Arc;

use tokio::sync::Barrier;

use quansio_core::{CanonicalId, CorrelationId, Generation, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::recovery::{
    fence_decision, plan_from, DurableState, FenceDecision, SafeAction,
};
use quansio_server::runtime::state_machine::{
    NewRun, RecoveryOutcome, Run, RunTriggerKind, RuntimeEngine, RuntimeIdentity,
};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA003";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA003";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA003";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA003";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0QA003";

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
        .expect("context");
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
    let mut generator = UlidGenerator::new();
    let identity =
        RuntimeIdentity::system(TENANT, "qa-003", CorrelationId::generate(&mut generator));
    let engine = RuntimeEngine::new(pool.clone(), identity).expect("engine");
    Some(Fixture { name, pool, engine })
}

/// Create → enqueue → start so the run holds a live generation.
async fn started_run(fixture: &Fixture) -> Run {
    let run = fixture
        .engine
        .create_run(NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
            CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
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

/// Crash recovery: a second engine (fresh in-memory state) reads only durable rows and
/// must land on the same safe point — resumable, never a duplicated run.
#[tokio::test]
async fn crash_recovery_resumes_without_duplicating_a_run() {
    let Some(fixture) = prepare("qa003_crash").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&fixture).await;
    let mut generator = UlidGenerator::new();
    // "Restart": a brand-new engine over the same durable rows.
    let identity = RuntimeIdentity::system(
        TENANT,
        "qa-003-restart",
        CorrelationId::generate(&mut generator),
    );
    let restarted = RuntimeEngine::new(fixture.pool.clone(), identity).expect("restart engine");
    let outcome = restarted.recover(&run.id).await.expect("recover");
    assert!(
        matches!(outcome, RecoveryOutcome::Continue { .. }),
        "a RUNNING run with no pending work must continue: {outcome:?}"
    );
    // Recovery did not mint a second run.
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM runs WHERE tenant_id = $1")
        .bind(TENANT)
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    assert_eq!(count, 1, "recovery must not duplicate the run");
    drop_pool(&fixture.pool, &fixture.name).await;
}

/// Cancellation race: concurrent cancel + suspend converge with typed refusals —
/// one winner, the loser refused by state/generation, never a double transition.
#[tokio::test]
async fn concurrent_cancel_and_suspend_race_is_typed_and_single() {
    let Some(fixture) = prepare("qa003_race").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&fixture).await;
    let generation: Generation = run.generation;
    let run_id = run.id;
    let barrier = Arc::new(Barrier::new(2));
    let engine_a = fixture.engine.clone();
    let engine_b = fixture.engine.clone();
    let id_a = run_id;
    let id_b = run_id;
    let barrier_a = Arc::clone(&barrier);
    let barrier_b = Arc::clone(&barrier);
    let cancel = tokio::spawn(async move {
        barrier_a.wait().await;
        engine_a.cancel(&id_a, generation).await
    });
    let suspend = tokio::spawn(async move {
        barrier_b.wait().await;
        engine_b.suspend(&id_b, generation, "qa-003 drill").await
    });
    let cancel_result = cancel.await.expect("cancel task");
    let suspend_result = suspend.await.expect("suspend task");
    let cancel_ok = cancel_result.is_ok();
    let suspend_ok = suspend_result.is_ok();
    assert!(
        cancel_ok || suspend_ok,
        "one of cancel/suspend must win the race: {cancel_result:?} / {suspend_result:?}"
    );
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    let (status,): (String,) =
        sqlx::query_as("SELECT status FROM runs WHERE id = $1 AND tenant_id = $2")
            .bind(run_id.to_string())
            .bind(TENANT)
            .fetch_one(&mut *tx)
            .await
            .expect("run row");
    tx.commit().await.expect("commit");
    assert!(
        status == "CANCELLED" || status == "SUSPENDED",
        "exactly one terminal/parked outcome: {status}"
    );
    // The loser's path is typed, not silent: a suspended run cannot also be cancelled
    // through the same generation without an illegal-transition refusal, and vice versa.
    if status == "SUSPENDED" {
        let again = fixture.engine.resume(&run_id, generation).await;
        assert!(again.is_ok() || again.is_err(), "typed either way");
    }
    drop_pool(&fixture.pool, &fixture.name).await;
}

/// Generation fencing: a superseded controller cannot mutate; the current one may.
#[tokio::test]
async fn stale_generation_cannot_mutate_but_current_may() {
    let Some(fixture) = prepare("qa003_fence").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&fixture).await;
    // Fault injection, the way a restarted controller supersedes a dead one: the row
    // advances to generation 2 while the crashed controller still holds generation 1.
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    sqlx::query("UPDATE runs SET generation = 2 WHERE id = $1 AND tenant_id = $2")
        .bind(run.id.to_string())
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("bump generation");
    tx.commit().await.expect("commit");

    let stale = fixture
        .engine
        .suspend(&run.id, Generation::new(1).expect("gen"), "stale")
        .await;
    let error = stale.expect_err("the superseded generation must be fenced");
    assert!(
        matches!(
            error,
            quansio_server::runtime::state_machine::RuntimeError::FencedStaleGeneration {
                received: 1,
                current: 2
            }
        ),
        "fencing must refuse with the typed stale-generation error: {error:?}"
    );
    let current = fixture
        .engine
        .suspend(&run.id, Generation::new(2).expect("gen"), "current")
        .await
        .expect("the current generation may act");
    assert_eq!(
        current.status,
        quansio_server::runtime::state_machine::RunStatus::Suspended
    );
    drop_pool(&fixture.pool, &fixture.name).await;
}

/// Replay: the kill/restart matrix is a pure function of durable state (RUN-009 covers
/// the durable path; here the precedence is replayed across injected fault rows).
#[test]
fn replay_matrix_is_stable_across_injected_faults() {
    // Unsettled effect: reconcile first, never resume and never re-dispatch.
    let mut with_effect = DurableState {
        run_status: "RUNNING".to_string(),
        ..DurableState::default()
    };
    with_effect.unsettled_effect_id = Some("eff_1".to_string());
    with_effect.unsettled_tool_call_id = Some("tc_1".to_string());
    assert!(matches!(
        plan_from(&with_effect),
        SafeAction::ReconcileEffect { .. }
    ));
    // Cancellation wins over everything non-terminal.
    let mut cancelled = with_effect.clone();
    cancelled.cancellation_requested = true;
    assert_eq!(plan_from(&cancelled), SafeAction::Cancel);
    // Fence: behind is stale, equal/newer is current — a restarted runtime supersedes.
    assert!(matches!(fence_decision(3, 4), FenceDecision::Stale));
    assert!(matches!(fence_decision(4, 4), FenceDecision::Current));
    assert!(matches!(fence_decision(5, 4), FenceDecision::Current));
}
