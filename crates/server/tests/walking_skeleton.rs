//! Walking-skeleton smoke test (APP-001).
//!
//! HTTP PostMessage → runtime → conformance-stub model → fs.read tool proposal →
//! Effect Ledger → read projection → WebSocket event, including reconnect-from-cursor.

use std::time::Duration;

use axum::http::Request;
use futures_util::{SinkExt, StreamExt};
use quansio_server::control::schema;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const CMD1: &str = "cmd_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const CMD2: &str = "cmd_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";

async fn prepare(prefix: &str) -> Option<(axum::Router, sqlx::PgPool, String)> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;
    let state = quansio_server::api::ApiState::new(pool.clone()).expect("api");
    Some((quansio_server::api::router(state), pool, name))
}

async fn seed_policy(pool: &sqlx::PgPool) {
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }
    ]);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0EEEEE', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
}

fn http(method: &str, path: &str, tenant: &str, body: Option<Value>) -> Request<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-quansio-tenant", tenant)
        .header("x-quansio-user", USER);
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(axum::body::Body::empty()).expect("request"),
    }
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn walking_skeleton_posts_a_message_through_the_stub_and_the_ledger() {
    let Some((router, pool, name)) = prepare("skel_turn").await else {
        common::blocked_marker();
        return;
    };
    let response = router
        .clone()
        .oneshot(http(
            "POST",
            "/v1/commands/PostMessage",
            TENANT,
            Some(json!({
                "command_id": CMD1,
                "params": { "workspace_id": WORKSPACE, "content": "hello skeleton" }
            })),
        ))
        .await
        .expect("response");
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, axum::http::StatusCode::OK, "post failed: {body}");
    assert_eq!(body["replayed"], false, "{body}");
    let thread_id = body["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let run_id = body["result"]["run_id"].as_str().expect("run").to_string();
    assert!(thread_id.starts_with("thr_"), "{body}");
    assert!(run_id.starts_with("run_"), "{body}");

    let messages = router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/messages?thread_id={thread_id}"),
            TENANT,
            None,
        ))
        .await
        .expect("messages");
    let listed = json_body(messages).await;
    assert_eq!(listed["messages"][0]["author_id"], USER, "{listed}");

    let run = router
        .clone()
        .oneshot(http("GET", &format!("/v1/runs/{run_id}"), TENANT, None))
        .await
        .expect("run");
    let run_body = json_body(run).await;
    assert_eq!(run_body["id"], run_id, "{run_body}");

    let effects = router
        .oneshot(http(
            "GET",
            &format!("/v1/effects?run_id={run_id}"),
            TENANT,
            None,
        ))
        .await
        .expect("effects");
    let effect_body = json_body(effects).await;
    assert!(
        effect_body["effects"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false),
        "expected an Effect Ledger row: {effect_body}"
    );
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn websocket_replays_from_cursor_after_reconnect() {
    let Some((router, pool, name)) = prepare("skel_ws").await else {
        common::blocked_marker();
        return;
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = reqwest::Client::new();
    let posted = client
        .post(format!("http://{addr}/v1/commands/PostMessage"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER)
        .header("content-type", "application/json")
        .body(
            json!({
                "command_id": CMD1,
                "params": { "workspace_id": WORKSPACE, "content": "stream me" }
            })
            .to_string(),
        )
        .send()
        .await
        .expect("post");
    let posted_body = posted.text().await.expect("post body");
    assert!(
        posted_body.contains("thread_id"),
        "post did not create a thread: {posted_body}"
    );

    let mut request = format!("ws://{addr}/v1/stream")
        .into_client_request()
        .expect("ws request");
    request
        .headers_mut()
        .insert("x-quansio-tenant", TENANT.parse().expect("header"));
    request
        .headers_mut()
        .insert("x-quansio-consumer", "skeleton".parse().expect("header"));
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect");
    ws.send(Message::Text(
        json!({
            "subscribe": [{ "channel": format!("workspace:{WORKSPACE}") }]
        })
        .to_string(),
    ))
    .await
    .expect("subscribe");

    let first = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("frame timeout")
        .expect("frame")
        .expect("ws");
    let Message::Text(text) = first else {
        panic!("expected text frame, got {first:?}");
    };
    let frame: Value = serde_json::from_str(&text).expect("json frame");
    assert_eq!(frame["kind"], "event", "{frame}");
    let cursor = frame["cursor"].as_str().expect("cursor").to_string();
    ws.close(None).await.ok();

    let mut request = format!("ws://{addr}/v1/stream")
        .into_client_request()
        .expect("ws request");
    request
        .headers_mut()
        .insert("x-quansio-tenant", TENANT.parse().expect("header"));
    request
        .headers_mut()
        .insert("x-quansio-consumer", "skeleton".parse().expect("header"));
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("reconnect");
    ws.send(Message::Text(
        json!({
            "subscribe": [{ "channel": format!("workspace:{WORKSPACE}") }],
            "cursor": cursor,
        })
        .to_string(),
    ))
    .await
    .expect("resubscribe");

    let posted = client
        .post(format!("http://{addr}/v1/commands/PostMessage"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER)
        .header("content-type", "application/json")
        .body(
            json!({
                "command_id": CMD2,
                "params": { "workspace_id": WORKSPACE, "content": "after reconnect" }
            })
            .to_string(),
        )
        .send()
        .await
        .expect("second post");
    assert!(posted.status().is_success(), "{}", posted.status());

    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("replay timeout")
        .expect("replay frame")
        .expect("ws");
    let Message::Text(text) = next else {
        panic!("expected text frame, got {next:?}");
    };
    let frame: Value = serde_json::from_str(&text).expect("json frame");
    assert_eq!(frame["kind"], "event", "{frame}");
    assert_ne!(frame["cursor"].as_str(), Some(cursor.as_str()), "{frame}");
    common::drop_pool(&pool, &name).await;
}
