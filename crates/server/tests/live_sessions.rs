//! APP-007: the live browser/computer/terminal surface's server half. A takeover moves the
//! same BrowserSession row to the user and parks the owning run (`WAITING_TAKEOVER`, the
//! ProtocolState answering `AwaitHandback`); no second session appears; the projection
//! reports the fence and the pause reason; a handback is the only way back and cannot undo
//! a policy pause; the native-computer form drives EXEC-010's control row with its
//! generation fence; every change is one event and one AuditEntry.
//!
//! Runs against a scratch database (blocked, not skipped, without one).

use axum::http::Request;
use quansio_machine::computer_use::store::ComputerControlStore;
use quansio_machine::control::browser::BrowserSessionStore;
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use quansio_server::runtime::protocol_state::{next_safe_action, NextAction, ProtocolStateStore};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0E7VE1";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0E7VE2";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const THREAD: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const TERMINAL: &str = "tsn_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";
const TOOL_CALL: &str = "tc_01J8Z3K6F1N8VQ2X5W9Y0E7VE7";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0E{n:04}")
}

struct Harness {
    router: axum::Router,
    pool: sqlx::PgPool,
    name: String,
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
        .expect("viewer");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, 'member', 'active')")
        .bind(format!("tm_{}", &VIEWER[4..]))
        .bind(TENANT)
        .bind(VIEWER)
        .execute(&mut *tx)
        .await
        .expect("tenant membership");
    sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ($1, $2, $3, $4, 'viewer', 'active')")
        .bind(format!("wm_{}", &VIEWER[4..]))
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(VIEWER)
        .execute(&mut *tx)
        .await
        .expect("workspace membership");
    // The live rows: a READY target, a worker thread and a RUNNING run on it, the run's
    // browser session (agent-held, on a page), an open terminal with a durable cursor, and
    // the target's computer-control row — what `quansio-dev-seed --live` seeds.
    sqlx::query(
        "INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, status, owner_kind) \
         VALUES ($1, $2, $3, 'persistent_workspace_computer', 'local_capsule_macos', 'READY', 'workspace')",
    )
    .bind(TARGET)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("target");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, work_node_id, status, execution_target_id) \
         VALUES ($1, $2, $3, 'worker', $4, 'ACTIVE', $5)",
    )
    .bind(THREAD)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(NODE)
    .bind(TARGET)
    .execute(&mut *tx)
    .await
    .expect("thread");
    sqlx::query(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, status, \
                           trigger_kind, execution_target_id, started_at) \
         VALUES ($1, $2, $3, $4, $5, 1, 'RUNNING', 'manual', $6, now())",
    )
    .bind(RUN)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(NODE)
    .bind(THREAD)
    .bind(TARGET)
    .execute(&mut *tx)
    .await
    .expect("run");
    sqlx::query(
        "INSERT INTO browser_sessions (id, tenant_id, target_id, run_id, control_holder, control_since, current_url, tabs, status) \
         VALUES ($1, $2, $3, $4, 'agent', now(), 'https://example.com/', $5::jsonb, 'active')",
    )
    .bind(SESSION)
    .bind(TENANT)
    .bind(TARGET)
    .bind(RUN)
    .bind(json!([{ "target_id": "cdp-1", "url": "https://example.com/", "title": "Example Domain" }]))
    .execute(&mut *tx)
    .await
    .expect("browser session");
    sqlx::query(
        "INSERT INTO terminal_sessions (id, tenant_id, target_id, run_id, pty_ref, cursor, status, last_command_id) \
         VALUES ($1, $2, $3, $4, 'pty:test', '128', 'open', $5)",
    )
    .bind(TERMINAL)
    .bind(TENANT)
    .bind(TARGET)
    .bind(RUN)
    .bind(command_id(9000))
    .execute(&mut *tx)
    .await
    .expect("terminal session");
    sqlx::query(
        "INSERT INTO computer_controls (target_id, tenant_id, run_id, holder, generation, control_since) \
         VALUES ($1, $2, $3, 'agent', 1, now())",
    )
    .bind(TARGET)
    .bind(TENANT)
    .bind(RUN)
    .execute(&mut *tx)
    .await
    .expect("computer control");
    tx.commit().await.expect("commit");
    let state = ApiState::new(pool.clone()).expect("api");
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
    })
}

fn http(method: &str, path: &str, user: &str, body: Option<Value>) -> Request<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", user);
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(axum::body::Body::empty()).expect("request"),
    }
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn command(
    router: &axum::Router,
    user: &str,
    name: &str,
    id: u32,
    params: Value,
) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(http(
            "POST",
            &format!("/v1/commands/{name}"),
            user,
            Some(json!({ "command_id": command_id(id), "params": params })),
        ))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn get(router: &axum::Router, user: &str, path: &str) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(http("GET", path, user, None))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn events_for(pool: &sqlx::PgPool, aggregate_id: &str) -> Vec<String> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let types: Vec<String> = sqlx::query_scalar(
        "SELECT type FROM runtime_events WHERE tenant_id = $1 AND aggregate_id = $2 ORDER BY sequence",
    )
    .bind(TENANT)
    .bind(aggregate_id)
    .fetch_all(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    types
}

async fn session_count(pool: &sqlx::PgPool) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM browser_sessions WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_one(&mut *tx)
            .await
            .expect("count");
    tx.commit().await.expect("commit");
    count
}

async fn next_action(pool: &sqlx::PgPool) -> Option<NextAction> {
    let mut conn = pool.acquire().await.expect("acquire");
    let state = ProtocolStateStore::load(&mut conn, TENANT, RUN)
        .await
        .expect("protocol state");
    state.as_ref().map(next_safe_action)
}

#[tokio::test]
async fn a_takeover_fences_the_same_session_and_parks_the_run() {
    let Some(h) = prepare("live_takeover").await else {
        common::blocked_marker();
        return;
    };
    // Before: the agent holds the session; the target lists everything the surface shows.
    let (status, view) = get(&h.router, OWNER, &format!("/v1/browser-sessions/{SESSION}")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["session"]["id"], SESSION);
    assert_eq!(view["session"]["control_holder"], "agent");
    assert_eq!(view["session"]["status"], "active");
    assert_eq!(view["session"]["current_url"], "https://example.com/");
    assert_eq!(view["session"]["tabs"][0]["title"], "Example Domain");
    assert_eq!(view["automation"]["paused"], false);
    assert_eq!(view["automation"]["agent_input_fenced"], false);
    assert_eq!(view["run"]["status"], "RUNNING");
    assert_eq!(view["terminal_sessions"][0]["id"], TERMINAL);
    assert_eq!(view["terminal_sessions"][0]["cursor"], 128);
    assert_eq!(view["computer_control"]["holder"], "agent");
    let (status, targets) = get(
        &h.router,
        OWNER,
        &format!("/v1/targets?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(status, 200, "{targets}");
    assert_eq!(targets["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(targets["items"][0]["id"], TARGET);
    assert_eq!(targets["items"][0]["browser_sessions"][0]["id"], SESSION);
    assert_eq!(targets["items"][0]["terminal_sessions"][0]["cursor"], 128);
    // A viewer may look but not take over.
    let (status, refused) = get(
        &h.router,
        VIEWER,
        &format!("/v1/browser-sessions/{SESSION}"),
    )
    .await;
    assert_eq!(status, 200, "{refused}");
    let (status, refused) = command(
        &h.router,
        VIEWER,
        "RequestTakeover",
        1,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["code"], "SCOPE_FORBIDDEN");

    // The takeover: same id, user holds, run parked, ProtocolState fenced.
    let (status, taken) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        2,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 200, "{taken}");
    assert_eq!(taken["result"]["session"]["id"], SESSION);
    assert_eq!(taken["result"]["session"]["control_holder"], "user");
    assert_eq!(taken["result"]["session"]["status"], "paused_takeover");
    assert_eq!(taken["result"]["session"]["agent_may_drive"], false);
    assert_eq!(taken["result"]["run"]["status"], "WAITING_TAKEOVER");
    assert_eq!(
        session_count(&h.pool).await,
        1,
        "a takeover never mints a second session"
    );
    assert_eq!(
        next_action(&h.pool).await,
        Some(NextAction::AwaitHandback {
            session_id: SESSION.to_string()
        })
    );
    let (status, view) = get(&h.router, OWNER, &format!("/v1/browser-sessions/{SESSION}")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["automation"]["paused"], true);
    assert_eq!(view["automation"]["reason"], "user_control");
    assert_eq!(view["automation"]["agent_input_fenced"], true);
    assert_eq!(view["automation"]["run_status"], "WAITING_TAKEOVER");
    assert_eq!(view["run"]["browser_control"]["holder"], "user");
    assert_eq!(view["run"]["browser_control"]["session_id"], SESSION);
    // Taking over what the user already holds is refused, not queued.
    let (status, again) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        3,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["code"], "CONFLICT_STATE");
    // A viewer cannot hand back either.
    let (status, refused) = command(
        &h.router,
        VIEWER,
        "Handback",
        4,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");

    // The handback: same id, agent holds, run resumed, fence cleared.
    let (status, back) = command(
        &h.router,
        OWNER,
        "Handback",
        5,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 200, "{back}");
    assert_eq!(back["result"]["session"]["id"], SESSION);
    assert_eq!(back["result"]["session"]["control_holder"], "agent");
    assert_eq!(back["result"]["session"]["status"], "active");
    assert_eq!(back["result"]["run"]["status"], "RUNNING");
    assert_eq!(session_count(&h.pool).await, 1);
    assert_eq!(next_action(&h.pool).await, Some(NextAction::Continue));
    let (status, view) = get(&h.router, OWNER, &format!("/v1/browser-sessions/{SESSION}")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["automation"]["paused"], false);
    assert_eq!(view["automation"]["agent_input_fenced"], false);
    assert_eq!(view["run"]["browser_control"], Value::Null);
    // Handing back what nobody holds is refused.
    let (status, again) = command(
        &h.router,
        OWNER,
        "Handback",
        6,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["code"], "CONFLICT_STATE");

    // One event per control change on the session, the run's park and resume, and the audit.
    assert_eq!(
        events_for(&h.pool, SESSION).await,
        vec!["browser.control_changed", "browser.control_changed"]
    );
    assert_eq!(
        events_for(&h.pool, RUN).await,
        vec!["run.waiting", "run.resumed"]
    );
    let (status, audit) = get(
        &h.router,
        OWNER,
        &format!("/v1/audit?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(status, 200, "{audit}");
    let actions: Vec<String> = audit["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|entry| {
            Some(format!(
                "{}:{}",
                entry["action"].as_str()?,
                entry["target_ref"].as_str()?
            ))
        })
        .collect();
    assert!(
        actions.contains(&format!("RequestTakeover:browser_session:{SESSION}"))
            && actions.contains(&format!("Handback:browser_session:{SESSION}")),
        "{actions:?}"
    );

    // Validation: exactly one subject.
    let (status, bad) = command(&h.router, OWNER, "RequestTakeover", 7, json!({})).await;
    assert_eq!(status, 400, "{bad}");
    assert_eq!(bad["code"], "VALIDATION_SCHEMA");
    let (status, bad) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        8,
        json!({ "session_id": SESSION, "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 400, "{bad}");
    let (status, missing) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        9,
        json!({ "session_id": "bsn_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ" }),
    )
    .await;
    assert_eq!(status, 404, "{missing}");

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_policy_pause_is_shown_and_no_handback_undoes_it() {
    let Some(h) = prepare("live_policy_pause").await else {
        common::blocked_marker();
        return;
    };
    {
        let mut conn = h.pool.acquire().await.expect("acquire");
        schema::set_tenant_context_conn(&mut conn, TENANT)
            .await
            .expect("tenant");
        BrowserSessionStore::pause_for_policy(&mut conn, TENANT, SESSION, "2026-09-21T00:00:00Z")
            .await
            .expect("policy pause");
    }
    let (status, view) = get(&h.router, OWNER, &format!("/v1/browser-sessions/{SESSION}")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["session"]["control_holder"], "none");
    assert_eq!(view["automation"]["paused"], true);
    assert_eq!(view["automation"]["reason"], "policy");
    assert_eq!(view["automation"]["agent_input_fenced"], true);
    let (status, refused) = command(
        &h.router,
        OWNER,
        "Handback",
        1,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["code"], "CONFLICT_STATE");
    let (status, refused) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        2,
        json!({ "session_id": SESSION }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert!(events_for(&h.pool, SESSION).await.is_empty());
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_native_computer_takeover_drives_the_control_row_with_its_fence() {
    let Some(h) = prepare("live_computer").await else {
        common::blocked_marker();
        return;
    };
    // No call in flight: the takeover completes at once, the run parks on the target.
    let (status, taken) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        1,
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 200, "{taken}");
    assert_eq!(taken["result"]["computer_control"]["holder"], "user");
    assert_eq!(taken["result"]["computer_control"]["generation"], 2);
    assert_eq!(
        taken["result"]["computer_control"]["takeover_pending"],
        false
    );
    assert_eq!(
        taken["result"]["computer_control"]["agent_may_input"],
        false
    );
    assert_eq!(taken["result"]["run"]["status"], "WAITING_TAKEOVER");
    assert_eq!(
        next_action(&h.pool).await,
        Some(NextAction::AwaitHandback {
            session_id: TARGET.to_string()
        })
    );
    let (status, view) = get(&h.router, OWNER, &format!("/v1/browser-sessions/{SESSION}")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["computer_control"]["holder"], "user");
    assert_eq!(
        view["automation"]["agent_input_fenced"], true,
        "the run is parked"
    );
    let (status, again) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        2,
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    let (status, back) = command(
        &h.router,
        OWNER,
        "Handback",
        3,
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 200, "{back}");
    assert_eq!(back["result"]["computer_control"]["holder"], "agent");
    assert_eq!(back["result"]["computer_control"]["generation"], 3);
    assert_eq!(back["result"]["run"]["status"], "RUNNING");
    assert_eq!(next_action(&h.pool).await, Some(NextAction::Continue));
    let (status, again) = command(
        &h.router,
        OWNER,
        "Handback",
        4,
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["code"], "CONFLICT_STATE");

    // A call in flight: the takeover fences new input and stays pending until it settles.
    {
        let mut conn = h.pool.acquire().await.expect("acquire");
        ComputerControlStore::begin_action(&mut conn, TENANT, TARGET, TOOL_CALL, 3)
            .await
            .expect("begin action");
    }
    let (status, pending) = command(
        &h.router,
        OWNER,
        "RequestTakeover",
        5,
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 200, "{pending}");
    assert_eq!(pending["result"]["computer_control"]["holder"], "agent");
    assert_eq!(
        pending["result"]["computer_control"]["takeover_pending"],
        true
    );
    assert_eq!(
        pending["result"]["computer_control"]["active_tool_call_id"],
        TOOL_CALL
    );
    assert_eq!(
        pending["result"]["computer_control"]["agent_may_input"],
        false
    );
    let (status, refused) = command(
        &h.router,
        OWNER,
        "Handback",
        6,
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        events_for(&h.pool, TARGET).await,
        vec![
            "target.control_changed",
            "target.control_changed",
            "target.control_changed"
        ]
    );
    common::drop_pool(&h.pool, &h.name).await;
}

// ------------------------------------------------------------------ ADP-001: user-only browser decisions

const TAB: &str = "cdp-1";

/// The worker's report with one pending certificate error, written the way the control channel will write it.
async fn report_pending_certificate(pool: &sqlx::PgPool) {
    let report = quansio_machine::control::browser::GuardReport::from_json(&json!({
        "tabs": [{
            "tab_id": TAB,
            "url": "https://example.com/",
            "document_revision": "rev_0123456789abcdef",
            "certificates": [{
                "id": "crt_1", "origin": "https://example.com", "error_type": "ERR_CERT_AUTHORITY_INVALID",
                "status": "pending", "certificate_sha256": null, "decided_by": null
            }],
            "downloads": [],
            "popups_blocked": 0
        }],
        "in_doubt": []
    }))
    .expect("report");
    let mut conn = pool.acquire().await.expect("acquire");
    schema::set_tenant_context_conn(&mut conn, TENANT)
        .await
        .expect("tenant");
    BrowserSessionStore::record_guard_report(
        &mut conn,
        TENANT,
        SESSION,
        &report,
        "2026-10-08T00:00:00Z",
    )
    .await
    .expect("report");
}

async fn session_guard(pool: &sqlx::PgPool) -> Value {
    let mut conn = pool.acquire().await.expect("acquire");
    schema::set_tenant_context_conn(&mut conn, TENANT)
        .await
        .expect("tenant");
    let session = BrowserSessionStore::load(&mut conn, TENANT, SESSION)
        .await
        .expect("load");
    session.guard.to_json()
}

async fn bearer_command(
    router: &axum::Router,
    token: &str,
    name: &str,
    id: u32,
    params: Value,
) -> (u16, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(format!("/v1/commands/{name}"))
        .header("x-quansio-tenant", TENANT)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(
            json!({ "command_id": command_id(id), "params": params }).to_string(),
        ))
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

#[tokio::test]
async fn a_person_resolves_a_certificate_error_and_everyone_else_is_refused() {
    let Some(h) = prepare("live_cert_decision").await else {
        common::blocked_marker();
        return;
    };
    report_pending_certificate(&h.pool).await;
    let before = session_guard(&h.pool).await;
    let params = |decision: &str| json!({ "session_id": SESSION, "tab_id": TAB, "certificate_id": "crt_1", "decision": decision });

    // A viewer may look at the surface but not decide for the agent's browser.
    let (status, refused) = command(
        &h.router,
        VIEWER,
        "ResolveBrowserCertificate",
        1,
        params("allow"),
    )
    .await;
    assert_eq!(status, 403, "{refused}");

    // A service principal, even one holding the Browser/computer family, is not a person.
    let (status, created) = command(
        &h.router,
        OWNER,
        "CreateServicePrincipal",
        2,
        json!({ "name": "browser-bot", "scopes": ["Browser/computer"] }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    let token = created["result"]["secret"]
        .as_str()
        .expect("key")
        .to_string();
    let (status, refused) = bearer_command(
        &h.router,
        &token,
        "ResolveBrowserCertificate",
        3,
        params("allow"),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["code"], "SCOPE_FORBIDDEN");
    let (status, refused) = bearer_command(
        &h.router,
        &token,
        "DecideBrowserPermission",
        4,
        json!({ "session_id": SESSION, "tab_id": TAB, "origin": "https://example.com", "permission": "camera", "decision": "allow" }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["code"], "SCOPE_FORBIDDEN");

    // None of the refusals touched the row.
    assert_eq!(session_guard(&h.pool).await, before);

    // A decision that is neither allow nor deny, and a certificate that is not pending, are refused too.
    let (status, refused) = command(
        &h.router,
        OWNER,
        "ResolveBrowserCertificate",
        5,
        params("maybe"),
    )
    .await;
    assert_eq!(status, 400, "{refused}");
    let (status, refused) = command(
        &h.router,
        OWNER,
        "ResolveBrowserCertificate",
        6,
        json!({ "session_id": SESSION, "tab_id": TAB, "certificate_id": "crt_nope", "decision": "allow" }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(session_guard(&h.pool).await, before);

    // The person's decision is recorded with their identity and returns the receipt the worker acts on.
    let (status, decided) = command(
        &h.router,
        OWNER,
        "ResolveBrowserCertificate",
        7,
        params("allow"),
    )
    .await;
    assert_eq!(status, 200, "{decided}");
    let receipt = &decided["result"]["receipt"];
    assert_eq!(receipt["kind"], "certificate");
    assert_eq!(receipt["subject"], "crt_1");
    assert_eq!(receipt["decision"], "allow");
    assert_eq!(receipt["decided_by"], OWNER);
    let guard = session_guard(&h.pool).await;
    assert_eq!(guard["tabs"][0]["certificates"][0]["status"], "continued");
    assert_eq!(guard["tabs"][0]["certificates"][0]["decided_by"], OWNER);
    assert_eq!(guard["decisions"][0]["decided_by"], OWNER);
    // The surface reads the same state back.
    let (status, view) = get(&h.router, OWNER, &format!("/v1/browser-sessions/{SESSION}")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(
        view["session"]["guard"]["tabs"][0]["certificates"][0]["status"],
        "continued"
    );
    // A second answer to the same error finds nothing pending.
    let (status, again) = command(
        &h.router,
        OWNER,
        "ResolveBrowserCertificate",
        8,
        params("deny"),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn only_camera_and_microphone_for_a_well_formed_origin_can_be_granted_and_only_by_a_person() {
    let Some(h) = prepare("live_perm_decision").await else {
        common::blocked_marker();
        return;
    };
    let params = |origin: &str, permission: &str, decision: &str| json!({ "session_id": SESSION, "tab_id": TAB, "origin": origin, "permission": permission, "decision": decision });
    let (status, refused) = command(
        &h.router,
        VIEWER,
        "DecideBrowserPermission",
        2,
        params("https://example.com", "camera", "allow"),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    for (n, (origin, permission)) in [
        ("https://example.com", "geolocation"),
        ("https://example.com", "notifications"),
        ("example.com", "camera"),
        ("https://example.com/path", "microphone"),
        ("https://user@example.com", "camera"),
    ]
    .into_iter()
    .enumerate()
    {
        let n = u32::try_from(n).expect("small");
        let (status, refused) = command(
            &h.router,
            OWNER,
            "DecideBrowserPermission",
            10 + n,
            params(origin, permission, "allow"),
        )
        .await;
        assert_eq!(status, 400, "{origin} {permission}: {refused}");
    }
    assert_eq!(session_guard(&h.pool).await["decisions"], json!([]));

    let (status, granted) = command(
        &h.router,
        OWNER,
        "DecideBrowserPermission",
        4,
        params("https://example.com", "camera", "allow"),
    )
    .await;
    assert_eq!(status, 200, "{granted}");
    let receipt = &granted["result"]["receipt"];
    assert_eq!(receipt["kind"], "permission");
    assert_eq!(receipt["subject"], "https://example.com|camera");
    assert_eq!(receipt["decided_by"], OWNER);
    let (status, denied) = command(
        &h.router,
        OWNER,
        "DecideBrowserPermission",
        5,
        params("https://example.com", "microphone", "deny"),
    )
    .await;
    assert_eq!(status, 200, "{denied}");
    let guard = session_guard(&h.pool).await;
    assert_eq!(guard["decisions"].as_array().map(Vec::len), Some(2));
    assert_eq!(guard["decisions"][1]["decision"], "deny");
    common::drop_pool(&h.pool, &h.name).await;
}
