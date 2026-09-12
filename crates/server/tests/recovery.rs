//! RUN-009 acceptance: recovery, generation fencing and effect reconciliation.
//!
//! The suites drive the shipped `Recoverer` over real durable state: real `runs`, real
//! ProtocolState, real `attempts` and real EffectRecords. "Killing the runtime" is modelled the
//! way the repository models it elsewhere — a second recoverer instance reading only durable state
//! — so nothing here depends on in-process memory.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`.

use quansio_core::{CanonicalId, CorrelationId, Generation, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::effects::{EffectStatus, ReconciliationEvidence};
use quansio_server::runtime::protocol_state::ProtocolState;
use quansio_server::runtime::recovery::{
    plan_from, DurableState, Recoverer, RecoveryResolution, RECOVERY_FORBIDDEN_TABLES,
    RECOVERY_READ_TABLES,
};
use quansio_server::runtime::state_machine::{
    NewRun, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity,
};
use serde_json::json;
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0RR009";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0RR009";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0RR009";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0RR009";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0RR009";

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
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
    let mut generator = UlidGenerator::new();
    let identity = RuntimeIdentity::system(
        TENANT,
        "recovery-test",
        CorrelationId::generate(&mut generator),
    );
    let engine = RuntimeEngine::new(pool.clone(), identity.clone()).expect("engine");
    Some(Fixture {
        name,
        pool,
        identity,
        engine,
    })
}

fn recoverer(fixture: &Fixture) -> Recoverer {
    Recoverer::new(fixture.pool.clone(), fixture.identity.clone()).expect("recoverer")
}

/// Create and start a run so it holds a live generation.
async fn started_run(fixture: &Fixture) -> (CanonicalId, u64) {
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
    let run = fixture.engine.enqueue(&run.id, run.generation).await.expect("enqueue");
    let run = fixture.engine.start(&run.id, run.generation).await.expect("start");
    (run.id, run.generation.get())
}

/// Write a ProtocolState the way a crash would leave it.
async fn write_protocol_state(fixture: &Fixture, run_id: &CanonicalId, mutate: impl FnOnce(&mut ProtocolState)) {
    let mut state = fixture
        .engine
        .store()
        .load_protocol_state(run_id)
        .await
        .expect("load")
        .unwrap_or_else(|| ProtocolState::new(run_id.to_string(), 1));
    mutate(&mut state);
    fixture
        .engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("store");
}

/// Insert an effect the way a dispatch interrupted by a crash would leave it.
async fn unsettled_effect(fixture: &Fixture, run_id: &CanonicalId, status: &str) -> String {
    let mut generator = UlidGenerator::new();
    let id = CanonicalId::generate(Prefix::EffectRecord, &mut generator).to_string();
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, effect_class, tier, \
         resource, params_digest, idempotency_key, capability_projection_id, status, target_kind, \
         target_id, generation) \
         VALUES ($1, $2, $3, $4, 'message.send', 3, $5, $6, $7, $8, $9, 'server', 'test', 1)",
    )
    .bind(&id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(run_id.to_string())
    .bind(json!({"kind": "domain", "selector": "example.com"}))
    .bind("0".repeat(64))
    .bind(format!("key-{id}"))
    .bind(format!("cap_{id}"))
    .bind(status)
    .execute(&mut *tx)
    .await
    .expect("effect");
    tx.commit().await.expect("commit");
    id
}

async fn run_status(fixture: &Fixture, run_id: &CanonicalId) -> RunStatus {
    fixture.engine.store().load_run(run_id).await.expect("run").status
}

async fn events_of_type(pool: &PgPool, event_type: &str) -> usize {
    quansio_events::EventStore::new(pool.clone())
        .read_events_after(TENANT, None, 500)
        .await
        .expect("events")
        .into_iter()
        .filter(|event| event.event_type.to_string() == event_type)
        .count()
}

// ---------------------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn killed_runtime_resumes_to_the_same_safe_logical_point() {
    let Some(fixture) = prepare("run009_matrix").await else {
        blocked_marker();
        return;
    };
    // Four durable positions a crash can interrupt, each with its own expected safe point.
    let (plain, _) = started_run(&fixture).await;
    let (awaiting, _) = started_run(&fixture).await;
    write_protocol_state(&fixture, &awaiting, |state| {
        state.pending_approvals = vec!["apr_1".to_string()];
    })
    .await;
    fixture
        .engine
        .store()
        .transition_run(&awaiting, Generation::new(1).expect("generation"), RunStatus::WaitingApproval, None)
        .await
        .expect("park");
    let (cancelling, _) = started_run(&fixture).await;
    write_protocol_state(&fixture, &cancelling, |state| {
        state.cancellation_requested = true;
    })
    .await;

    let before_crash = recoverer(&fixture);
    let reports: Vec<_> = {
        let mut reports = Vec::new();
        for run in [&plain, &awaiting, &cancelling] {
            reports.push(before_crash.recover_run(run).await.expect("recover"));
        }
        reports
    };
    assert_eq!(
        reports[0].resolution,
        RecoveryResolution::Resumable,
        "a running run with no pending work resumes"
    );
    assert_eq!(
        reports[1].resolution,
        RecoveryResolution::Waiting {
            state: "WAITING_APPROVAL".to_string(),
            key: "apr_1".to_string()
        },
        "a parked run stays parked on its own key"
    );
    assert_eq!(
        reports[2].resolution,
        RecoveryResolution::Cancelled,
        "a cancellation requested before the crash is honoured"
    );
    assert_eq!(run_status(&fixture, &cancelling).await, RunStatus::Cancelled);
    assert_eq!(events_of_type(&fixture.pool, "run.cancelled").await, 1);

    // A restarted runtime — a different instance reading only durable state — lands on the very
    // same logical point for every run.
    let after_restart = recoverer(&fixture);
    for (run, expected) in [(&plain, &reports[0]), (&awaiting, &reports[1])] {
        let report = after_restart.recover_run(run).await.expect("recover");
        assert_eq!(
            report.resolution, expected.resolution,
            "recovery is a pure function of durable state"
        );
    }
    // The cancelled run is now terminal, and recovery does not cancel it a second time.
    let settled = after_restart.recover_run(&cancelling).await.expect("recover");
    assert_eq!(
        settled.resolution,
        RecoveryResolution::Terminal,
        "the applied cancellation is a terminal state, not a repeated action"
    );
    assert_eq!(
        events_of_type(&fixture.pool, "run.cancelled").await,
        1,
        "recovery never cancels twice"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn stale_worker_output_cannot_mutate_the_current_run() {
    let Some(fixture) = prepare("run009_stale").await else {
        blocked_marker();
        return;
    };
    let (run_id, generation) = started_run(&fixture).await;
    let recoverer = recoverer(&fixture);

    // The run advances to a new generation the way it does after a restart (a newer generation
    // supersedes the old one), which leaves the crashed worker behind.
    let advanced = generation + 1;
    {
        let mut tx = fixture.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT).await.expect("context");
        sqlx::query("UPDATE runs SET generation = $1 WHERE id = $2 AND tenant_id = $3")
            .bind(i64::try_from(advanced).unwrap_or(2))
            .bind(run_id.to_string())
            .bind(TENANT)
            .execute(&mut *tx)
            .await
            .expect("advance generation");
        tx.commit().await.expect("commit");
    }

    // Recovery fences the worker that is now behind.
    let error = recoverer
        .fence_worker(&run_id, generation)
        .await
        .expect_err("a behind generation is refused");
    assert_eq!(error.code(), "FENCED_STALE_GENERATION");
    recoverer
        .fence_worker(&run_id, advanced)
        .await
        .expect("the current generation may act");

    // The engine refuses the stale worker's mutation, so the run keeps its state.
    let before = run_status(&fixture, &run_id).await;
    let stale = fixture
        .engine
        .run_turn(
            &run_id,
            Generation::new(generation).expect("generation"),
            quansio_server::runtime::state_machine::TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect_err("the engine fences a behind generation");
    assert!(
        matches!(stale, RuntimeError::FencedStaleGeneration { .. }),
        "the refusal names the fence: {stale:?}"
    );
    assert_eq!(run_status(&fixture, &run_id).await, before, "state is untouched");

    // The recovery report carries the current generation, so a stale caller can be judged against it.
    let report = recoverer.recover_run(&run_id).await.expect("recover");
    assert_eq!(report.generation, advanced);
    assert_eq!(report.stale_attempts, 0, "no attempt was written by the stale worker");
    finish(fixture).await;
}

#[tokio::test]
async fn an_uncertain_effect_is_reconciled_before_the_run_resumes() {
    let Some(fixture) = prepare("run009_uncertain").await else {
        blocked_marker();
        return;
    };
    let (run_id, _) = started_run(&fixture).await;
    let effect_id = unsettled_effect(&fixture, &run_id, "OUTCOME_UNKNOWN").await;
    let recoverer = recoverer(&fixture);

    // Recovery refuses to resume: the external outcome is unknown, so it must be reconciled.
    let report = recoverer.recover_run(&run_id).await.expect("recover");
    let RecoveryResolution::ReconciliationRequired {
        effect_id: required,
        strategy,
    } = report.resolution.clone()
    else {
        panic!("an unknown outcome must be reconciled first: {report:?}");
    };
    assert_eq!(required, effect_id);
    assert!(!strategy.is_empty(), "the class's strategy is named: {strategy}");
    assert!(report.requires_reconciliation());

    // Applying evidence settles it without dispatching anything.
    let settled = recoverer
        .reconcile_effect(
            &effect_id,
            ReconciliationEvidence::Determined {
                landed: true,
                remote_ref: Some("provider://message/1".to_string()),
                evidence_ids: vec!["evd_recovery".to_string()],
            },
        )
        .await
        .expect("reconcile");
    assert_eq!(settled.status, EffectStatus::ReconciledSuccess);

    // Only now does the run resume, and nothing was dispatched a second time.
    let after = recoverer.recover_run(&run_id).await.expect("recover");
    assert_eq!(after.resolution, RecoveryResolution::Resumable);
    let effects: i64 = {
        let mut tx = fixture.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT).await.expect("context");
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM effect_records WHERE run_id = $1")
            .bind(run_id.to_string())
            .fetch_one(&mut *tx)
            .await
            .expect("count");
        tx.commit().await.expect("commit");
        count
    };
    assert_eq!(effects, 1, "recovery reconciled the record instead of retrying it");
    finish(fixture).await;
}

#[tokio::test]
async fn recovery_decisions_read_only_durable_state() {
    // The pure decision surface: every durable position maps to the safe action, and the durable
    // view carries no memory-shaped field.
    let mut waiting = DurableState {
        run_status: "WAITING_CHILD".to_string(),
        child_agent_threads: vec!["ath_child".to_string()],
        ..DurableState::default()
    };
    assert!(!plan_from(&waiting).mutates(), "a park changes nothing on its own");
    waiting.run_status = "RUNNING".to_string();
    assert!(plan_from(&waiting).mutates());

    // The module declares the only tables it may read, and none of them is a memory table.
    for forbidden in RECOVERY_FORBIDDEN_TABLES {
        assert!(
            !RECOVERY_READ_TABLES.contains(forbidden),
            "recovery must never read {forbidden}"
        );
    }
}

#[tokio::test]
async fn the_recovery_module_never_reads_a_memory_table() {
    // Structural gate for the build item: recovery reads ProtocolState, RuntimeEvents, Steps,
    // Attempts, Checkpoints, Evidence and the Effect Ledger — nothing semantic.
    let mut scanned = 0usize;
    let mut referenced: Vec<String> = Vec::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/recovery");
    for entry in std::fs::read_dir(&root).expect("recovery module directory") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&path).expect("source");
        for table in table_names_in(&source) {
            if !referenced.contains(&table) {
                referenced.push(table);
            }
        }
    }
    assert!(scanned >= 2, "the scan looked at the module's sources");
    assert!(
        referenced.iter().any(|table| table == "runs"),
        "the scan really found table reads: {referenced:?}"
    );
    for table in &referenced {
        assert!(
            RECOVERY_READ_TABLES.contains(&table.as_str()),
            "recovery reads {table}, which is not a durable recovery table"
        );
        assert!(
            !RECOVERY_FORBIDDEN_TABLES.contains(&table.as_str()),
            "recovery must never read {table}"
        );
    }
    assert_eq!(
        referenced.iter().filter(|table| table.as_str() == "protocol_states").count(),
        0,
        "ProtocolState is read through the store, not by raw SQL"
    );
}

/// Every table a SQL statement in `source` touches (`FROM x`, `JOIN x`, `INTO x`, `UPDATE x`).
///
/// Only string literals are scanned: SQL lives in literals, and prose in doc comments is not a
/// read of anything.
fn table_names_in(source: &str) -> Vec<String> {
    let mut tables = Vec::new();
    let mut literals: Vec<&str> = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        literals.push(&after[..end]);
        rest = &after[end + 1..];
    }
    for literal in literals {
        tables.extend(tables_in_literal(literal));
    }
    tables
}

fn tables_in_literal(literal: &str) -> Vec<String> {
    let mut tables = Vec::new();
    let words: Vec<&str> = literal.split_whitespace().collect();
    for (index, word) in words.iter().enumerate() {
        let keyword = word
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .to_ascii_uppercase();
        if matches!(keyword.as_str(), "FROM" | "JOIN" | "INTO" | "UPDATE") {
            if let Some(next) = words.get(index + 1) {
                let table = next
                    .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .to_ascii_lowercase();
                if !table.is_empty()
                    && table.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
                {
                    tables.push(table);
                }
            }
        }
    }
    tables
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}
