//! RUN-010 acceptance: runtime budgets, quotas and capacity control.
//!
//! The suites drive the shipped `BudgetService` over real `budgets` rows and real `usage.*` events,
//! and put its capacity number through RUN-004's real orchestration gate — the same number the
//! orchestrator enforces.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`.

use quansio_core::{CorrelationId, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::budgets::{
    BudgetError, BudgetLimits, BudgetService, BudgetSpec, Meter, MeterUsage,
};
use quansio_server::runtime::orchestration::{
    capacity::CapacityGate, select, DependencyEdge, GraphSnapshot, RunRef, WorkNodeView,
};
use quansio_server::runtime::state_machine::{
    NewRun, RunTriggerKind, RuntimeEngine, RuntimeIdentity,
};
use serde_json::json;
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0RR010";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0RR010";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0RR010";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0RR010";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0RR010";

struct Fixture {
    name: String,
    pool: PgPool,
    budgets: BudgetService,
    /// A real run the run-scoped budgets point at.
    run_id: String,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let mut generator = UlidGenerator::new();
    let identity = RuntimeIdentity::system(
        TENANT,
        "budgets-test",
        CorrelationId::generate(&mut generator),
    );
    let budgets = BudgetService::new(pool.clone(), identity.clone()).expect("budget service");
    let engine = RuntimeEngine::new(pool.clone(), identity).expect("engine");
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
    let run = engine
        .create_run(NewRun::new(
            WORKSPACE,
            quansio_core::CanonicalId::parse_typed(WORK_NODE, quansio_core::Prefix::WorkNode)
                .expect("node"),
            quansio_core::CanonicalId::parse_typed(AGENT, quansio_core::Prefix::AgentThread)
                .expect("agent"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    Some(Fixture {
        name,
        pool,
        budgets,
        run_id: run.id.to_string(),
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

fn limits(pairs: &[(Meter, u64)]) -> BudgetLimits {
    let mut set = BudgetLimits::new();
    for (meter, value) in pairs {
        set.set(*meter, *value);
    }
    set
}

fn charges(pairs: &[(Meter, u64)]) -> MeterUsage {
    let mut set = MeterUsage::new();
    for (meter, value) in pairs {
        set.set(*meter, *value);
    }
    set
}

async fn grant(fixture: &Fixture, id: &str, spec: BudgetSpec) -> Result<(), BudgetError> {
    fixture.budgets.create(id, spec).await.map(|_| ())
}

/// A workspace budget with a tokens and concurrency limit.
fn workspace_spec() -> BudgetSpec {
    BudgetSpec {
        workspace_id: Some(WORKSPACE.to_string()),
        scope: "workspace".to_string(),
        scope_ref: Some(WORKSPACE.to_string()),
        limits: limits(&[(Meter::Tokens, 1_000), (Meter::Concurrency, 4)]),
        parent_budget_id: None,
    }
}

fn run_spec(parent: &str, tokens: u64, run_id: &str) -> BudgetSpec {
    BudgetSpec {
        workspace_id: Some(WORKSPACE.to_string()),
        scope: "run".to_string(),
        scope_ref: Some(run_id.to_string()),
        limits: limits(&[(Meter::Tokens, tokens)]),
        parent_budget_id: Some(parent.to_string()),
    }
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

/// Insert an effect the way a dispatch that is still in flight would leave it.
async fn active_effect(fixture: &Fixture, status: &str) -> String {
    let id = format!("eff_01J8Z3K6F1N8VQ2X5W9Y0R{:03}", 10);
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    sqlx::query(
        "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, effect_class, tier, \
         resource, params_digest, idempotency_key, capability_projection_id, status, target_kind, \
         target_id, generation) \
         VALUES ($1, $2, $3, $4, 'message.send', 3, $5, $6, $7, $8, $9, 'server', 'test', 1)",
    )
    .bind(&id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(&fixture.run_id)
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

async fn effect_row(fixture: &Fixture, effect_id: &str) -> (String, Option<String>) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, reconciliation FROM effect_records WHERE id = $1 AND tenant_id = $2",
    )
    .bind(effect_id)
    .bind(TENANT)
    .fetch_one(&mut *tx)
    .await
    .expect("effect row");
    tx.commit().await.expect("commit");
    row
}

// ---------------------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn child_budgets_cannot_exceed_parent_remaining() {
    let Some(fixture) = prepare("run010_inheritance").await else {
        blocked_marker();
        return;
    };
    grant(&fixture, "bdg_ws", workspace_spec())
        .await
        .expect("workspace budget");

    // A child within the parent's remaining allowance is granted.
    grant(
        &fixture,
        "bdg_run",
        run_spec("bdg_ws", 600, &fixture.run_id),
    )
    .await
    .expect("600 of 1000 is allowed");
    assert_eq!(
        fixture
            .budgets
            .parent_remaining("bdg_ws")
            .await
            .expect("remaining")
            .get(Meter::Tokens),
        1_000
    );

    // A child beyond it is refused, naming the meter, the request and what is left.
    let error = grant(
        &fixture,
        "bdg_greedy",
        run_spec("bdg_ws", 2_000, &fixture.run_id),
    )
    .await
    .expect_err("2000 > 1000 remaining");
    assert!(matches!(
        error,
        BudgetError::ExceedsParent {
            meter: Meter::Tokens,
            requested: 2_000,
            remaining: 1_000,
            ..
        }
    ));
    assert_eq!(error.code(), "VALIDATION_BOUNDS");

    // Consuming the parent shrinks what a later child may ask for: remaining, not the original limit.
    fixture
        .budgets
        .charge("bdg_ws", &charges(&[(Meter::Tokens, 500)]))
        .await
        .expect("charge");
    grant(
        &fixture,
        "bdg_run2",
        run_spec("bdg_ws", 500, &fixture.run_id),
    )
    .await
    .expect("500 of the 500 remaining is allowed");
    let error = grant(
        &fixture,
        "bdg_run3",
        run_spec("bdg_ws", 600, &fixture.run_id),
    )
    .await
    .expect_err("600 > 500 remaining");
    assert!(matches!(
        error,
        BudgetError::ExceedsParent { remaining: 500, .. }
    ));

    // Nesting holds through the chain: a grandchild is bounded by its child parent, not the root.
    grant(
        &fixture,
        "bdg_thread",
        run_spec("bdg_run2", 400, &fixture.run_id),
    )
    .await
    .expect("400 <= 500 remaining");
    let error = grant(
        &fixture,
        "bdg_thread2",
        run_spec("bdg_run2", 501, &fixture.run_id),
    )
    .await
    .expect_err("501 > 500 remaining");
    assert!(matches!(
        error,
        BudgetError::ExceedsParent { remaining: 500, .. }
    ));
    finish(fixture).await;
}

#[tokio::test]
async fn a_budget_capacity_limit_bounds_admission_and_exhaustion_stops_it() {
    let Some(fixture) = prepare("run010_capacity").await else {
        blocked_marker();
        return;
    };
    let mut spec = workspace_spec();
    spec.limits = limits(&[(Meter::Concurrency, 2), (Meter::ToolCalls, 1)]);
    grant(&fixture, "bdg_cap", spec).await.expect("budget");

    // The number the budget sets is the number RUN-004's gate enforces.
    let limit = fixture
        .budgets
        .capacity_limit("bdg_cap")
        .await
        .expect("limit")
        .expect("a concurrency limit is set");
    assert_eq!(limit, 2);
    let gate = CapacityGate::new(limit);

    // Two runs are already executing, so a third ready node is held back by the real gate.
    let snapshot = GraphSnapshot {
        workspace_id: WORKSPACE.to_string(),
        revision: 1,
        nodes: vec![
            running_node("wn_a", "run_a"),
            running_node("wn_b", "run_b"),
            ready_node("wn_c", "run_c"),
        ],
        edges: Vec::<DependencyEdge>::new(),
    };
    assert_eq!(gate.headroom(&snapshot), 0, "the budget's limit is reached");
    let selection = select(&snapshot, gate.headroom(&snapshot));
    assert!(
        selection.is_empty(),
        "no admission while the limit is reached: {selection:?}"
    );

    // Exhausting a different meter stops admission even with every slot free.
    let error = fixture
        .budgets
        .charge("bdg_cap", &charges(&[(Meter::ToolCalls, 2)]))
        .await
        .expect_err("2 > 1 tool-call limit");
    assert_eq!(error.code(), "BUDGET_EXHAUSTED");
    assert_eq!(
        fixture
            .budgets
            .capacity_limit("bdg_cap")
            .await
            .expect("limit"),
        Some(0),
        "an exhausted budget admits no new work"
    );
    let free = GraphSnapshot {
        workspace_id: WORKSPACE.to_string(),
        revision: 1,
        nodes: vec![ready_node("wn_a", "run_a")],
        edges: Vec::<DependencyEdge>::new(),
    };
    let exhausted_gate = CapacityGate::new(0);
    assert!(
        select(&free, exhausted_gate.headroom(&free)).is_empty(),
        "exhaustion stops new work even with capacity free"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn budget_exhaustion_stops_new_work_without_corrupting_active_effects() {
    let Some(fixture) = prepare("run010_graceful").await else {
        blocked_marker();
        return;
    };
    let mut spec = run_spec_without_parent(&fixture.run_id);
    spec.limits = limits(&[(Meter::ToolCalls, 3)]);
    grant(&fixture, "bdg_run", spec).await.expect("run budget");

    // An effect is in flight when the budget runs out.
    let effect_id = active_effect(&fixture, "DISPATCHED").await;
    let before = effect_row(&fixture, &effect_id).await;
    let effect_events_before = events_of_type(&fixture.pool, "effect.settled_success").await;

    // Consuming past the limit is refused, and the budget reports itself exhausted.
    let error = fixture
        .budgets
        .charge("bdg_run", &charges(&[(Meter::ToolCalls, 4)]))
        .await
        .expect_err("4 > 3 tool-call limit");
    assert_eq!(error.code(), "BUDGET_EXHAUSTED");
    let record = fixture.budgets.load("bdg_run").await.expect("budget");
    assert_eq!(record.status, "exhausted");
    assert!(!record.is_active());
    assert!(
        record.exhausted_meters().is_empty(),
        "a refused charge consumes nothing, so no meter has run down: the status carries the refusal"
    );

    // The exhaustion is reported on the usage stream (DOMAIN.md §13.2, consumed by OPS-004).
    assert_eq!(events_of_type(&fixture.pool, "usage.exhausted").await, 1);
    assert_eq!(
        events_of_type(&fixture.pool, "usage.recorded").await,
        0,
        "a refused charge records no usage"
    );

    // Nothing in flight was touched: the effect keeps its state and no settlement happened.
    assert_eq!(
        effect_row(&fixture, &effect_id).await,
        before,
        "an active effect is untouched by budget exhaustion"
    );
    assert_eq!(
        events_of_type(&fixture.pool, "effect.settled_success").await,
        effect_events_before,
        "budget exhaustion settles nothing"
    );

    // A charge inside the limit still records usage and leaves the budget active.
    grant(
        &fixture,
        "bdg_run2",
        run_spec("bdg_ws", 100, &fixture.run_id),
    )
    .await
    .expect_err("the parent budget does not exist");
    let mut within = run_spec_without_parent(&fixture.run_id);
    within.limits = limits(&[(Meter::ToolCalls, 3)]);
    grant(&fixture, "bdg_run3", within).await.expect("budget");
    let outcome = fixture
        .budgets
        .charge("bdg_run3", &charges(&[(Meter::ToolCalls, 3)]))
        .await
        .expect("3 of 3 fits exactly");
    assert!(outcome.exhausted, "reaching the limit exhausts the budget");
    assert!(
        !outcome.may_continue(),
        "an exhausted budget funds no new work"
    );
    assert_eq!(
        fixture
            .budgets
            .load("bdg_run3")
            .await
            .expect("budget")
            .exhausted_meters(),
        vec![Meter::ToolCalls],
        "the meter that ran down is named"
    );
    assert_eq!(events_of_type(&fixture.pool, "usage.recorded").await, 1);
    finish(fixture).await;
}

fn run_spec_without_parent(run_id: &str) -> BudgetSpec {
    BudgetSpec {
        workspace_id: Some(WORKSPACE.to_string()),
        scope: "run".to_string(),
        scope_ref: Some(run_id.to_string()),
        limits: BudgetLimits::new(),
        parent_budget_id: None,
    }
}

fn running_node(id: &str, run_id: &str) -> WorkNodeView {
    WorkNodeView {
        id: id.to_string(),
        kind: "task".to_string(),
        status: "in_progress".to_string(),
        parent_id: None,
        priority: 1,
        revision: 1,
        run: Some(RunRef {
            run_id: run_id.to_string(),
            generation: 1,
            status: "RUNNING".to_string(),
        }),
    }
}

fn ready_node(id: &str, run_id: &str) -> WorkNodeView {
    WorkNodeView {
        id: id.to_string(),
        kind: "task".to_string(),
        status: "ready".to_string(),
        parent_id: None,
        priority: 1,
        revision: 1,
        run: Some(RunRef {
            run_id: run_id.to_string(),
            generation: 1,
            status: "QUEUED".to_string(),
        }),
    }
}
