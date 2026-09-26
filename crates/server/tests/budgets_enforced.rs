//! RUN-010 / OPS-004: budgets are granted by `SetBudget` and enforced by the runtime — a spent budget
//! starts no new model call, what each call used is charged, and crossing 80 % of a limit is announced
//! once (DOMAIN.md §13.2).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (the dev stack). Absent → `BLOCKED_EXTERNAL`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::Request;
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use quansio_server::runtime::budgets::{BudgetService, BudgetSpec, Meter, MeterUsage};
use quansio_server::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, ModelUsage, RuntimeError,
    RuntimeIdentity,
};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0BDGT1";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0BDGT1";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0BDGT2";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0BDGT1";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0BDGT1";

/// A model that reports 20 tokens a call (15 in, 5 out) and counts its calls.
#[derive(Default)]
struct TokenModel {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ModelProposalSource for TokenModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelProposal {
            assistant_text: Some("noted".to_string()),
            usage: Some(ModelUsage {
                input_tokens: 15,
                output_tokens: 5,
            }),
            ..ModelProposal::default()
        })
    }
}

struct Harness {
    router: axum::Router,
    pool: sqlx::PgPool,
    name: String,
    model: Arc<TokenModel>,
}

async fn prepare(prefix: &str) -> Option<Harness> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'viewer')")
        .bind(VIEWER)
        .bind(format!("{VIEWER}@example.com"))
        .execute(&mut *tx)
        .await
        .expect("user");
    schema::set_tenant_context(&mut tx, TENANT).await.expect("tenant");
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ('tm_01J8Z3K6F1N8VQ2X5W9Y0BDGT2', $1, $2, 'member', 'active')")
        .bind(TENANT)
        .bind(VIEWER)
        .execute(&mut *tx)
        .await
        .expect("tenant membership");
    sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ('wm_01J8Z3K6F1N8VQ2X5W9Y0BDGT2', $1, $2, $3, 'viewer', 'active')")
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(VIEWER)
        .execute(&mut *tx)
        .await
        .expect("workspace membership");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0BDGT1', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([{ "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    let model = Arc::new(TokenModel::default());
    let state = ApiState::new(pool.clone())
        .expect("api")
        .with_model_source(model.clone());
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
        model,
    })
}

async fn call(router: &axum::Router, user: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", user);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(axum::body::Body::empty()).expect("request"),
    };
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn command_id() -> String {
    format!("cmd_{}", UlidGenerator::new().generate().to_base32())
}

async fn command(router: &axum::Router, user: &str, name: &str, params: Value) -> (u16, Value) {
    call(
        router,
        user,
        "POST",
        &format!("/v1/commands/{name}"),
        Some(json!({ "command_id": command_id(), "params": params })),
    )
    .await
}

async fn events_of(pool: &sqlx::PgPool, event_type: &str) -> Vec<Value> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT).await.expect("tenant");
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT payload FROM runtime_events WHERE tenant_id = $1 AND type = $2 ORDER BY sequence",
    )
    .bind(TENANT)
    .bind(event_type)
    .fetch_all(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    rows.into_iter().map(|(payload,)| payload).collect()
}

#[tokio::test]
async fn set_budget_grants_and_relimits_within_the_tenant_budget() {
    let Some(h) = prepare("budget_grant").await else {
        common::blocked_marker();
        return;
    };
    let (status, tenant) = command(&h.router, OWNER, "SetBudget", json!({ "scope": "tenant", "limits": { "tokens": 100 } })).await;
    assert_eq!(status, 200, "{tenant}");
    let tenant_id = tenant["result"]["id"].as_str().expect("id").to_string();
    assert!(tenant_id.starts_with("bdg_"), "{tenant}");

    // A workspace budget nests under the tenant's and may not ask for more than it has left.
    let (status, workspace) = command(&h.router, OWNER, "SetBudget", json!({ "scope": "workspace", "workspace_id": WORKSPACE, "limits": { "tokens": 60 } })).await;
    assert_eq!(status, 200, "{workspace}");
    assert_eq!(workspace["result"]["parent_budget_id"], tenant_id.as_str());
    let (status, refused) = command(&h.router, OWNER, "SetBudget", json!({ "scope": "workspace", "workspace_id": WORKSPACE, "limits": { "tokens": 500 } })).await;
    assert_eq!(refused["code"], "VALIDATION_BOUNDS", "{status} {refused}");

    // Re-limiting keeps the same budget.
    let (status, relimited) = command(&h.router, OWNER, "SetBudget", json!({ "scope": "workspace", "workspace_id": WORKSPACE, "limits": { "tokens": 40 } })).await;
    assert_eq!(status, 200, "{relimited}");
    assert_eq!(relimited["result"]["id"], workspace["result"]["id"]);
    assert_eq!(relimited["result"]["limits"]["tokens"], 40);

    // Only an administrator grants budgets; a scope is tenant or workspace.
    let (status, denied) = command(&h.router, VIEWER, "SetBudget", json!({ "scope": "workspace", "workspace_id": WORKSPACE, "limits": { "tokens": 10 } })).await;
    assert_eq!(denied["code"], "SCOPE_FORBIDDEN", "{status} {denied}");
    let (_, unknown) = command(&h.router, OWNER, "SetBudget", json!({ "scope": "galaxy", "limits": {} })).await;
    assert_eq!(unknown["code"], "VALIDATION_SCHEMA", "{unknown}");

    // The usage read shows both.
    let (status, usage) = call(&h.router, OWNER, "GET", &format!("/v1/usage?workspace_id={WORKSPACE}"), None).await;
    assert_eq!(status, 200, "{usage}");
    let scopes: Vec<&str> = usage["budgets"].as_array().expect("budgets").iter().filter_map(|budget| budget["scope"].as_str()).collect();
    assert_eq!(scopes, ["workspace", "tenant"], "{usage}");
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_spent_budget_starts_no_new_model_call() {
    let Some(h) = prepare("budget_enforced").await else {
        common::blocked_marker();
        return;
    };
    // 50 tokens for the workspace; each model call uses 20.
    let (status, granted) = command(&h.router, OWNER, "SetBudget", json!({ "scope": "workspace", "workspace_id": WORKSPACE, "limits": { "tokens": 50 } })).await;
    assert_eq!(status, 200, "{granted}");
    for turn in 1..=3 {
        let (status, posted) = command(&h.router, OWNER, "PostMessage", json!({ "workspace_id": WORKSPACE, "content": format!("note {turn}") })).await;
        assert_eq!(status, 200, "{posted}");
        assert_eq!(posted["result"]["turn"]["kind"], "completed", "turn {turn}: {posted}");
    }
    assert_eq!(h.model.calls.load(Ordering::SeqCst), 3);
    let (_, usage) = call(&h.router, OWNER, "GET", &format!("/v1/usage?workspace_id={WORKSPACE}"), None).await;
    let budget = &usage["budgets"][0];
    assert_eq!(budget["consumed"]["tokens"], 60, "every call was charged: {usage}");
    assert_eq!(budget["status"], "exhausted", "{usage}");

    // The next message is accepted, but its turn starts no model call: the run is suspended.
    let (status, posted) = command(&h.router, OWNER, "PostMessage", json!({ "workspace_id": WORKSPACE, "content": "one more" })).await;
    assert_eq!(status, 200, "{posted}");
    assert_eq!(posted["result"]["turn"]["kind"], "budget_exhausted", "{posted}");
    assert_eq!(h.model.calls.load(Ordering::SeqCst), 3, "no model call against a spent budget");
    let run_id = posted["result"]["run_id"].as_str().expect("run");
    let (_, run) = call(&h.router, OWNER, "GET", &format!("/v1/runs/{run_id}"), None).await;
    assert_eq!(run["status"], "SUSPENDED", "{run}");

    // The soft quota is announced once (40 of 50 is 80 %), exhaustion once.
    let crossed = events_of(&h.pool, "usage.threshold_crossed").await;
    assert_eq!(crossed.len(), 1, "{crossed:?}");
    assert_eq!(crossed[0]["meter"], "tokens");
    assert_eq!(crossed[0]["consumed"], 40);
    assert_eq!(events_of(&h.pool, "usage.exhausted").await.len(), 1);

    // Raising the limit lets work start again.
    let (status, _) = command(&h.router, OWNER, "SetBudget", json!({ "scope": "workspace", "workspace_id": WORKSPACE, "limits": { "tokens": 1000 } })).await;
    assert_eq!(status, 200);
    let (_, posted) = command(&h.router, OWNER, "PostMessage", json!({ "workspace_id": WORKSPACE, "content": "and now" })).await;
    assert_eq!(posted["result"]["turn"]["kind"], "completed", "{posted}");
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn concurrent_charges_add_up() {
    let Some(h) = prepare("budget_race").await else {
        common::blocked_marker();
        return;
    };
    let identity = RuntimeIdentity::system(TENANT, "test", CorrelationId::generate(&mut UlidGenerator::new()));
    let service = BudgetService::new(h.pool.clone(), identity).expect("service");
    let mut limits = MeterUsage::new();
    limits.set(Meter::Tokens, 1_000);
    let budget = service
        .create(
            &format!("bdg_{}", UlidGenerator::new().generate().to_base32()),
            BudgetSpec {
                workspace_id: None,
                scope: "tenant".to_string(),
                scope_ref: None,
                limits,
                parent_budget_id: None,
            },
        )
        .await
        .expect("budget");
    let mut one = MeterUsage::new();
    one.set(Meter::Tokens, 7);
    let charges = (0..20).map(|_| {
        let (service, id, one) = (service.clone(), budget.id.clone(), one.clone());
        tokio::spawn(async move { service.record(&id, &one).await })
    });
    for charge in charges {
        charge.await.expect("task").expect("record");
    }
    let after = service.load(&budget.id).await.expect("load");
    assert_eq!(after.consumed.get(Meter::Tokens), 140, "twenty concurrent charges of 7, none lost");
    common::drop_pool(&h.pool, &h.name).await;
}
