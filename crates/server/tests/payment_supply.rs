//! `SupplyPaymentInput` through the real API and a real PostgreSQL database (ADP-009, DOMAIN.md §13.9 C, §14).
//!
//! What is proved: a signed-in person who can author work in the workspace seals the values of one checkout; the answer, the
//! command log, the audit chain and every other table hold **neither** the card number **nor** the security code (the log's
//! digest even covers the params without their values, so a short code cannot be recovered from it); the sealed value is the one
//! the worker's take returns, once; a viewer, a service principal, an unknown field, a non-https origin, an over-long lifetime
//! and a deployment without a key provider are each refused and seal nothing; a resend of a `command_id` replays the outcome
//! without sealing again.
//!
//! Only a payment provider's published test card number and a made-up security code are used.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; without it a test prints `BLOCKED_EXTERNAL` and returns.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use axum::http::Request;
use quansio_machine::secrets::payment::PaymentInputs;
use quansio_machine::secrets::LocalMasterKeyProvider;
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tower::ServiceExt;

mod common;
use common::{blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP00D";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP00D";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADPVWR";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP00D";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP00D";
const TEST_CARD: &str = "4242424242424242";
const TEST_CVC: &str = "6803";
const ORIGIN: &str = "https://shop.example";

static NEXT_COMMAND: AtomicU32 = AtomicU32::new(1);

fn command_id() -> String {
    format!(
        "cmd_01J8Z3K6F1N8VQ2X5W9Y0P{:04}",
        NEXT_COMMAND.fetch_add(1, Ordering::SeqCst)
    )
}

fn provider() -> LocalMasterKeyProvider {
    LocalMasterKeyProvider::from_master_key(&[9u8; 32], "local:payment-supply-test")
        .expect("a master key")
}

struct Harness {
    router: axum::Router,
    pool: PgPool,
    name: String,
}

async fn prepare(prefix: &str, keys: bool) -> Option<Harness> {
    let name = scratch_name(prefix);
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, 'viewer@example.com', 'seed')")
        .bind(VIEWER)
        .execute(&mut *tx)
        .await
        .expect("user");
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ('tm_01J8Z3K6F1N8VQ2X5W9YADPVWR', $1, $2, 'member', 'active')")
        .bind(TENANT)
        .bind(VIEWER)
        .execute(&mut *tx)
        .await
        .expect("tenant membership");
    sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ('wm_01J8Z3K6F1N8VQ2X5W9YADPVWR', $1, $2, $3, 'viewer', 'active')")
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(VIEWER)
        .execute(&mut *tx)
        .await
        .expect("workspace membership");
    tx.commit().await.expect("commit");
    let mut state = ApiState::new(pool.clone()).expect("api");
    if keys {
        state = state.with_key_provider(Arc::new(provider()));
    }
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
    })
}

macro_rules! harness {
    ($prefix:expr, $keys:expr) => {
        match prepare($prefix, $keys).await {
            Some(h) => h,
            None => {
                blocked_marker();
                return;
            }
        }
    };
}

#[derive(Clone)]
enum Who {
    User(&'static str),
    Bearer(String),
}

async fn call(h: &Harness, who: &Who, name: &str, command_id: &str, params: Value) -> (u16, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/commands/{name}"))
        .header("x-quansio-tenant", TENANT)
        .header("content-type", "application/json");
    builder = match who {
        Who::User(user) => builder.header("x-quansio-user", *user),
        Who::Bearer(token) => builder.header("authorization", format!("Bearer {token}")),
    };
    let request = builder
        .body(axum::body::Body::from(
            json!({ "command_id": command_id, "params": params }).to_string(),
        ))
        .expect("request");
    let response = h.router.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn supply(h: &Harness, who: &Who, params: Value) -> (u16, Value) {
    call(h, who, "SupplyPaymentInput", &command_id(), params).await
}

fn checkout(card: &str, cvc: &str) -> Value {
    json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": { "card_number": card, "cvc": cvc } })
}

async fn active_handles(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM secret_handles WHERE provider = 'payment_input' AND status = 'active'")
        .fetch_one(pool)
        .await
        .expect("count")
}

async fn all_handles(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM secret_handles WHERE provider = 'payment_input'")
        .fetch_one(pool)
        .await
        .expect("count")
}

/// Every table, as text, searched for each needle.
async fn find_anywhere(pool: &PgPool, needles: &[&str]) -> Vec<(String, String)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND left(tablename, 1) <> '_'",
    )
    .fetch_all(pool)
    .await
    .expect("tables");
    let mut found = Vec::new();
    for table in tables {
        let rows = sqlx::query(&format!("SELECT t::text AS row FROM {table} t"))
            .fetch_all(pool)
            .await
            .expect("scan");
        for row in rows {
            let text: String = row.try_get("row").unwrap_or_default();
            for needle in needles {
                if text.contains(needle) {
                    found.push((table.clone(), (*needle).to_string()));
                }
            }
        }
    }
    found
}

fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or("")
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_seals_one_checkout_and_no_record_holds_a_value() {
    let h = harness!("pay_supply", true);
    let who = Who::User(OWNER);
    let (status, body) = supply(&h, &who, checkout(TEST_CARD, TEST_CVC)).await;
    assert_eq!(status, 200, "{body}");
    let supplied = body["result"]["supplied"].as_array().expect("supplied");
    let mut names: Vec<&str> = supplied
        .iter()
        .map(|entry| entry["field"].as_str().expect("field"))
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["card_number", "cvc"]);
    assert!(supplied.iter().all(|entry| entry["expires_at"].is_string()));
    assert_eq!(body["result"]["origin"], ORIGIN);
    assert!(
        !body.to_string().contains(TEST_CARD) && !body.to_string().contains(TEST_CVC),
        "the answer holds a value: {body}"
    );
    assert_eq!(active_handles(&h.pool).await, 2);

    // Not in the command log (result, digest), not in the audit chain, not in any other table: the sealed bytes are the only
    // place the value exists, and they are not the value.
    assert_eq!(
        find_anywhere(&h.pool, &[TEST_CARD, TEST_CVC]).await,
        Vec::<(String, String)>::new()
    );
    let audit: String = sqlx::query_scalar("SELECT reason FROM audit_entries WHERE action = 'SupplyPaymentInput' ORDER BY occurred_at DESC LIMIT 1")
        .fetch_one(&h.pool)
        .await
        .expect("audit entry");
    assert!(
        audit.contains("card_number") && audit.contains(ORIGIN),
        "{audit}"
    );

    // The value the worker takes is the value the person typed, once; the stored seal is overwritten with it.
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let taken = PaymentInputs::take(
        &mut tx,
        &provider(),
        TENANT,
        WORKSPACE,
        "card_number",
        "act_1",
        &now(),
    )
    .await
    .expect("take");
    assert_eq!(
        taken.materialization.material.expose(),
        TEST_CARD.as_bytes()
    );
    assert_eq!(taken.origin, ORIGIN);
    tx.commit().await.expect("commit");
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    assert!(
        PaymentInputs::take(
            &mut tx,
            &provider(),
            TENANT,
            WORKSPACE,
            "card_number",
            "act_2",
            &now()
        )
        .await
        .is_err(),
        "a second take finds nothing"
    );
    tx.rollback().await.expect("rollback");
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_log_digest_does_not_depend_on_the_values() {
    let h = harness!("pay_digest", true);
    let who = Who::User(OWNER);
    let (first, _) = supply(&h, &who, checkout(TEST_CARD, TEST_CVC)).await;
    let (second, _) = supply(&h, &who, checkout("5555555555554444", "1234")).await;
    assert_eq!((first, second), (200, 200));
    let digests: Vec<String> = sqlx::query_scalar("SELECT params_digest FROM commands WHERE command_name = 'SupplyPaymentInput' ORDER BY created_at")
        .fetch_all(&h.pool)
        .await
        .expect("digests");
    assert_eq!(digests.len(), 2);
    assert_eq!(
        digests[0], digests[1],
        "the digest covers the params without their values"
    );
    // Latest wins: the second supply cleared the first.
    assert_eq!(active_handles(&h.pool).await, 2);
    assert_eq!(all_handles(&h.pool).await, 4);
    assert_eq!(
        find_anywhere(&h.pool, &[TEST_CARD, TEST_CVC, "5555555555554444", "1234"]).await,
        Vec::<(String, String)>::new()
    );
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resend_replays_the_outcome_and_seals_nothing_more() {
    let h = harness!("pay_replay", true);
    let who = Who::User(OWNER);
    let id = command_id();
    let (status, first) = call(
        &h,
        &who,
        "SupplyPaymentInput",
        &id,
        checkout(TEST_CARD, TEST_CVC),
    )
    .await;
    assert_eq!(status, 200, "{first}");
    let (status, again) = call(
        &h,
        &who,
        "SupplyPaymentInput",
        &id,
        checkout(TEST_CARD, TEST_CVC),
    )
    .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["replayed"], true);
    assert_eq!(again["result"], first["result"]);
    assert_eq!(all_handles(&h.pool).await, 2);
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_a_service_principal_and_a_stranger_workspace_are_refused_and_seal_nothing() {
    let h = harness!("pay_refused", true);
    let (status, refused) = supply(&h, &Who::User(VIEWER), checkout(TEST_CARD, TEST_CVC)).await;
    assert_eq!(status, 403, "{refused}");
    assert!(!refused.to_string().contains(TEST_CARD));

    let owner = Who::User(OWNER);
    // A principal holding the Browser/computer family still cannot supply: the values are a person's.
    let (status, created) = call(
        &h,
        &owner,
        "CreateServicePrincipal",
        &command_id(),
        json!({ "name": "browser-bot", "scopes": ["Browser/computer"] }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    let bot = Who::Bearer(
        created["result"]["secret"]
            .as_str()
            .expect("key")
            .to_string(),
    );
    let (status, refused) = supply(&h, &bot, checkout(TEST_CARD, TEST_CVC)).await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(code(&refused), "SCOPE_FORBIDDEN");

    let (status, refused) = supply(&h, &owner, json!({ "workspace_id": "ws_01J8Z3K6F1N8VQ2X5W9YADP0ZZ", "origin": ORIGIN, "fields": { "card_number": TEST_CARD } })).await;
    assert_eq!(status, 404, "{refused}");

    assert_eq!(all_handles(&h.pool).await, 0);
    assert_eq!(
        find_anywhere(&h.pool, &[TEST_CARD]).await,
        Vec::<(String, String)>::new()
    );
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_supply_that_breaks_a_rule_is_refused_whole() {
    let h = harness!("pay_invalid", true);
    let who = Who::User(OWNER);
    let long = "9".repeat(200);
    let cases = [
        json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": {} }),
        json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": { "card_number": TEST_CARD, "password": "hunter2" } }),
        json!({ "workspace_id": WORKSPACE, "origin": "http://shop.example", "fields": { "card_number": TEST_CARD } }),
        json!({ "workspace_id": WORKSPACE, "origin": "https://shop.example/checkout", "fields": { "card_number": TEST_CARD } }),
        json!({ "workspace_id": WORKSPACE, "origin": "https://u:p@shop.example", "fields": { "card_number": TEST_CARD } }),
        json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": { "card_number": long } }),
        json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": { "card_number": TEST_CARD }, "ttl_seconds": 100000 }),
        json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": { "card_number": TEST_CARD }, "ttl_seconds": 1 }),
        json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": { "card_number": 4242 } }),
        json!({ "workspace_id": WORKSPACE, "fields": { "card_number": TEST_CARD } }),
    ];
    for case in cases {
        let (status, refused) = supply(&h, &who, case.clone()).await;
        assert_eq!(status, 400, "{case} -> {refused}");
        assert_eq!(code(&refused), "VALIDATION_SCHEMA", "{refused}");
        assert!(
            !refused.to_string().contains(TEST_CARD),
            "a refusal repeats a value: {refused}"
        );
    }
    assert_eq!(all_handles(&h.pool).await, 0);
    // One bad field among good ones seals none of the good ones.
    let (status, _) = supply(&h, &who, json!({ "workspace_id": WORKSPACE, "origin": ORIGIN, "fields": { "card_number": TEST_CARD, "cvc": "" } })).await;
    assert_eq!(status, 400);
    assert_eq!(all_handles(&h.pool).await, 0);
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deployment_without_a_key_provider_refuses_and_keeps_nothing() {
    let h = harness!("pay_nokeys", false);
    let (status, refused) = supply(&h, &Who::User(OWNER), checkout(TEST_CARD, TEST_CVC)).await;
    assert_eq!(status, 503, "{refused}");
    assert_eq!(code(&refused), "ADAPTER_UNAVAILABLE");
    assert_eq!(all_handles(&h.pool).await, 0);
    assert_eq!(
        find_anywhere(&h.pool, &[TEST_CARD, TEST_CVC]).await,
        Vec::<(String, String)>::new()
    );
    drop_pool(&h.pool, &h.name).await;
}
