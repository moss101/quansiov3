//! Walking-skeleton smoke test through the real intelligence plane (APP-001).
//!
//! HTTP `PostMessage` → runtime turn → Context plane (`BuildContext`) → model gateway
//! (`FulfillModel`) → the dev stack's stub provider → `fs.read` tool proposal → capability, policy
//! and the Effect Ledger → a second model call that carries the tool result → the agent's reply →
//! read projections and a WebSocket event. The model source is the deployable's
//! [`GatewayModelSource`]; the Python service runs as its own process.
//!
//! The provider is the dev stub (deterministic; the conformance-stub provider the acceptance
//! names), never real-boundary evidence. The user turn scripts its tool round with
//! `[[stub:call fs__read {...}]]`, which the stub honours only when that tool is offered — so the
//! round proves the capability-filtered tool list reached the provider under its wire name.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `uv`, and the stub provider at
//! `QUANSIO_TEST_STUB_PROVIDER_URL` (default `http://127.0.0.1:59020`); without them the test
//! prints a `BLOCKED_EXTERNAL` marker and returns.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use quansio_server::control::schema;
use quansio_server::intelligence::model_source::{GatewayModelSource, ModelCallSettings};
use quansio_server::intelligence::IntelligenceClient;
use quansio_server::runtime::context_bridge::GatewayContextBridge;
use serde_json::{json, Value};
use sqlx::Row;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YAPP001";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YAPP001";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YAPP001";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YAPP001";
const COMMAND: &str = "cmd_01J8Z3K6F1N8VQ2X5W9YAPP001";
const COMMAND_UNOFFERED: &str = "cmd_01J8Z3K6F1N8VQ2X5W9YAPP002";

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
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YAPP001', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
}

async fn model_calls(pool: &sqlx::PgPool, run_id: &str) -> Vec<(String, String, String, String)> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query(
        "SELECT r.provider, r.model_id, c.route_id, c.context_projection_id \
         FROM model_calls c JOIN model_routes r ON r.id = c.route_id \
         WHERE c.tenant_id = $1 AND c.run_id = $2 ORDER BY c.created_at",
    )
    .bind(TENANT)
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await
    .expect("model calls");
    tx.commit().await.expect("commit");
    rows.iter()
        .map(|row| {
            (
                row.get("provider"),
                row.get("model_id"),
                row.get("route_id"),
                row.get::<Option<String>, _>("context_projection_id")
                    .unwrap_or_default(),
            )
        })
        .collect()
}

async fn tool_calls(pool: &sqlx::PgPool, run_id: &str) -> Vec<(String, String, Value)> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query(
        "SELECT c.tool_name, c.status, c.args FROM tool_calls c \
         JOIN steps s ON s.id = c.step_id JOIN turns t ON t.id = s.turn_id \
         WHERE c.tenant_id = $1 AND t.run_id = $2",
    )
    .bind(TENANT)
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await
    .expect("tool calls");
    tx.commit().await.expect("commit");
    rows.iter()
        .map(|row| (row.get("tool_name"), row.get("status"), row.get("args")))
        .collect()
}

#[tokio::test]
async fn a_chat_turn_runs_through_the_context_plane_the_gateway_and_the_ledger() {
    let Some(service) = common::intelligence::start().await else {
        return;
    };
    let name = common::scratch_name("skel_gateway");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;

    // The deployable's composition: the gateway model source over the Python process.
    let settings = ModelCallSettings::load().expect("config/intelligence.yaml");
    let client = IntelligenceClient::connect_lazy(&service.endpoint).expect("endpoint");
    let context = Arc::new(GatewayContextBridge::new(client.clone(), settings.deadline));
    let state = quansio_server::api::ApiState::new(pool.clone())
        .expect("api")
        .with_model_source(Arc::new(GatewayModelSource::new(
            pool.clone(),
            client,
            context,
            settings,
        )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = quansio_server::api::router(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });

    let http = reqwest::Client::new();
    let post = |command: &'static str, content: &'static str| {
        http.post(format!("http://{addr}/v1/commands/PostMessage"))
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", USER)
            .json(&json!({
                "command_id": command,
                "params": { "workspace_id": WORKSPACE, "content": content }
            }))
            .send()
    };
    let response = post(
        COMMAND,
        "Read the notes. [[stub:call fs__read {\"path\": \"/work/root/notes.md\"}]]",
    )
    .await
    .expect("post");
    let status = response.status();
    let body: Value = response.json().await.expect("json");
    assert_eq!(status, 200, "{body}");
    let run_id = body["result"]["run_id"].as_str().expect("run").to_string();
    let thread_id = body["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    assert!(
        body["result"]["reply_message_id"].is_string(),
        "the turn completed with a reply: {body}"
    );

    // The tool round: the model proposed fs.read, the runtime dispatched it through the ledger.
    let calls = tool_calls(&pool, &run_id).await;
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0].0, "fs.read",
        "the registry name, decoded from the wire"
    );
    assert_eq!(calls[0].1, "completed", "{calls:?}");
    assert_eq!(calls[0].2, json!({"path": "/work/root/notes.md"}));
    let effects: Value = http
        .get(format!("http://{addr}/v1/effects?run_id={run_id}"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER)
        .send()
        .await
        .expect("effects")
        .json()
        .await
        .expect("json");
    assert!(
        effects["effects"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "an Effect Ledger row: {effects}"
    );

    // Two model calls, both through the gateway's route and each rendered from a projection;
    // the second carried the tool result, which the stub counts in its reply.
    let recorded = model_calls(&pool, &run_id).await;
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    for (provider, model, route, projection) in &recorded {
        assert_eq!(provider, "openai-compatible");
        assert_eq!(model, common::intelligence::STUB_MODEL);
        assert!(route.starts_with("mr_"), "{route}");
        assert!(projection.starts_with("ctx_"), "{projection}");
    }
    assert_ne!(
        recorded[0].3, recorded[1].3,
        "each call has its own projection"
    );
    let messages: Value = http
        .get(format!("http://{addr}/v1/messages?thread_id={thread_id}"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER)
        .send()
        .await
        .expect("messages")
        .json()
        .await
        .expect("json");
    let reply = &messages["messages"][1];
    assert_eq!(reply["author_kind"], "agent", "{messages}");
    assert_eq!(
        reply["content_blocks"][0]["text"], "stub completion: 1 tool result(s) received",
        "{messages}"
    );

    // The turn's events reach a WebSocket subscriber.
    let mut request = format!("ws://{addr}/v1/stream")
        .into_client_request()
        .expect("ws request");
    request
        .headers_mut()
        .insert("x-quansio-tenant", TENANT.parse().expect("header"));
    request.headers_mut().insert(
        "x-quansio-consumer",
        "skeleton-gateway".parse().expect("header"),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect");
    ws.send(Message::Text(
        json!({ "subscribe": [{ "channel": format!("workspace:{WORKSPACE}") }] }).to_string(),
    ))
    .await
    .expect("subscribe");
    let mut types = Vec::new();
    while let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(Duration::from_secs(3), ws.next()).await
    {
        let frame: Value = serde_json::from_str(&text).expect("frame");
        if frame["kind"] == "event" {
            types.push(frame.to_string());
        }
    }
    assert!(
        types.iter().any(|frame| frame.contains(&run_id)),
        "the run's events are streamed: {types:?}"
    );

    // A scripted call for a tool the run was not offered is not made: the stub only answers
    // with calls to offered tools, so the turn ends in text and no tool call is recorded.
    let response = post(
        COMMAND_UNOFFERED,
        "Write it. [[stub:call fs__write {\"path\": \"/work/root/out.txt\"}]]",
    )
    .await
    .expect("post");
    let body: Value = response.json().await.expect("json");
    let run_id = body["result"]["run_id"].as_str().expect("run").to_string();
    assert!(tool_calls(&pool, &run_id).await.is_empty(), "{body}");
    assert_eq!(model_calls(&pool, &run_id).await.len(), 1, "{body}");

    common::drop_pool(&pool, &name).await;
    drop(service);
}

#[tokio::test]
async fn a_provider_failure_fails_the_turn_with_its_code() {
    // A provider nobody listens on: the gateway's stream ends in a typed error event.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let provider = format!("http://{}", dead.local_addr().expect("addr"));
    drop(dead);
    let Some(service) = common::intelligence::start_with_provider(Some(&provider)).await else {
        return;
    };
    let name = common::scratch_name("skel_gateway_fail");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;
    let settings = ModelCallSettings::load().expect("config/intelligence.yaml");
    let client = IntelligenceClient::connect_lazy(&service.endpoint).expect("endpoint");
    let context = Arc::new(GatewayContextBridge::new(client.clone(), settings.deadline));
    let router = quansio_server::api::router(
        quansio_server::api::ApiState::new(pool.clone())
            .expect("api")
            .with_model_source(Arc::new(GatewayModelSource::new(
                pool.clone(),
                client,
                context,
                settings,
            ))),
    );
    let response = tower::ServiceExt::oneshot(
        router,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/commands/PostMessage")
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", USER)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "command_id": COMMAND,
                    "params": { "workspace_id": WORKSPACE, "content": "hello" }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await
    .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        !status.is_success(),
        "a failed model call must not succeed: {body}"
    );
    assert_eq!(body["code"], "PROVIDER_UNAVAILABLE", "{body}");
    assert_eq!(body["retryable"], true, "{body}");

    // The model step failed; no reply was invented.
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let steps: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, status FROM steps WHERE tenant_id = $1 ORDER BY created_at")
            .bind(TENANT)
            .fetch_all(&mut *tx)
            .await
            .expect("steps");
    let agent_messages: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE tenant_id = $1 AND author_kind = 'agent'",
    )
    .bind(TENANT)
    .fetch_one(&mut *tx)
    .await
    .expect("messages");
    tx.commit().await.expect("commit");
    assert_eq!(
        steps,
        vec![("model_call".to_string(), "failed".to_string())]
    );
    assert_eq!(agent_messages, 0);
    common::drop_pool(&pool, &name).await;
    drop(service);
}
