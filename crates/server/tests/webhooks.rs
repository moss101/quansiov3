//! APP-015: the public event stream and outbound webhooks, against a scratch database
//! (blocked, not skipped, without one) and a real local HTTP receiver.
//!
//! * delivery/retry — a delivery is signed, retried with backoff under the same event id,
//!   dead-lettered after its attempts (the subscription turns `failing`), redelivered on
//!   command, and attempted again after an instance died holding its lease;
//! * tenant scope — a subscription receives its tenant's events only, a workspace
//!   subscription its workspace's only, never a protected thread's; another tenant can
//!   neither see nor change it, and only administrators manage webhooks;
//! * redaction scan — secrets, credential-shaped text, emails and protected data classes never
//!   reach a receiver, and the signing secret is kept nowhere but its sealed handle;
//! * egress deny — no network rule, a denying rule, a blocked host, a disallowed address and a
//!   name that resolves to one are all refused; pausing a subscription and denying egress
//!   tenant-wide (the kill switches) stop delivery, queued retries included;
//! * the SSE form of the stream carries the same frames with the cursor as `id:`.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (an admin URL for scratch databases).

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::Request;
use chrono::{Duration as ChronoDuration, Utc};
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventStore, EventType};
use quansio_machine::secrets::{KeyProvider, LocalMasterKeyProvider};
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use quansio_server::notify::webhooks::{
    verify, HostResolver, WebhookDeliverer, WebhookFanout, MAX_WEBHOOK_ATTEMPTS,
};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT_A: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0WHKA1";
const OWNER_A: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0WHKA2";
const WS_A1: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0WHKA3";
const WS_A2: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0WHKA4";
const NODE_A: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0WHKA5";
const VIEWER_A: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0WHKA6";
const POLICY_A: &str = "pol_01J8Z3K6F1N8VQ2X5W9Y0WHKA7";
const PROTECTED_THREAD: &str = "thr_01J8Z3K6F1N8VQ2X5W9Y0WHKA8";
const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0WHKB1";
const OWNER_B: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0WHKB2";
const WS_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0WHKB3";
const NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0WHKB4";
const POLICY_B: &str = "pol_01J8Z3K6F1N8VQ2X5W9Y0WHKB5";
const CANARY: &str = "qncy_test_canary_not_for_prod";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0W{n:04}")
}

fn network_rule(decision: &str, classes: &[&str]) -> Value {
    json!({
        "effect_class": "network.egress.new_destination",
        "resource_selector": { "kind": "domain", "selector": "*" },
        "decision": decision,
        "conditions": { "allowed_address_classes": classes },
    })
}

fn read_rule() -> Value {
    json!({ "effect_class": "read.internal", "resource_selector": { "kind": "fs", "selector": "**" }, "decision": "allow" })
}

fn keys() -> Arc<dyn KeyProvider> {
    Arc::new(LocalMasterKeyProvider::from_master_key(&[7u8; 32], "local:test-v1").expect("key"))
}

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

struct Harness {
    router: axum::Router,
    pool: sqlx::PgPool,
    name: String,
}

async fn seed_policy(pool: &sqlx::PgPool, tenant: &str, id: &str, rules: Value) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, tenant)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) VALUES ($1, $2, NULL, 'tenant', $3)",
    )
    .bind(id)
    .bind(tenant)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
}

/// Two tenants; tenant A has two workspaces, a viewer and a protected thread. Both tenants'
/// policies let webhooks reach public and loopback addresses (the receiver is local) unless
/// `rules_a` says otherwise.
async fn prepare(prefix: &str, rules_a: Value, signing: bool) -> Option<Harness> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT_A, OWNER_A, WS_A1, NODE_A).await;
    common::seed_tenant(&pool, TENANT_B, OWNER_B, WS_B, NODE_B).await;
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'second')")
        .bind(WS_A2)
        .bind(TENANT_A)
        .execute(&mut *tx)
        .await
        .expect("workspace");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'viewer')")
        .bind(VIEWER_A)
        .bind(format!("{VIEWER_A}@example.com"))
        .execute(&mut *tx)
        .await
        .expect("viewer");
    schema::set_tenant_context(&mut tx, TENANT_A)
        .await
        .expect("tenant");
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, 'member', 'active')")
        .bind(format!("tm_{}", &VIEWER_A[4..]))
        .bind(TENANT_A)
        .bind(VIEWER_A)
        .execute(&mut *tx)
        .await
        .expect("tenant membership");
    sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ($1, $2, $3, $4, 'viewer', 'active')")
        .bind(format!("wm_{}", &VIEWER_A[4..]))
        .bind(TENANT_A)
        .bind(WS_A1)
        .bind(VIEWER_A)
        .execute(&mut *tx)
        .await
        .expect("workspace membership");
    // A direct thread with participants is protected (APP-009).
    sqlx::query("INSERT INTO threads (id, tenant_id, workspace_id, kind, title) VALUES ($1, $2, $3, 'direct', 'private')")
        .bind(PROTECTED_THREAD)
        .bind(TENANT_A)
        .bind(WS_A1)
        .execute(&mut *tx)
        .await
        .expect("thread");
    sqlx::query("INSERT INTO thread_participants (id, tenant_id, thread_id, kind, principal_id, role) VALUES ('thp_01J8Z3K6F1N8VQ2X5W9Y0WHKA9', $1, $2, 'user', $3, 'owner')")
        .bind(TENANT_A)
        .bind(PROTECTED_THREAD)
        .bind(OWNER_A)
        .execute(&mut *tx)
        .await
        .expect("participant");
    tx.commit().await.expect("commit");
    seed_policy(&pool, TENANT_A, POLICY_A, rules_a).await;
    seed_policy(
        &pool,
        TENANT_B,
        POLICY_B,
        json!([read_rule(), network_rule("allow", &["public", "loopback"])]),
    )
    .await;
    let mut state = ApiState::new(pool.clone()).expect("api");
    if signing {
        state = state.with_key_provider(keys());
    }
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
    })
}

fn loopback_rules() -> Value {
    json!([read_rule(), network_rule("allow", &["public", "loopback"])])
}

fn http(
    method: &str,
    path: &str,
    tenant: &str,
    user: &str,
    body: Option<Value>,
) -> Request<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-quansio-tenant", tenant)
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
    h: &Harness,
    tenant: &str,
    user: &str,
    name: &str,
    id: u32,
    params: Value,
) -> (u16, Value) {
    let response = h
        .router
        .clone()
        .oneshot(http(
            "POST",
            &format!("/v1/commands/{name}"),
            tenant,
            user,
            Some(json!({ "command_id": command_id(id), "params": params })),
        ))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn get(h: &Harness, tenant: &str, user: &str, path: &str) -> (u16, Value) {
    let response = h
        .router
        .clone()
        .oneshot(http("GET", path, tenant, user, None))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

/// Commit one event the way an owner would: the fan-out consumes whatever the log holds.
async fn emit(
    pool: &sqlx::PgPool,
    tenant: &str,
    workspace: Option<&str>,
    aggregate: (&str, &str),
    event_type: &str,
    payload: Value,
) -> String {
    let (aggregate_type, aggregate_id) = (aggregate.0.to_string(), aggregate.1.to_string());
    let workspace = workspace.map(str::to_string);
    let event_type = event_type.to_string();
    EventStore::new(pool.clone())
        .commit_mutation(tenant, move |conn, batch| {
            Box::pin(async move {
                let version: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(MAX(aggregate_version), 0) + 1 FROM runtime_events \
                     WHERE aggregate_type = $1 AND aggregate_id = $2",
                )
                .bind(&aggregate_type)
                .bind(&aggregate_id)
                .fetch_one(&mut *conn)
                .await?;
                let mut draft = EventDraft::new(
                    aggregate_type,
                    aggregate_id,
                    u64::try_from(version).unwrap_or(1),
                    EventType::parse(&event_type).expect("event type"),
                    CorrelationId::generate(&mut UlidGenerator::new()),
                    Actor::system("test"),
                )
                .with_payload(payload);
                if let Some(workspace) = workspace {
                    draft = draft.with_workspace(workspace);
                }
                let id = draft.event_id.to_string();
                batch.emit(draft);
                Ok(id)
            })
        })
        .await
        .expect("event")
}

async fn scalar(pool: &sqlx::PgPool, sql: &str, binds: &[&str]) -> Value {
    let mut query = sqlx::query_scalar::<_, Value>(sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.fetch_one(pool).await.expect("scalar")
}

async fn delivery(pool: &sqlx::PgPool, subscription: &str, event_id: &str) -> Value {
    scalar(
        pool,
        "SELECT to_jsonb(d) FROM webhook_deliveries d WHERE subscription_id = $1 AND event_id = $2",
        &[subscription, event_id],
    )
    .await
}

// ---------------------------------------------------------------------------------------
// The receiver: a real HTTP server on loopback that records what arrives
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Received {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Default)]
struct Receiver {
    received: Arc<Mutex<Vec<Received>>>,
    answers: Arc<Mutex<VecDeque<u16>>>,
    always: Arc<Mutex<Option<u16>>>,
}

impl Receiver {
    async fn start() -> (Self, u16) {
        let receiver = Self::default();
        let state = receiver.clone();
        let app = axum::Router::new().fallback(move |request: Request<axum::body::Body>| {
            let state = state.clone();
            async move {
                let path = request.uri().path().to_string();
                let headers = request
                    .headers()
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.as_str().to_string(),
                            value.to_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect();
                let bytes = axum::body::to_bytes(request.into_body(), 1 << 22)
                    .await
                    .unwrap_or_default();
                state.received.lock().expect("lock").push(Received {
                    path,
                    headers,
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                });
                let status = state
                    .answers
                    .lock()
                    .expect("lock")
                    .pop_front()
                    .or(*state.always.lock().expect("lock"))
                    .unwrap_or(200);
                axum::http::StatusCode::from_u16(status).expect("status")
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        (receiver, port)
    }

    fn answer(&self, statuses: &[u16]) {
        self.answers.lock().expect("lock").extend(statuses);
    }

    fn always(&self, status: Option<u16>) {
        *self.always.lock().expect("lock") = status;
    }

    fn received(&self) -> Vec<Received> {
        self.received.lock().expect("lock").clone()
    }

    fn at(&self, path: &str) -> Vec<Received> {
        self.received()
            .into_iter()
            .filter(|received| received.path == path)
            .collect()
    }
}

fn deliverer(pool: &sqlx::PgPool, tenant: &str) -> WebhookDeliverer {
    WebhookDeliverer::new(pool.clone(), tenant, Some(keys()))
        .with_timeout(std::time::Duration::from_secs(3))
}

async fn fan_out(pool: &sqlx::PgPool, tenant: &str) -> usize {
    WebhookFanout::new(pool.clone(), tenant)
        .scan(512)
        .await
        .expect("fan-out")
}

async fn create(h: &Harness, tenant: &str, user: &str, id: u32, params: Value) -> (String, String) {
    let (status, body) = command(h, tenant, user, "CreateWebhookSubscription", id, params).await;
    assert_eq!(status, 200, "{body}");
    let result = &body["result"];
    (
        result["subscription"]["id"]
            .as_str()
            .expect("id")
            .to_string(),
        result["secret"].as_str().expect("secret").to_string(),
    )
}

// ---------------------------------------------------------------------------------------
// Delivery and retry
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn delivery_is_signed_at_least_once_and_retried_until_dead_lettered() {
    let Some(h) = prepare("whk_delivery", loopback_rules(), true).await else {
        common::blocked_marker();
        return;
    };
    let (receiver, port) = Receiver::start().await;
    let url = format!("http://127.0.0.1:{port}/hook");
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "CreateWebhookSubscription",
        1,
        json!({ "url": url, "workspace_id": WS_A1, "event_types": ["run.*"] }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let sub = body["result"]["subscription"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let secret = body["result"]["secret"]
        .as_str()
        .expect("secret")
        .to_string();
    assert!(secret.starts_with("whsec_"));
    assert_eq!(
        body["result"]["subscription"]["destination"],
        format!("127.0.0.1:{port}")
    );
    // The secret is returned once: a replayed command does not return it again.
    let (_, replay) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "CreateWebhookSubscription",
        1,
        json!({ "url": url, "workspace_id": WS_A1, "event_types": ["run.*"] }),
    )
    .await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"]["secret"], Value::Null);
    assert_eq!(replay["result"]["secret_returned_once"], true);

    let run = "run_01J8Z3K6F1N8VQ2X5W9Y0WHKC1";
    let succeeded = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", run),
        "run.succeeded",
        json!({ "run_id": run }),
    )
    .await;
    emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("thread", "thr_01J8Z3K6F1N8VQ2X5W9Y0WHKC2"),
        "thread.created",
        json!({}),
    )
    .await;
    emit(
        &h.pool,
        TENANT_A,
        Some(WS_A2),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKC3"),
        "run.succeeded",
        json!({}),
    )
    .await;

    // One delivery, enqueued once however often the log is consumed.
    assert_eq!(fan_out(&h.pool, TENANT_A).await, 1);
    assert_eq!(fan_out(&h.pool, TENANT_A).await, 0);
    sqlx::query("DELETE FROM event_cursors WHERE tenant_id = $1 AND consumer = 'webhook-fanout'")
        .bind(TENANT_A)
        .execute(&h.pool)
        .await
        .expect("lose the cursor");
    assert_eq!(
        fan_out(&h.pool, TENANT_A).await,
        0,
        "a replayed log enqueues nothing new"
    );

    // First attempt fails; the retry waits for its backoff; the second attempt delivers.
    receiver.answer(&[500]);
    let now = Utc::now();
    let first = deliverer(&h.pool, TENANT_A)
        .deliver_due(now, 32)
        .await
        .expect("deliver");
    assert_eq!((first.attempted, first.retried), (1, 1));
    let early = deliverer(&h.pool, TENANT_A)
        .deliver_due(now + ChronoDuration::seconds(5), 32)
        .await
        .expect("deliver");
    assert_eq!(early.attempted, 0, "a retry waits for its backoff");
    let second = deliverer(&h.pool, TENANT_A)
        .deliver_due(now + ChronoDuration::seconds(45), 32)
        .await
        .expect("deliver");
    assert_eq!((second.attempted, second.delivered), (1, 1));

    let got = receiver.at("/hook");
    assert_eq!(
        got.len(),
        2,
        "at least once: the failed attempt and the retry"
    );
    for request in &got {
        assert_eq!(request.header("quansio-event-id"), Some(succeeded.as_str()));
        assert_eq!(request.header("quansio-event-type"), Some("run.succeeded"));
        let signature = request.header("quansio-signature").expect("signed");
        verify(
            secret.as_bytes(),
            signature,
            request.body.as_bytes(),
            Utc::now().timestamp(),
            300,
        )
        .expect("the signature verifies with the subscription's secret");
        assert!(verify(
            b"whsec_wrong",
            signature,
            request.body.as_bytes(),
            Utc::now().timestamp(),
            300
        )
        .is_err());
        let envelope: Value = serde_json::from_str(&request.body).expect("json");
        assert_eq!(
            envelope["event_id"],
            succeeded.as_str(),
            "consumers dedupe on the stable id"
        );
        assert_eq!(envelope["tenant_id"], TENANT_A);
        assert_eq!(envelope["workspace_id"], WS_A1);
    }
    assert_eq!(
        got[0].body, got[1].body,
        "a retry re-sends the same envelope"
    );
    let row = delivery(&h.pool, &sub, &succeeded).await;
    assert_eq!(row["status"], "delivered");
    assert_eq!(row["attempts"], 2);
    assert_eq!(row["last_status"], "http_200");

    // The projection.
    let (status, deliveries) = get(
        &h,
        TENANT_A,
        OWNER_A,
        &format!("/v1/webhooks/{sub}/deliveries"),
    )
    .await;
    assert_eq!(status, 200, "{deliveries}");
    assert_eq!(deliveries["items"][0]["status"], "delivered");
    let (_, list) = get(
        &h,
        TENANT_A,
        OWNER_A,
        &format!("/v1/webhooks?workspace_id={WS_A1}"),
    )
    .await;
    assert_eq!(list["items"][0]["id"], sub.as_str());
    assert_eq!(list["items"][0]["deliveries"]["delivered"], 1);
    assert!(
        !list.to_string().contains(&secret),
        "the projection never shows the secret"
    );

    // Out of attempts: dead-lettered, and the subscription is failing.
    receiver.always(Some(503));
    let failing = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", run),
        "run.failed",
        json!({}),
    )
    .await;
    assert_eq!(fan_out(&h.pool, TENANT_A).await, 1);
    let mut at = Utc::now();
    for attempt in 1..=MAX_WEBHOOK_ATTEMPTS {
        let report = deliverer(&h.pool, TENANT_A)
            .deliver_due(at, 32)
            .await
            .expect("deliver");
        assert_eq!(report.attempted, 1, "attempt {attempt}");
        at += ChronoDuration::hours(2);
    }
    let row = delivery(&h.pool, &sub, &failing).await;
    assert_eq!(row["status"], "dead_lettered");
    assert_eq!(row["attempts"], MAX_WEBHOOK_ATTEMPTS);
    assert_eq!(row["last_status"], "http_503");
    assert_eq!(
        scalar(
            &h.pool,
            "SELECT to_jsonb(status) FROM webhook_subscriptions WHERE id = $1",
            &[&sub]
        )
        .await,
        "failing"
    );
    let dead: i64 = sqlx::query_scalar("SELECT count(*) FROM runtime_events WHERE type = 'webhook.delivery_dead_lettered' AND aggregate_id = $1")
        .bind(&sub)
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(dead, 1);
    assert_eq!(
        deliverer(&h.pool, TENANT_A)
            .deliver_due(at, 32)
            .await
            .expect("deliver")
            .attempted,
        0
    );
    // Webhook events are never delivered (a delivery must not cause one).
    fan_out(&h.pool, TENANT_A).await;
    let webhook_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_deliveries WHERE event_type LIKE 'webhook.%'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("count");
    assert_eq!(webhook_rows, 0);

    // Redelivery: re-queued, delivered, and the subscription recovers.
    receiver.always(None);
    let dead_id = row["id"].as_str().expect("id").to_string();
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "RedeliverWebhook",
        2,
        json!({ "delivery_id": dead_id }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["delivery"]["status"], "pending");
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "RedeliverWebhook",
        3,
        json!({ "delivery_id": dead_id }),
    )
    .await;
    assert_eq!(
        status, 409,
        "only a dead-lettered delivery is redelivered: {body}"
    );
    let report = deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    assert_eq!(report.delivered, 1);
    assert_eq!(
        delivery(&h.pool, &sub, &failing).await["status"],
        "delivered"
    );
    assert_eq!(
        scalar(
            &h.pool,
            "SELECT to_jsonb(status) FROM webhook_subscriptions WHERE id = $1",
            &[&sub]
        )
        .await,
        "active"
    );

    // An instance died holding a lease: the delivery is attempted again once the lease lapses.
    let crashed = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", run),
        "run.cancelled",
        json!({}),
    )
    .await;
    assert_eq!(fan_out(&h.pool, TENANT_A).await, 1);
    sqlx::query(
        "UPDATE webhook_deliveries SET status = 'delivering', attempts = 1, \
         lease_until = now() + interval '60 seconds' WHERE event_id = $1",
    )
    .bind(&crashed)
    .execute(&h.pool)
    .await
    .expect("a lease held by a dead instance");
    let before = receiver.at("/hook").len();
    assert_eq!(
        deliverer(&h.pool, TENANT_A)
            .deliver_due(Utc::now(), 32)
            .await
            .expect("deliver")
            .attempted,
        0,
        "the lease is still held"
    );
    let report = deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now() + ChronoDuration::seconds(120), 32)
        .await
        .expect("deliver");
    assert_eq!(report.delivered, 1);
    let row = delivery(&h.pool, &sub, &crashed).await;
    assert_eq!(row["status"], "delivered");
    assert_eq!(row["attempts"], 2);
    assert_eq!(receiver.at("/hook").len(), before + 1);

    // Rotating the secret: later deliveries verify with the new one only.
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "UpdateWebhookSubscription",
        4,
        json!({ "subscription_id": sub, "secret": "rotate" }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let rotated = body["result"]["secret"]
        .as_str()
        .expect("secret")
        .to_string();
    assert_ne!(rotated, secret);
    emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", run),
        "run.resumed",
        json!({}),
    )
    .await;
    fan_out(&h.pool, TENANT_A).await;
    deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    let last = receiver.at("/hook").pop().expect("delivered");
    let signature = last.header("quansio-signature").expect("signed");
    assert!(verify(
        rotated.as_bytes(),
        signature,
        last.body.as_bytes(),
        Utc::now().timestamp(),
        300
    )
    .is_ok());
    assert!(verify(
        secret.as_bytes(),
        signature,
        last.body.as_bytes(),
        Utc::now().timestamp(),
        300
    )
    .is_err());

    // Deleting revokes the grant and its secret.
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "DeleteWebhookSubscription",
        5,
        json!({ "subscription_id": sub }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let revoked: String = sqlx::query_scalar(
        "SELECT status FROM secret_handles WHERE provider = 'webhook' AND tenant_id = $1",
    )
    .bind(TENANT_A)
    .fetch_one(&h.pool)
    .await
    .expect("handle");
    assert_eq!(revoked, "revoked");
    common::drop_pool(&h.pool, &h.name).await;
}

// ---------------------------------------------------------------------------------------
// Tenant scope
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn deliveries_stay_in_their_tenant_and_workspace() {
    let Some(h) = prepare("whk_scope", loopback_rules(), true).await else {
        common::blocked_marker();
        return;
    };
    let (receiver, port) = Receiver::start().await;
    let base = format!("http://127.0.0.1:{port}");
    let (workspace_sub, _) = create(
        &h,
        TENANT_A,
        OWNER_A,
        1,
        json!({ "url": format!("{base}/a1"), "workspace_id": WS_A1 }),
    )
    .await;
    let (tenant_sub, _) = create(
        &h,
        TENANT_A,
        OWNER_A,
        2,
        json!({ "url": format!("{base}/a") }),
    )
    .await;
    let (other_sub, _) = create(
        &h,
        TENANT_B,
        OWNER_B,
        101,
        json!({ "url": format!("{base}/b") }),
    )
    .await;

    let in_a1 = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKD1"),
        "run.succeeded",
        json!({}),
    )
    .await;
    let in_a2 = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A2),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKD2"),
        "run.succeeded",
        json!({}),
    )
    .await;
    let in_b = emit(
        &h.pool,
        TENANT_B,
        Some(WS_B),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKD3"),
        "run.succeeded",
        json!({}),
    )
    .await;
    // A protected thread's content is its participants', never an integrator's.
    let private = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("thread", PROTECTED_THREAD),
        "thread.message_posted",
        json!({ "body": "between us" }),
    )
    .await;
    // A notification is its recipient's alone.
    let notification = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("notification", "ntf_01J8Z3K6F1N8VQ2X5W9Y0WHKD4"),
        "notification.created",
        json!({ "user_id": OWNER_A }),
    )
    .await;

    fan_out(&h.pool, TENANT_A).await;
    fan_out(&h.pool, TENANT_B).await;
    deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    deliverer(&h.pool, TENANT_B)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");

    let ids = |path: &str| -> Vec<String> {
        receiver
            .at(path)
            .iter()
            .map(|received| {
                received
                    .header("quansio-event-id")
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    };
    assert_eq!(
        ids("/a1"),
        vec![in_a1.clone()],
        "a workspace subscription: its workspace only"
    );
    let mut tenant_wide = ids("/a");
    tenant_wide.sort();
    let mut expected = vec![in_a1.clone(), in_a2.clone()];
    expected.sort();
    assert_eq!(
        tenant_wide, expected,
        "a tenant subscription: every workspace of its tenant"
    );
    assert_eq!(
        ids("/b"),
        vec![in_b.clone()],
        "another tenant's events never cross"
    );
    for received in receiver.received() {
        let body: Value = serde_json::from_str(&received.body).expect("json");
        let expected_tenant = if received.path == "/b" {
            TENANT_B
        } else {
            TENANT_A
        };
        assert_eq!(body["tenant_id"], expected_tenant);
        for withheld in [&private, &notification] {
            assert_ne!(received.header("quansio-event-id"), Some(withheld.as_str()));
        }
    }
    assert!(!receiver
        .received()
        .iter()
        .any(|received| received.body.contains("between us")));

    // Tenant B can neither see nor change tenant A's subscriptions.
    let (_, listed) = get(&h, TENANT_B, OWNER_B, "/v1/webhooks").await;
    let listed: Vec<&str> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(listed, vec![other_sub.as_str()]);
    let (status, body) = command(
        &h,
        TENANT_B,
        OWNER_B,
        "UpdateWebhookSubscription",
        102,
        json!({ "subscription_id": tenant_sub, "status": "paused" }),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    let (status, _) = get(
        &h,
        TENANT_B,
        OWNER_B,
        &format!("/v1/webhooks/{workspace_sub}/deliveries"),
    )
    .await;
    assert_eq!(status, 404);

    // Only administrators manage webhooks.
    let (status, body) = command(
        &h,
        TENANT_A,
        VIEWER_A,
        "CreateWebhookSubscription",
        3,
        json!({ "url": format!("{base}/v"), "workspace_id": WS_A1 }),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, _) = get(
        &h,
        TENANT_A,
        VIEWER_A,
        &format!("/v1/webhooks?workspace_id={WS_A1}"),
    )
    .await;
    assert_eq!(status, 403);
    let (status, body) = command(
        &h,
        TENANT_A,
        VIEWER_A,
        "CreateWebhookSubscription",
        4,
        json!({ "url": format!("{base}/v") }),
    )
    .await;
    assert_eq!(
        status, 403,
        "a tenant-wide subscription is the tenant administrator's: {body}"
    );
    common::drop_pool(&h.pool, &h.name).await;
}

// ---------------------------------------------------------------------------------------
// Redaction scan
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn payloads_never_carry_secrets_or_protected_data() {
    let Some(h) = prepare("whk_redact", loopback_rules(), true).await else {
        common::blocked_marker();
        return;
    };
    let (receiver, port) = Receiver::start().await;
    let (_, secret) = create(
        &h,
        TENANT_A,
        OWNER_A,
        1,
        json!({ "url": format!("http://127.0.0.1:{port}/scan") }),
    )
    .await;
    let sensitive = [
        CANARY,
        "sk-live-4f9a8b7c6d5e",
        "ghp_0123456789abcdefABCDEF",
        "ada@example.com",
        "hunter2-password-value",
        "tok_live_refresh_ABC123",
        "Payroll of Ada Lovelace",
        "4111 1111 1111 1111",
    ];
    emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("artifact", "art_01J8Z3K6F1N8VQ2X5W9Y0WHKE1"),
        "artifact.created",
        json!({
            "artifact_id": "art_01J8Z3K6F1N8VQ2X5W9Y0WHKE1",
            "summary": format!("uploaded by ada@example.com with {CANARY}"),
            "headers": { "Authorization": "Bearer sk-live-4f9a8b7c6d5e", "cookie": "session=abc" },
            "connector": { "password": "hunter2-password-value", "refresh_token": "tok_live_refresh_ABC123" },
            "note": "token ghp_0123456789abcdefABCDEF pasted by mistake",
            "classified": {
                "id": "art_01J8Z3K6F1N8VQ2X5W9Y0WHKE1",
                "data_classes": ["pii", "financial"],
                "title": "Payroll of Ada Lovelace",
                "card": "4111 1111 1111 1111",
                "status": "ready",
            },
        }),
    )
    .await;
    fan_out(&h.pool, TENANT_A).await;
    let report = deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    assert_eq!(report.delivered, 1);
    let got = receiver.at("/scan");
    assert_eq!(got.len(), 1);
    let request = &got[0];
    let everything = format!("{:?}{}", request.headers, request.body);
    for value in sensitive {
        assert!(
            !everything.contains(value),
            "{value:?} reached the receiver: {everything}"
        );
    }
    assert!(
        !everything.contains(&secret),
        "the signing secret is never sent"
    );
    let body: Value = serde_json::from_str(&request.body).expect("json");
    assert_eq!(
        body["payload"]["classified"]["status"], "ready",
        "identifiers and status stay"
    );
    assert_eq!(
        body["payload"]["classified"]["id"],
        "art_01J8Z3K6F1N8VQ2X5W9Y0WHKE1"
    );
    assert_eq!(
        body["payload"]["artifact_id"],
        "art_01J8Z3K6F1N8VQ2X5W9Y0WHKE1"
    );

    // The secret exists only sealed: not in the command log, the events, the audit trail or
    // the handle's ciphertext.
    for (what, sql) in [
        (
            "commands",
            "SELECT COALESCE(string_agg(result::text, ''), '') FROM commands",
        ),
        (
            "runtime_events",
            "SELECT COALESCE(string_agg(payload::text, ''), '') FROM runtime_events",
        ),
        (
            "audit_entries",
            "SELECT COALESCE(string_agg(to_jsonb(a)::text, ''), '') FROM audit_entries a",
        ),
        (
            "secret_handles",
            "SELECT COALESCE(string_agg(encode(ciphertext, 'escape'), ''), '') FROM secret_handles",
        ),
        (
            "webhook_subscriptions",
            "SELECT COALESCE(string_agg(to_jsonb(s)::text, ''), '') FROM webhook_subscriptions s",
        ),
    ] {
        let text: String = sqlx::query_scalar(sql)
            .fetch_one(&h.pool)
            .await
            .expect(what);
        assert!(
            !text.contains(&secret),
            "the signing secret is stored in {what}"
        );
    }
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_entries WHERE action = 'CreateWebhookSubscription'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("audit");
    assert_eq!(audited, 1);
    common::drop_pool(&h.pool, &h.name).await;
}

// ---------------------------------------------------------------------------------------
// Egress deny and the kill switches
// ---------------------------------------------------------------------------------------

/// A resolver that answers what the test says — a name that "rebinds" to loopback.
struct FixedResolver(Vec<IpAddr>);

#[async_trait]
impl HostResolver for FixedResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Vec<IpAddr> {
        self.0.clone()
    }
}

#[tokio::test]
async fn destinations_obey_egress_policy_and_the_kill_switches() {
    // Tenant A starts with no network rule at all.
    let Some(h) = prepare("whk_egress", json!([read_rule()]), true).await else {
        common::blocked_marker();
        return;
    };
    let (receiver, port) = Receiver::start().await;
    let local = format!("http://127.0.0.1:{port}/hook");

    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "CreateWebhookSubscription",
        1,
        json!({ "url": local }),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (403, Some("POLICY_DENIED")),
        "no network rule: {body}"
    );

    let set_policy = |id: u32, rules: Value| {
        let h = &h;
        async move {
            let (status, body) = command(
                h,
                TENANT_A,
                OWNER_A,
                "SetPolicy",
                id,
                json!({ "scope": "tenant", "rules": rules }),
            )
            .await;
            assert_eq!(status, 200, "{body}");
        }
    };
    set_policy(2, json!([read_rule(), network_rule("deny", &[])])).await;
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "CreateWebhookSubscription",
        3,
        json!({ "url": local }),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (403, Some("POLICY_DENIED")),
        "a denying rule: {body}"
    );

    // Public addresses only (the default class set).
    set_policy(4, json!([read_rule(), network_rule("allow", &["public"])])).await;
    for (id, url) in [
        (5, local.clone()),
        (6, "http://169.254.169.254/latest/meta-data".to_string()),
        (7, "https://metadata.google.internal/hook".to_string()),
        (8, "http://localhost:9/hook".to_string()),
    ] {
        let (status, body) = command(
            &h,
            TENANT_A,
            OWNER_A,
            "CreateWebhookSubscription",
            id,
            json!({ "url": url }),
        )
        .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (403, Some("EGRESS_DENIED")),
            "{url}: {body}"
        );
    }
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "CreateWebhookSubscription",
        9,
        json!({ "url": "https://user:pw@hooks.example.com/x" }),
    )
    .await;
    assert_eq!(status, 400, "credentials in the URL are refused: {body}");

    // A name is judged at each attempt against what it resolves to then: rebinding to
    // loopback is refused and nothing is sent.
    let (named, _) = create(
        &h,
        TENANT_A,
        OWNER_A,
        10,
        json!({ "url": format!("http://hooks.example.com:{port}/named") }),
    )
    .await;
    let event = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKF1"),
        "run.succeeded",
        json!({}),
    )
    .await;
    fan_out(&h.pool, TENANT_A).await;
    let rebinding =
        deliverer(&h.pool, TENANT_A).with_resolver(Arc::new(FixedResolver(vec!["127.0.0.1"
            .parse()
            .expect("ip")])));
    rebinding
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    assert_eq!(
        delivery(&h.pool, &named, &event).await["last_status"],
        "egress_denied:address_not_allowed"
    );
    let unresolved =
        deliverer(&h.pool, TENANT_A).with_resolver(Arc::new(FixedResolver(Vec::new())));
    unresolved
        .deliver_due(Utc::now() + ChronoDuration::minutes(5), 32)
        .await
        .expect("deliver");
    assert_eq!(
        delivery(&h.pool, &named, &event).await["last_status"],
        "egress_denied:unresolved"
    );
    assert!(
        receiver.received().is_empty(),
        "nothing reached the receiver"
    );

    // Loopback allowed: the kill switches.
    set_policy(11, loopback_rules()).await;
    command(
        &h,
        TENANT_A,
        OWNER_A,
        "DeleteWebhookSubscription",
        12,
        json!({ "subscription_id": named }),
    )
    .await;
    let (sub, _) = create(&h, TENANT_A, OWNER_A, 13, json!({ "url": local })).await;

    // Per destination: a paused subscription's deliveries are held, then sent on resume.
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "UpdateWebhookSubscription",
        14,
        json!({ "subscription_id": sub, "status": "paused" }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let held = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKF2"),
        "run.succeeded",
        json!({}),
    )
    .await;
    assert_eq!(
        fan_out(&h.pool, TENANT_A).await,
        1,
        "a paused subscription still enqueues"
    );
    let report = deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    assert_eq!(report.attempted, 0, "held while paused");
    assert!(receiver.at("/hook").is_empty());
    command(
        &h,
        TENANT_A,
        OWNER_A,
        "UpdateWebhookSubscription",
        15,
        json!({ "subscription_id": sub, "status": "active" }),
    )
    .await;
    deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    assert_eq!(receiver.at("/hook").len(), 1);
    assert_eq!(delivery(&h.pool, &sub, &held).await["status"], "delivered");

    // Tenant-wide: a queued retry and a new event both stop once the policy denies egress.
    receiver.answer(&[500]);
    let retried = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKF3"),
        "run.failed",
        json!({}),
    )
    .await;
    fan_out(&h.pool, TENANT_A).await;
    deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now(), 32)
        .await
        .expect("deliver");
    assert_eq!(delivery(&h.pool, &sub, &retried).await["status"], "pending");
    let sent_before = receiver.at("/hook").len();
    set_policy(16, json!([read_rule(), network_rule("deny", &[])])).await;
    let fresh = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKF4"),
        "run.succeeded",
        json!({}),
    )
    .await;
    fan_out(&h.pool, TENANT_A).await;
    deliverer(&h.pool, TENANT_A)
        .deliver_due(Utc::now() + ChronoDuration::minutes(5), 32)
        .await
        .expect("deliver");
    assert_eq!(
        receiver.at("/hook").len(),
        sent_before,
        "nothing is sent while egress is denied"
    );
    for event in [&retried, &fresh] {
        assert_eq!(
            delivery(&h.pool, &sub, event).await["last_status"],
            "egress_denied:policy_denies"
        );
    }

    // Without a key provider nothing is signed, so nothing is sent.
    set_policy(17, loopback_rules()).await;
    let unsigned = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKF5"),
        "run.succeeded",
        json!({}),
    )
    .await;
    fan_out(&h.pool, TENANT_A).await;
    WebhookDeliverer::new(h.pool.clone(), TENANT_A, None)
        .deliver_due(Utc::now() + ChronoDuration::hours(1), 32)
        .await
        .expect("deliver");
    assert_eq!(
        delivery(&h.pool, &sub, &unsigned).await["last_status"],
        "signing_unavailable"
    );
    assert_eq!(receiver.at("/hook").len(), sent_before);
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_deployment_without_a_key_provider_refuses_to_create_a_webhook() {
    let Some(h) = prepare("whk_nokeys", loopback_rules(), false).await else {
        common::blocked_marker();
        return;
    };
    let (status, body) = command(
        &h,
        TENANT_A,
        OWNER_A,
        "CreateWebhookSubscription",
        1,
        json!({ "url": "https://hooks.example.com/x" }),
    )
    .await;
    assert_eq!(status, 500, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("QUANSIO_SECRETS_KEY_FILE"),
        "{body}"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM webhook_subscriptions")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(count, 0);
    common::drop_pool(&h.pool, &h.name).await;
}

// ---------------------------------------------------------------------------------------
// The SSE stream
// ---------------------------------------------------------------------------------------

async fn next_sse_event(
    body: &mut (impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
    buffer: &mut String,
) -> Option<(String, Value)> {
    use futures_util::StreamExt;
    loop {
        if let Some(end) = buffer.find("\n\n") {
            let block: String = buffer.drain(..end + 2).collect();
            let mut id = None;
            let mut data = String::new();
            let mut kind = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("id:") {
                    id = Some(value.trim().to_string());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data.push_str(value.trim_start());
                } else if let Some(value) = line.strip_prefix("event:") {
                    kind = Some(value.trim().to_string());
                }
            }
            if kind.as_deref() == Some("event") {
                return Some((
                    id.unwrap_or_default(),
                    serde_json::from_str(&data).expect("frame"),
                ));
            }
            continue;
        }
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(10), body.next())
            .await
            .ok()??
            .ok()?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
    }
}

#[tokio::test]
async fn the_event_stream_is_also_served_as_server_sent_events() {
    let Some(h) = prepare("whk_sse", loopback_rules(), true).await else {
        common::blocked_marker();
        return;
    };
    let first = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKG1"),
        "run.started",
        json!({}),
    )
    .await;
    emit(
        &h.pool,
        TENANT_A,
        Some(WS_A2),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKG2"),
        "run.started",
        json!({}),
    )
    .await;
    emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("thread", PROTECTED_THREAD),
        "thread.message_posted",
        json!({ "body": "between us" }),
    )
    .await;
    let second = emit(
        &h.pool,
        TENANT_A,
        Some(WS_A1),
        ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0WHKG1"),
        "run.succeeded",
        json!({}),
    )
    .await;

    let path = format!("/v1/stream/events?channel=workspace%3A{WS_A1}");
    let response = h
        .router
        .clone()
        .oneshot(http("GET", &path, TENANT_A, VIEWER_A, None))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap_or_default()
        .starts_with("text/event-stream"));
    let mut body = response.into_body().into_data_stream();
    let mut buffer = String::new();
    let (cursor, frame) = next_sse_event(&mut body, &mut buffer)
        .await
        .expect("first frame");
    assert_eq!(frame["kind"], "event");
    assert_eq!(
        frame["cursor"],
        cursor.as_str(),
        "the SSE id is the stream cursor"
    );
    assert_eq!(frame["event"]["event_id"], first.as_str());
    let (_, frame) = next_sse_event(&mut body, &mut buffer)
        .await
        .expect("second frame");
    assert_eq!(
        frame["event"]["event_id"],
        second.as_str(),
        "other workspaces and a protected thread the viewer is not in are withheld"
    );
    drop(body);

    // A reconnect with Last-Event-ID resumes after it.
    let request = Request::builder()
        .method("GET")
        .uri(&path)
        .header("x-quansio-tenant", TENANT_A)
        .header("x-quansio-user", VIEWER_A)
        .header("last-event-id", &cursor)
        .body(axum::body::Body::empty())
        .expect("request");
    let response = h.router.clone().oneshot(request).await.expect("response");
    let mut body = response.into_body().into_data_stream();
    let mut buffer = String::new();
    let (_, frame) = next_sse_event(&mut body, &mut buffer)
        .await
        .expect("resumed frame");
    assert_eq!(frame["event"]["event_id"], second.as_str());
    drop(body);

    let (status, body) = get(
        &h,
        TENANT_A,
        VIEWER_A,
        &format!("/v1/stream/events?channel=thread%3A{PROTECTED_THREAD}"),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, _) = get(&h, TENANT_A, VIEWER_A, "/v1/stream/events").await;
    assert_eq!(status, 400);
    common::drop_pool(&h.pool, &h.name).await;
}
