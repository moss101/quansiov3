//! ADP-004: the maintenance status a desktop reads before it restarts itself (DOMAIN.md §13.6 A).
//!
//! `GET /v1/maintenance` is derived on every read from the rows their owners keep: Turns in flight
//! (runtime store), effects reserved, dispatched or being reconciled (Effect Ledger) and open terminal
//! sessions (machine). The suite stages each kind of row on a real PostgreSQL and checks that exactly the
//! ones a restart would interrupt are counted, that a row finishing flips the answer back (nothing is
//! cached), that a workspace narrows it and another tenant's rows never count.
//!
//! Runs against a scratch database (blocked, not skipped, without one).

use axum::http::Request;
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0M4A00";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0M4A00";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0M4A00";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0M4A00";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0M4A01";
const OTHER_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0M4A01";
const STRANGER_TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0M4B00";
const STRANGER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0M4B00";
const STRANGER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0M4B00";
const STRANGER_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0M4B00";

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}_01J8Z3K6F1N8VQ2X5W9Y0{n:05}")
}

struct Harness {
    router: axum::Router,
    pool: PgPool,
    name: String,
}

async fn in_tenant(pool: &PgPool, tenant: &str) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, tenant)
        .await
        .expect("tenant context");
    tx
}

/// One target, an agent thread and a Run in `workspace`, with the Run in `status`.
async fn add_run(pool: &PgPool, tenant: &str, workspace: &str, node: &str, n: u32, status: &str) {
    let mut tx = in_tenant(pool, tenant).await;
    sqlx::query(
        "INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, status, owner_kind) \
         VALUES ($1, $2, $3, 'persistent_workspace_computer', 'local_capsule_macos', 'READY', 'workspace')",
    )
    .bind(id("tgt", n))
    .bind(tenant)
    .bind(workspace)
    .execute(&mut *tx)
    .await
    .expect("target");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, work_node_id, status, execution_target_id) \
         VALUES ($1, $2, $3, 'worker', $4, 'ACTIVE', $5)",
    )
    .bind(id("ath", n))
    .bind(tenant)
    .bind(workspace)
    .bind(node)
    .bind(id("tgt", n))
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    sqlx::query(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, status, \
                           trigger_kind, execution_target_id, started_at) \
         VALUES ($1, $2, $3, $4, $5, 1, $6, 'manual', $7, now())",
    )
    .bind(id("run", n))
    .bind(tenant)
    .bind(workspace)
    .bind(node)
    .bind(id("ath", n))
    .bind(status)
    .bind(id("tgt", n))
    .execute(&mut *tx)
    .await
    .expect("run");
    tx.commit().await.expect("commit");
}

async fn add_turn(pool: &PgPool, tenant: &str, run: u32, n: u32, status: &str) {
    let mut tx = in_tenant(pool, tenant).await;
    sqlx::query(
        "INSERT INTO turns (id, tenant_id, run_id, seq, input_kind, status) VALUES ($1, $2, $3, $4, 'message', $5)",
    )
    .bind(id("trn", n))
    .bind(tenant)
    .bind(id("run", run))
    .bind(1_i32)
    .bind(status)
    .execute(&mut *tx)
    .await
    .expect("turn");
    tx.commit().await.expect("commit");
}

async fn add_effect(pool: &PgPool, tenant: &str, workspace: &str, run: u32, n: u32, status: &str) {
    let mut tx = in_tenant(pool, tenant).await;
    sqlx::query(
        "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, effect_class, tier, resource, params_digest, \
                                     idempotency_key, capability_projection_id, status, generation) \
         VALUES ($1, $2, $3, $4, 'fs.write', 1, '{}'::jsonb, 'sha256:0', $5, $6, $7, 1)",
    )
    .bind(id("eff", n))
    .bind(tenant)
    .bind(workspace)
    .bind(id("run", run))
    .bind(format!("key-{n}"))
    .bind(id("cp", n))
    .bind(status)
    .execute(&mut *tx)
    .await
    .expect("effect");
    tx.commit().await.expect("commit");
}

async fn add_terminal(pool: &PgPool, tenant: &str, target: u32, n: u32, status: &str) {
    let mut tx = in_tenant(pool, tenant).await;
    sqlx::query(
        "INSERT INTO terminal_sessions (id, tenant_id, target_id, pty_ref, cursor, status) \
         VALUES ($1, $2, $3, 'pty:test', '0', $4)",
    )
    .bind(id("tsn", n))
    .bind(tenant)
    .bind(id("tgt", target))
    .bind(status)
    .execute(&mut *tx)
    .await
    .expect("terminal");
    tx.commit().await.expect("commit");
}

async fn set_status(pool: &PgPool, tenant: &str, table: &str, row: &str, status: &str) {
    let mut tx = in_tenant(pool, tenant).await;
    // The table names are fixed by the tests below, never input.
    let sql = format!("UPDATE {table} SET status = $1 WHERE id = $2");
    sqlx::query(&sql)
        .bind(status)
        .bind(row)
        .execute(&mut *tx)
        .await
        .expect("update");
    tx.commit().await.expect("commit");
}

async fn prepare(prefix: &str) -> Option<Harness> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    common::seed_tenant(
        &pool,
        STRANGER_TENANT,
        STRANGER,
        STRANGER_WORKSPACE,
        STRANGER_NODE,
    )
    .await;
    // A second workspace in the first tenant.
    let mut tx = in_tenant(&pool, TENANT).await;
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'other')")
        .bind(OTHER_WORKSPACE)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("workspace");
    sqlx::query(
        "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) \
         VALUES ($1, $2, $3, 'objective', 'other node', '{\"kind\":\"user\",\"id\":\"usr_seed\"}'::jsonb)",
    )
    .bind(OTHER_NODE)
    .bind(TENANT)
    .bind(OTHER_WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("node");
    tx.commit().await.expect("commit");
    let state = ApiState::new(pool.clone()).expect("api");
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
    })
}

async fn get(router: &axum::Router, tenant: &str, user: Option<&str>, path: &str) -> (u16, Value) {
    let mut builder = Request::builder().method("GET").uri(path);
    if !tenant.is_empty() {
        builder = builder.header("x-quansio-tenant", tenant);
    }
    if let Some(user) = user {
        builder = builder.header("x-quansio-user", user);
    }
    let response = router
        .clone()
        .oneshot(builder.body(axum::body::Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn counts(view: &Value) -> (i64, i64, i64, bool) {
    (
        view["active_turns"].as_i64().expect("active_turns"),
        view["inflight_actions"].as_i64().expect("inflight_actions"),
        view["open_terminals"].as_i64().expect("open_terminals"),
        view["idle"].as_bool().expect("idle"),
    )
}

#[tokio::test]
async fn an_empty_tenant_is_idle() {
    let Some(h) = prepare("maint_idle").await else {
        common::blocked_marker();
        return;
    };
    let (status, view) = get(&h.router, TENANT, Some(OWNER), "/v1/maintenance").await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(counts(&view), (0, 0, 0, true));
    assert!(view["workspace_id"].is_null());
    assert!(view["as_of"].as_str().is_some_and(|text| text.contains('T')));
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn it_counts_exactly_what_a_restart_would_interrupt() {
    let Some(h) = prepare("maint_counts").await else {
        common::blocked_marker();
        return;
    };
    // Turns: only an `active` Turn of a Run that is executing counts.
    add_run(&h.pool, TENANT, WORKSPACE, NODE, 1, "RUNNING").await;
    add_turn(&h.pool, TENANT, 1, 1, "active").await; // counted
    add_run(&h.pool, TENANT, WORKSPACE, NODE, 2, "RUNNING").await;
    add_turn(&h.pool, TENANT, 2, 2, "completed").await; // between Turns: a Run stays RUNNING
    add_run(&h.pool, TENANT, WORKSPACE, NODE, 3, "WAITING_APPROVAL").await;
    add_turn(&h.pool, TENANT, 3, 3, "active").await; // waiting holds no computation
    add_run(&h.pool, TENANT, WORKSPACE, NODE, 4, "VERIFYING").await;
    add_turn(&h.pool, TENANT, 4, 4, "active").await; // counted
    add_run(&h.pool, TENANT, WORKSPACE, NODE, 5, "SUCCEEDED").await;
    add_turn(&h.pool, TENANT, 5, 5, "active").await; // a finished Run's stale Turn row is not work
    // Effects: reserved, dispatched and reconciling are in flight; the rest are not.
    for (n, status) in [
        (1, "RESERVED"),
        (2, "DISPATCHED"),
        (3, "RECONCILING"),
        (4, "OUTCOME_UNKNOWN"),
        (5, "SETTLED_SUCCESS"),
        (6, "PROPOSED"),
        (7, "DENIED"),
    ] {
        add_effect(&h.pool, TENANT, WORKSPACE, 1, n, status).await;
    }
    // Terminals: open ones are reported, closed and lost ones are not.
    add_terminal(&h.pool, TENANT, 1, 1, "open").await;
    add_terminal(&h.pool, TENANT, 1, 2, "closed").await;
    add_terminal(&h.pool, TENANT, 2, 3, "lost").await;

    let (status, view) = get(&h.router, TENANT, Some(OWNER), "/v1/maintenance").await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(counts(&view), (2, 3, 1, false), "{view}");

    // Nothing is cached: finishing the work flips the answer back on the next read.
    set_status(&h.pool, TENANT, "turns", &id("trn", 1), "completed").await;
    set_status(&h.pool, TENANT, "turns", &id("trn", 4), "aborted").await;
    for n in 1..=3 {
        set_status(&h.pool, TENANT, "effect_records", &id("eff", n), "SETTLED_SUCCESS").await;
    }
    let (_, view) = get(&h.router, TENANT, Some(OWNER), "/v1/maintenance").await;
    // An open terminal alone never makes the status busy.
    assert_eq!(counts(&view), (0, 0, 1, true), "{view}");
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_workspace_narrows_the_counts_and_another_tenant_never_counts() {
    let Some(h) = prepare("maint_scope").await else {
        common::blocked_marker();
        return;
    };
    add_run(&h.pool, TENANT, WORKSPACE, NODE, 1, "RUNNING").await;
    add_turn(&h.pool, TENANT, 1, 1, "active").await;
    add_terminal(&h.pool, TENANT, 1, 1, "open").await;
    add_run(&h.pool, TENANT, OTHER_WORKSPACE, OTHER_NODE, 2, "RUNNING").await;
    add_turn(&h.pool, TENANT, 2, 2, "active").await;
    add_effect(&h.pool, TENANT, OTHER_WORKSPACE, 2, 1, "DISPATCHED").await;
    // Another tenant's busy Run is invisible whatever is asked.
    add_run(&h.pool, STRANGER_TENANT, STRANGER_WORKSPACE, STRANGER_NODE, 9, "RUNNING").await;
    add_turn(&h.pool, STRANGER_TENANT, 9, 9, "active").await;
    add_effect(&h.pool, STRANGER_TENANT, STRANGER_WORKSPACE, 9, 9, "DISPATCHED").await;

    let (_, whole) = get(&h.router, TENANT, Some(OWNER), "/v1/maintenance").await;
    assert_eq!(counts(&whole), (2, 1, 1, false), "{whole}");
    let (_, first) = get(
        &h.router,
        TENANT,
        Some(OWNER),
        &format!("/v1/maintenance?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(counts(&first), (1, 0, 1, false), "{first}");
    assert_eq!(first["workspace_id"], WORKSPACE);
    let (_, second) = get(
        &h.router,
        TENANT,
        Some(OWNER),
        &format!("/v1/maintenance?workspace_id={OTHER_WORKSPACE}"),
    )
    .await;
    assert_eq!(counts(&second), (1, 1, 0, false), "{second}");
    // The stranger's own tenant sees only its own work, never the first tenant's.
    let (_, stranger) = get(&h.router, STRANGER_TENANT, Some(STRANGER), "/v1/maintenance").await;
    assert_eq!(counts(&stranger), (1, 1, 0, false), "{stranger}");
    // A workspace of another tenant, asked for by name, finds nothing here.
    let (_, foreign) = get(
        &h.router,
        TENANT,
        Some(OWNER),
        &format!("/v1/maintenance?workspace_id={STRANGER_WORKSPACE}"),
    )
    .await;
    assert_eq!(counts(&foreign), (0, 0, 0, true), "{foreign}");
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn it_refuses_an_unauthenticated_caller_and_a_malformed_workspace() {
    let Some(h) = prepare("maint_refuse").await else {
        common::blocked_marker();
        return;
    };
    let (status, body) = get(&h.router, "", None, "/v1/maintenance").await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["code"], "AUTH_REQUIRED", "{body}");
    let (status, body) = get(
        &h.router,
        TENANT,
        Some(OWNER),
        "/v1/maintenance?workspace_id=thr_notaworkspace",
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "VALIDATION_BOUNDS", "{body}");
    common::drop_pool(&h.pool, &h.name).await;
}
