//! Budget service refusals, unlimited meters and limit changes (QA-001 coverage of RUN-010).
//!
//! These suites complement `tests/budgets.rs` and `tests/budgets_enforced.rs`: they drive the
//! shipped `BudgetService` over real `budgets` rows and real `usage.*` events through the paths
//! those suites leave out — a malformed tenant, recording against an unknown budget, recording a
//! meter the budget does not limit, lowering limits below what was already consumed, and charging
//! through the `BudgetPort` trait object a composition root holds.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use quansio_core::{CorrelationId, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::budgets::service::BudgetPort;
use quansio_server::runtime::budgets::{
    BudgetError, BudgetLimits, BudgetService, BudgetSpec, Meter, MeterUsage,
};
use quansio_server::runtime::state_machine::RuntimeIdentity;
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";

fn identity(tenant: &str) -> RuntimeIdentity {
    RuntimeIdentity::system(
        tenant,
        "budget-service-paths-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

async fn prepare(prefix: &str) -> Option<(String, PgPool, BudgetService)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let service = BudgetService::new(pool.clone(), identity(TENANT)).expect("service");
    Some((name, pool, service))
}

fn meters(pairs: &[(Meter, u64)]) -> MeterUsage {
    let mut set = MeterUsage::new();
    for (meter, value) in pairs {
        set.set(*meter, *value);
    }
    set
}

fn workspace_budget(limits: BudgetLimits) -> BudgetSpec {
    BudgetSpec {
        workspace_id: Some(WORKSPACE.to_string()),
        scope: "workspace".to_string(),
        scope_ref: Some(WORKSPACE.to_string()),
        limits,
        parent_budget_id: None,
    }
}

async fn usage_events(pool: &PgPool) -> Vec<String> {
    quansio_events::EventStore::new(pool.clone())
        .read_events_after(TENANT, None, 1_000)
        .await
        .expect("events")
        .into_iter()
        .map(|event| event.event_type.to_string())
        .filter(|event_type| event_type.starts_with("usage."))
        .collect()
}

#[tokio::test]
async fn recording_against_an_unknown_budget_or_for_a_malformed_tenant_is_refused() {
    let Some((name, pool, service)) = prepare("qa001_budget_unknown").await else {
        blocked_marker();
        return;
    };
    assert_eq!(service.identity().tenant_id, TENANT);
    let refused = service
        .record("bdg_missing", &meters(&[(Meter::Tokens, 5)]))
        .await
        .expect_err("unknown budget");
    assert!(
        matches!(&refused, BudgetError::NotFound { id } if id == "bdg_missing"),
        "{refused:?}"
    );
    assert_eq!(refused.code(), "NOT_FOUND");
    assert!(usage_events(&pool).await.is_empty());

    // A service bound to a malformed tenant id is refused at construction.
    let refused = BudgetService::new(pool.clone(), identity("tenant-1")).err();
    assert!(
        matches!(&refused, Some(BudgetError::Malformed { id, .. }) if id == "tenant-1"),
        "{refused:?}"
    );
    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn recording_a_meter_the_budget_does_not_limit_counts_it_without_a_threshold() {
    let Some((name, pool, service)) = prepare("qa001_budget_unlimited").await else {
        blocked_marker();
        return;
    };
    service
        .create("bdg_ws", workspace_budget(meters(&[(Meter::Tokens, 100)])))
        .await
        .expect("budget");
    let outcome = service
        .record("bdg_ws", &meters(&[(Meter::ToolCalls, 1_000)]))
        .await
        .expect("record");
    assert!(outcome.may_continue(), "an unlimited meter never exhausts");
    assert_eq!(outcome.consumed.get(Meter::ToolCalls), 1_000);
    assert_eq!(outcome.consumed.get(Meter::Tokens), 0);
    assert_eq!(
        usage_events(&pool).await,
        vec!["usage.recorded".to_string()],
        "no threshold is crossed on a meter without a limit"
    );

    // Crossing 80 % of a limited meter in the same record does emit the threshold.
    let outcome = service
        .record(
            "bdg_ws",
            &meters(&[(Meter::ToolCalls, 1), (Meter::Tokens, 85)]),
        )
        .await
        .expect("record");
    assert!(outcome.may_continue());
    assert_eq!(
        usage_events(&pool).await,
        vec![
            "usage.recorded".to_string(),
            "usage.recorded".to_string(),
            "usage.threshold_crossed".to_string(),
        ]
    );
    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn lowering_limits_below_consumption_exhausts_the_budget_and_the_port_then_refuses() {
    let Some((name, pool, service)) = prepare("qa001_budget_lowered").await else {
        blocked_marker();
        return;
    };
    service
        .create(
            "bdg_ws",
            workspace_budget(meters(&[(Meter::Tokens, 1_000)])),
        )
        .await
        .expect("budget");

    // Charged through the trait object a composition root holds.
    let port: &dyn BudgetPort = &service;
    let charged = port
        .charge("bdg_ws", &meters(&[(Meter::Tokens, 300)]))
        .await
        .expect("charge within the limit");
    assert_eq!(charged.consumed.get(Meter::Tokens), 300);
    assert!(charged.may_continue());

    let lowered = service
        .set_limits("bdg_ws", meters(&[(Meter::Tokens, 200)]))
        .await
        .expect("lower limits");
    assert_eq!(lowered.status, "exhausted");
    assert!(!lowered.is_active());
    assert_eq!(lowered.exhausted_meters(), vec![Meter::Tokens]);
    assert_eq!(
        lowered.consumed.get(Meter::Tokens),
        300,
        "what was consumed is kept"
    );
    assert_eq!(
        service.load("bdg_ws").await.expect("load").status,
        "exhausted"
    );

    let refused = port
        .charge("bdg_ws", &meters(&[(Meter::Tokens, 1)]))
        .await
        .expect_err("an exhausted budget funds nothing");
    assert!(
        matches!(
            refused,
            BudgetError::Exhausted {
                meter: Meter::Tokens,
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(
        service
            .load("bdg_ws")
            .await
            .expect("load")
            .consumed
            .get(Meter::Tokens),
        300,
        "the refused charge consumed nothing"
    );

    // Raising the limit again reactivates it.
    let raised = service
        .set_limits("bdg_ws", meters(&[(Meter::Tokens, 2_000)]))
        .await
        .expect("raise limits");
    assert_eq!(raised.status, "active");
    assert!(usage_events(&pool)
        .await
        .iter()
        .any(|event| event == "usage.budget_set"));
    drop_pool(&pool, &name).await;
}
