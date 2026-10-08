//! Remote control of a person's own computer over the live-frames WebSocket (ADP-009, DOMAIN.md §8.6.2): a **real PostgreSQL**
//! database, the **real server** on a TCP port and **real WebSocket clients** for the viewer, the computer's own side and a bystander
//! who watches the same target channel.
//!
//! Viewer authorization, negative first:
//! * nothing flows until the person at the computer says yes, and only that person, the computer's owner, can say it; the viewer cannot
//!   grant itself, a stranger cannot, and a grant can narrow a request and never widen it;
//! * frames reach the bound viewer and nobody else, input reaches the computer's own side and nobody else, even though a bystander is
//!   subscribed to the same channel and the same target;
//! * input is admitted only for a control grant, from the viewer's own account, and only while the computer reports capture ready, the
//!   screen unlocked and Screen Recording and Accessibility granted, freshly; a computer that falls silent stops admitting input;
//! * a malformed message, a frame for another viewer, an impersonated host, a stale sequence number and an excess rate are each refused
//!   with a reason and relayed to no one;
//! * Stop ends it for both sides at once, handing native input back to the agent ends it, and the lifecycle is in the event log and the
//!   audit chain with no pixel and no input in either.
//!
//! Capturing a real screen and posting real input need the operating-system permissions of ADP-002 and are the computer's own side's
//! (`BLOCKED_EXTERNAL` here); this suite plays that side with frames and readiness reports it sends itself.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; without it a test prints `BLOCKED_EXTERNAL` and returns.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::http::Request;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use quansio_machine::remote::RemoteConfig;
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

mod common;
use common::{blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP0RC";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP0RC";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADPVWR";
const STRANGER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADPSTR";
const SECOND: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADPSND";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP0RC";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP0RC";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9YADP0RC";
const SERVER_TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9YADP0SV";

static NEXT: AtomicU32 = AtomicU32::new(1);

fn command_id() -> String {
    format!(
        "cmd_01J8Z3K6F1N8VQ2X5W9Y0R{:04}",
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

struct Harness {
    router: axum::Router,
    addr: std::net::SocketAddr,
    pool: PgPool,
    name: String,
}

async fn member(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, user: &str, workspace_role: &str) {
    let suffix = user.trim_start_matches("usr_");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'seed')")
        .bind(user)
        .bind(format!("{user}@example.com"))
        .execute(&mut **tx)
        .await
        .expect("user");
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, 'member', 'active')")
        .bind(format!("tm_{suffix}"))
        .bind(TENANT)
        .bind(user)
        .execute(&mut **tx)
        .await
        .expect("tenant membership");
    sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ($1, $2, $3, $4, $5, 'active')")
        .bind(format!("wm_{suffix}"))
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(user)
        .bind(workspace_role)
        .execute(&mut **tx)
        .await
        .expect("workspace membership");
}

async fn prepare(prefix: &str, holder: &str) -> Option<Harness> {
    prepare_with(prefix, holder, |_| {}).await
}

async fn prepare_with(
    prefix: &str,
    holder: &str,
    tune: impl FnOnce(&mut RemoteConfig),
) -> Option<Harness> {
    let name = scratch_name(prefix);
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    member(&mut tx, VIEWER, "editor").await;
    member(&mut tx, STRANGER, "editor").await;
    member(&mut tx, SECOND, "editor").await;
    // The person's own Mac, owned by the owner; and a computer nobody owns and a non-native one.
    sqlx::query("INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, status, owner_kind, owner_ref) VALUES ($1, $2, $3, 'persistent_workspace_computer', 'local_capsule_macos', 'READY', 'user', $4)")
        .bind(TARGET)
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(OWNER)
        .execute(&mut *tx)
        .await
        .expect("target");
    sqlx::query("INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, status, owner_kind) VALUES ($1, $2, $3, 'persistent_workspace_computer', 'cloud_microvm', 'READY', 'workspace')")
        .bind(SERVER_TARGET)
        .bind(TENANT)
        .bind(WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("cloud target");
    sqlx::query("INSERT INTO computer_controls (target_id, tenant_id, run_id, holder, generation, control_since) VALUES ($1, $2, NULL, $3, 1, now())")
        .bind(TARGET)
        .bind(TENANT)
        .bind(holder)
        .execute(&mut *tx)
        .await
        .expect("computer control");
    tx.commit().await.expect("commit");
    // A shorter readiness window so a silent computer is observed in test time; every other bound is the shipped one.
    let mut config = RemoteConfig::builtin();
    config.host_report_max_age_seconds = 2;
    config.consent_ttl_seconds = 10;
    tune(&mut config);
    let state = ApiState::new(pool.clone())
        .expect("api")
        .with_remote_config(config);
    let router = quansio_server::api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let served = router.clone();
    tokio::spawn(async move {
        axum::serve(listener, served).await.ok();
    });
    Some(Harness {
        router,
        addr,
        pool,
        name,
    })
}

macro_rules! harness {
    ($prefix:expr, $holder:expr) => {
        match prepare($prefix, $holder).await {
            Some(h) => h,
            None => {
                blocked_marker();
                return;
            }
        }
    };
}

async fn command(h: &Harness, user: &str, name: &str, params: Value) -> (u16, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(format!("/v1/commands/{name}"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", user)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({ "command_id": command_id(), "params": params }).to_string(),
        ))
        .expect("request");
    let response = h.router.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or("")
}

/// A WebSocket client of `/v1/stream`, subscribed to the computer's channel.
struct Peer {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Peer {
    async fn connect(h: &Harness, user: &str) -> Self {
        let mut request = format!("ws://{}/v1/stream", h.addr)
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("x-quansio-tenant", TENANT.parse().expect("h"));
        request
            .headers_mut()
            .insert("x-quansio-user", user.parse().expect("h"));
        let (mut ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect");
        ws.send(Message::Text(
            json!({ "subscribe": [{ "channel": format!("target:{TARGET}") }] }).to_string(),
        ))
        .await
        .expect("subscribe");
        // Let the subscription reach its tail before anything is published.
        tokio::time::sleep(Duration::from_millis(250)).await;
        Self { ws }
    }

    async fn send(&mut self, message: Value) {
        self.ws
            .send(Message::Text(message.to_string()))
            .await
            .expect("send");
    }

    /// The next JSON text frame within `ms`, or `None`.
    async fn next(&mut self, ms: u64) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => return serde_json::from_str(&text).ok(),
                Ok(Some(Ok(_))) => {}
                _ => return None,
            }
        }
    }

    /// The next frame that is a `remote.*` live frame (or this socket's own refusal) within `ms`; durable events are skipped.
    async fn next_remote(&mut self, ms: u64) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let frame = self
                .next(u64::try_from(left.as_millis()).unwrap_or(0))
                .await?;
            if frame["kind"] == "live"
                && frame["frame"]["type"]
                    .as_str()
                    .is_some_and(|kind| kind.starts_with("remote."))
            {
                return Some(frame["frame"].clone());
            }
            if frame.get("remote").is_some() {
                return Some(frame["remote"].clone());
            }
        }
    }

    /// Everything `remote` that arrives within `ms`.
    async fn remote_within(&mut self, ms: u64) -> Vec<Value> {
        let mut found = Vec::new();
        while let Some(frame) = self.next_remote(ms).await {
            found.push(frame);
        }
        found
    }
}

fn jpeg() -> String {
    STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1])
}

fn frame(seq: u64) -> Value {
    json!({ "remote": { "kind": "frame", "target_id": TARGET, "viewer_user_id": VIEWER, "seq": seq, "width": 1280, "height": 720, "mime": "image/jpeg", "data": jpeg() } })
}

fn conditions(
    capture_ready: bool,
    unlocked: bool,
    screen_recording: &str,
    accessibility: &str,
) -> Value {
    json!({ "remote": { "kind": "conditions", "target_id": TARGET, "viewer_user_id": VIEWER, "capture_ready": capture_ready, "unlocked": unlocked, "screen_recording": screen_recording, "accessibility": accessibility } })
}

fn ready() -> Value {
    conditions(true, true, "granted", "granted")
}

fn click(seq: u64) -> Value {
    json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": seq, "event": { "type": "pointer_down", "x": 100, "y": 100, "button": "left" } } })
}

async fn request(h: &Harness, scope: &str) {
    let (status, body) = command(
        h,
        VIEWER,
        "RequestRemoteControl",
        json!({ "target_id": TARGET, "scope": scope }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["session"]["state"], "requested");
}

async fn grant(h: &Harness, scope: Option<&str>) -> (u16, Value) {
    let mut params = json!({ "target_id": TARGET, "viewer_user_id": VIEWER });
    if let Some(scope) = scope {
        params["scope"] = json!(scope);
    }
    command(h, OWNER, "GrantRemoteControl", params).await
}

async fn authorized(h: &Harness, scope: &str) -> (Peer, Peer, Peer) {
    let mut viewer = Peer::connect(h, VIEWER).await;
    let mut host = Peer::connect(h, OWNER).await;
    let mut stranger = Peer::connect(h, STRANGER).await;
    request(h, scope).await;
    let (status, body) = grant(h, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["session"]["granted_scope"], scope);
    // Both parties are told; the bystander is not.
    for peer in [&mut viewer, &mut host] {
        let state = peer.next_remote(2000).await.expect("a state frame");
        assert!(
            state["type"] == "remote.state" || state.get("ok").is_some(),
            "{state}"
        );
    }
    let _ = stranger.remote_within(300).await;
    // The computer's side reports ready and sends the first frame.
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    host.send(frame(1)).await;
    let first = viewer.remote_within(500).await;
    assert!(
        first.iter().any(|f| f["type"] == "remote.frame"),
        "{first:?}"
    );
    let _ = host.remote_within(200).await;
    (viewer, host, stranger)
}

async fn event_count(h: &Harness, kind: &str) -> i64 {
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let n = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = $2",
    )
    .bind(TENANT)
    .bind(kind)
    .fetch_one(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_flows_until_the_person_at_the_computer_says_yes_and_only_the_parties_receive_it() {
    let h = harness!("rc_flow", "user");
    let mut viewer = Peer::connect(&h, VIEWER).await;
    let mut host = Peer::connect(&h, OWNER).await;
    let mut stranger = Peer::connect(&h, STRANGER).await;
    request(&h, "control").await;

    // Before a grant: the computer's side cannot send a frame, nor can the viewer send input; nothing reaches anyone.
    host.send(ready()).await;
    host.send(frame(1)).await;
    viewer.send(click(1)).await;
    // (The computer's side is told a request is waiting, which is a state and not a refusal.)
    let refused_host: Vec<Value> = host
        .remote_within(400)
        .await
        .into_iter()
        .filter(|f| f["type"] != "remote.state")
        .collect();
    assert!(
        refused_host.iter().all(|f| f["ok"] == false),
        "{refused_host:?}"
    );
    assert!(
        refused_host.iter().any(|f| f["reason"] == "not_authorized"),
        "{refused_host:?}"
    );
    let refused_viewer: Vec<Value> = viewer
        .remote_within(400)
        .await
        .into_iter()
        .filter(|f| f["type"] != "remote.state")
        .collect();
    assert!(
        !refused_viewer.is_empty() && refused_viewer.iter().all(|f| f["ok"] == false),
        "{refused_viewer:?}"
    );

    // The viewer cannot grant itself, nor can a stranger; neither changes anything.
    let (status, body) = command(
        &h,
        VIEWER,
        "GrantRemoteControl",
        json!({ "target_id": TARGET, "viewer_user_id": VIEWER }),
    )
    .await;
    assert_eq!(
        status, 403,
        "the viewer is a party to the request and is not the computer's owner: {body}"
    );
    assert_eq!(code(&body), "SCOPE_FORBIDDEN");
    let (status, body) = command(
        &h,
        STRANGER,
        "GrantRemoteControl",
        json!({ "target_id": TARGET, "viewer_user_id": VIEWER }),
    )
    .await;
    assert_eq!(
        status, 404,
        "a stranger is not a party and is told there is no such request: {body}"
    );
    let (status, _) = command(
        &h,
        VIEWER,
        "DenyRemoteControl",
        json!({ "target_id": TARGET, "viewer_user_id": VIEWER }),
    )
    .await;
    assert_eq!(status, 403);

    // The person at the computer says yes.
    let (status, body) = grant(&h, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["session"]["state"], "authorized");
    for peer in [&mut viewer, &mut host] {
        let mut told = false;
        for frame in peer.remote_within(600).await {
            told |= frame["type"] == "remote.state" && frame["state"] == "authorized";
        }
        assert!(told, "both parties are told it is authorized");
    }

    // Frames reach the viewer alone; input reaches the computer's side alone. The bystander on the same channel sees neither.
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    host.send(frame(1)).await;
    let seen = viewer.remote_within(600).await;
    let shown = seen
        .iter()
        .find(|f| f["type"] == "remote.frame")
        .expect("the viewer sees the frame");
    assert_eq!(shown["data"], jpeg());
    assert_eq!(shown["width"], 1280);
    assert!(
        host.remote_within(300)
            .await
            .iter()
            .all(|f| f["type"] != "remote.frame"),
        "the computer is not echoed its own frame"
    );
    viewer.send(click(1)).await;
    let driven = host.remote_within(600).await;
    let input = driven
        .iter()
        .find(|f| f["type"] == "remote.input")
        .expect("the computer's side receives the input");
    assert_eq!(input["event"]["type"], "pointer_down");
    assert_eq!(input["event"]["x"], 100);
    assert!(
        viewer
            .remote_within(300)
            .await
            .iter()
            .all(|f| f["type"] != "remote.input"),
        "the viewer is not echoed its own input"
    );
    let bystander = stranger.remote_within(600).await;
    assert!(
        bystander.iter().all(|f| f["type"] == "remote.state"
            && f.get("data").is_none()
            && f.get("event").is_none()),
        "a bystander never receives pixels or input: {bystander:?}"
    );
    assert!(bystander
        .iter()
        .all(|f| f["type"] != "remote.frame" && f["type"] != "remote.input"));

    // Stop ends it for both sides at once, and nothing flows after it.
    let (status, body) = command(
        &h,
        OWNER,
        "StopRemoteControl",
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["sessions"][0]["state"], "stopped");
    assert_eq!(body["result"]["sessions"][0]["reason"], "host_stopped");
    for peer in [&mut viewer, &mut host] {
        let told = peer.remote_within(600).await;
        assert!(
            told.iter().any(|f| f["type"] == "remote.state"
                && f["state"] == "stopped"
                && f["reason"] == "host_stopped"),
            "{told:?}"
        );
    }
    viewer.send(click(2)).await;
    host.send(frame(2)).await;
    let after_viewer = viewer.remote_within(400).await;
    assert!(
        after_viewer
            .iter()
            .any(|f| f["reason"] == "stopped:host_stopped"),
        "{after_viewer:?}"
    );
    assert!(host
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.input"));
    assert!(viewer
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.frame"));

    // The lifecycle is in the event log and the audit chain, and neither holds a pixel or an input event.
    assert!(
        event_count(&h, "target.remote_changed").await >= 3,
        "requested, authorized, stopped"
    );
    let audit: Vec<(String, Option<String>)> = sqlx::query_as("SELECT action, reason FROM audit_entries WHERE action LIKE '%RemoteControl' ORDER BY occurred_at")
        .fetch_all(&h.pool)
        .await
        .expect("audit");
    let actions: Vec<&str> = audit.iter().map(|(action, _)| action.as_str()).collect();
    assert_eq!(
        actions,
        [
            "RequestRemoteControl",
            "GrantRemoteControl",
            "StopRemoteControl"
        ]
    );
    let everything = format!("{audit:?}");
    assert!(!everything.contains(&jpeg()) && !everything.contains("pointer_down"));
    let events: Vec<Value> = sqlx::query_scalar(
        "SELECT payload FROM runtime_events WHERE type = 'target.remote_changed'",
    )
    .fetch_all(&h.pool)
    .await
    .expect("events");
    assert!(events
        .iter()
        .all(|payload| payload.get("data").is_none() && payload.get("event").is_none()));
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_may_narrow_a_request_and_never_widen_it_and_a_view_grant_takes_no_input() {
    let h = harness!("rc_scope", "user");
    let mut viewer = Peer::connect(&h, VIEWER).await;
    let mut host = Peer::connect(&h, OWNER).await;
    request(&h, "view").await;
    let (status, body) = grant(&h, Some("control")).await;
    assert_eq!(status, 400, "a grant cannot widen what was asked: {body}");
    assert_eq!(code(&body), "VALIDATION_SCHEMA");
    let (status, body) = command(&h, OWNER, "GrantRemoteControl", json!({ "target_id": TARGET, "viewer_user_id": VIEWER, "scope": "view", "ttl_seconds": 999_999 })).await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = grant(&h, Some("view")).await;
    assert_eq!(status, 200, "{body}");
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    host.send(frame(1)).await;
    assert!(
        viewer
            .remote_within(600)
            .await
            .iter()
            .any(|f| f["type"] == "remote.frame"),
        "a view grant shows the screen"
    );
    viewer.send(click(1)).await;
    let refused = viewer.remote_within(500).await;
    assert!(
        refused.iter().any(|f| f["reason"] == "view_only"),
        "{refused:?}"
    );
    assert!(host
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.input"));
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn input_needs_a_computer_that_is_capturing_unlocked_permitted_and_still_reporting() {
    let h = harness!("rc_ready", "user");
    let (mut viewer, mut host, _) = authorized(&h, "control").await;
    // Ready: admitted.
    viewer.send(click(1)).await;
    assert!(host
        .remote_within(600)
        .await
        .iter()
        .any(|f| f["type"] == "remote.input"));

    let mut seq = 1;
    for (report, reason) in [
        (
            conditions(false, true, "granted", "granted"),
            "capture_not_ready",
        ),
        (conditions(true, false, "granted", "granted"), "locked"),
        (
            conditions(true, true, "denied", "granted"),
            "permission_missing:screen_recording",
        ),
        (
            conditions(true, true, "granted", "denied"),
            "permission_missing:accessibility",
        ),
        (
            conditions(true, true, "granted", "not_determined"),
            "permission_missing:accessibility",
        ),
    ] {
        host.send(report).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        seq += 1;
        viewer.send(click(seq)).await;
        let answers = viewer.remote_within(500).await;
        assert!(
            answers.iter().any(|f| f["reason"] == reason),
            "{reason}: {answers:?}"
        );
        assert!(
            host.remote_within(250)
                .await
                .iter()
                .all(|f| f["type"] != "remote.input"),
            "{reason}: nothing reached the machine"
        );
    }
    // A locked screen shows nothing either.
    host.send(conditions(true, false, "granted", "granted"))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    host.send(frame(2)).await;
    let refused_frame = host.remote_within(400).await;
    assert!(
        refused_frame.iter().any(|f| f["reason"] == "locked"),
        "{refused_frame:?}"
    );
    assert!(viewer
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.frame"));

    // Ready again: admitted. Then the computer falls silent: its last report goes stale and input stops being admitted.
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    seq += 1;
    viewer.send(click(seq)).await;
    assert!(host
        .remote_within(600)
        .await
        .iter()
        .any(|f| f["type"] == "remote.input"));
    tokio::time::sleep(Duration::from_millis(2300)).await;
    seq += 1;
    viewer.send(click(seq)).await;
    let silent = viewer.remote_within(500).await;
    assert!(
        silent.iter().any(|f| f["reason"] == "capture_not_ready"),
        "{silent:?}"
    );
    assert!(host
        .remote_within(250)
        .await
        .iter()
        .all(|f| f["type"] != "remote.input"));
    // It reports again: input is admitted, and what was refused while it was silent was not kept to be replayed.
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    seq += 1;
    viewer.send(click(seq)).await;
    let resumed = host.remote_within(600).await;
    assert_eq!(
        resumed
            .iter()
            .filter(|f| f["type"] == "remote.input")
            .count(),
        1,
        "{resumed:?}"
    );
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_bound_viewer_sends_input_and_only_the_computers_own_side_sends_frames() {
    let h = harness!("rc_parties", "user");
    let (mut viewer, mut host, mut stranger) = authorized(&h, "control").await;
    // A stranger on the same channel has no session of their own: input and frames are refused and reach no one.
    stranger.send(click(1)).await;
    stranger.send(frame(9)).await;
    stranger.send(ready()).await;
    let answers = stranger.remote_within(500).await;
    assert!(
        !answers.is_empty() && answers.iter().all(|f| f["ok"] == false),
        "{answers:?}"
    );
    assert!(host
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.input"));
    assert!(viewer
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.frame"));

    // The viewer cannot pass itself off as the computer: frames and readiness reports are the computer's side's alone.
    viewer.send(frame(9)).await;
    viewer
        .send(conditions(true, true, "granted", "granted"))
        .await;
    let impersonation = viewer.remote_within(500).await;
    assert!(
        impersonation
            .iter()
            .filter(|f| f["reason"] == "wrong_party")
            .count()
            >= 2,
        "{impersonation:?}"
    );

    // The computer's side cannot send input as the viewer: input is bound to the sender's own account.
    host.send(click(2)).await;
    let by_host = host.remote_within(400).await;
    assert!(by_host.iter().any(|f| f["ok"] == false), "{by_host:?}");
    assert!(viewer
        .remote_within(200)
        .await
        .iter()
        .all(|f| f["type"] != "remote.input"));

    // A frame for another viewer is not this session's.
    host.send(json!({ "remote": { "kind": "frame", "target_id": TARGET, "viewer_user_id": STRANGER, "seq": 5, "width": 10, "height": 10, "mime": "image/jpeg", "data": jpeg() } })).await;
    assert!(host
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "no_session"));
    assert!(stranger
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.frame"));
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_stale_or_excessive_message_is_refused_with_a_reason_and_relayed_to_no_one() {
    let h = harness!("rc_protocol", "user");
    let (mut viewer, mut host, _) = authorized(&h, "control").await;
    let bad_messages = [
        json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 2, "event": { "type": "open_url", "url": "https://example.com" } } }),
        json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 2, "event": { "type": "pointer_down", "x": 99999, "y": 1, "button": "left" } } }),
        json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 2, "event": { "type": "key_down", "key": "ab" } } }),
        json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 2, "event": { "type": "text", "text": "x\u{0000}y" } } }),
        json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 2, "event": { "type": "pointer_down", "x": 1, "y": 1, "button": "left" }, "as_host": true } }),
        json!({ "remote": { "kind": "shell", "target_id": TARGET } }),
        json!({ "not_remote": {} }),
    ];
    for message in bad_messages {
        viewer.send(message.clone()).await;
        let answers = viewer.remote_within(400).await;
        assert!(
            answers.iter().any(|f| f["reason"] == "invalid_message"),
            "{message}: {answers:?}"
        );
    }
    viewer
        .ws
        .send(Message::Text("not json at all".to_string()))
        .await
        .expect("send");
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "invalid_message"));
    // A point outside the screen the viewer was shown (1280x720) is refused by the session, not just by the form.
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    viewer.send(json!({ "remote": { "kind": "input", "target_id": TARGET, "seq": 2, "event": { "type": "pointer_down", "x": 1280, "y": 0, "button": "left" } } })).await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "invalid_input"));
    // An image that is not the image it says it is, and one that is too large.
    host.send(json!({ "remote": { "kind": "frame", "target_id": TARGET, "viewer_user_id": VIEWER, "seq": 5, "width": 10, "height": 10, "mime": "image/jpeg", "data": STANDARD.encode(b"<html>not an image</html>") } })).await;
    host.send(json!({ "remote": { "kind": "frame", "target_id": TARGET, "viewer_user_id": VIEWER, "seq": 6, "width": 10, "height": 10, "mime": "image/jpeg", "data": "A".repeat(RemoteConfig::builtin().frame_max_bytes + 10) } })).await;
    let frames = host.remote_within(600).await;
    assert_eq!(
        frames
            .iter()
            .filter(|f| f["reason"] == "invalid_message")
            .count(),
        2,
        "{frames:?}"
    );
    assert!(viewer
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.frame"));
    assert!(host
        .remote_within(200)
        .await
        .iter()
        .all(|f| f["type"] != "remote.input"));

    // Sequence numbers only increase, and the rate is bounded.
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    viewer.send(click(2)).await;
    assert!(host
        .remote_within(500)
        .await
        .iter()
        .any(|f| f["type"] == "remote.input"));
    viewer.send(click(2)).await;
    viewer.send(click(1)).await;
    assert_eq!(
        viewer
            .remote_within(500)
            .await
            .iter()
            .filter(|f| f["reason"] == "stale_seq")
            .count(),
        2
    );
    let burst = u64::from(RemoteConfig::builtin().input_max_per_second) + 30;
    for seq in 10..(10 + burst) {
        viewer.send(click(seq)).await;
    }
    let answers = viewer.remote_within(1500).await;
    assert!(
        answers.iter().any(|f| f["reason"] == "rate_limited"),
        "a burst past the limit is refused"
    );
    let relayed = host
        .remote_within(800)
        .await
        .iter()
        .filter(|f| f["type"] == "remote.input")
        .count();
    assert!(
        relayed <= usize::try_from(RemoteConfig::builtin().input_max_per_second).unwrap_or(0),
        "no more than the rate reaches the machine ({relayed})"
    );
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn asking_is_refused_for_a_viewer_who_may_not_a_computer_nobody_owns_and_one_that_is_not_a_native_computer(
) {
    let h = harness!("rc_ask", "user");
    // A member who cannot author work in the workspace.
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    member(&mut tx, "usr_01J8Z3K6F1N8VQ2X5W9YADPRDR", "viewer").await;
    tx.commit().await.expect("commit");
    let (status, body) = command(
        &h,
        "usr_01J8Z3K6F1N8VQ2X5W9YADPRDR",
        "RequestRemoteControl",
        json!({ "target_id": TARGET, "scope": "control" }),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = command(
        &h,
        VIEWER,
        "RequestRemoteControl",
        json!({ "target_id": SERVER_TARGET, "scope": "view" }),
    )
    .await;
    assert_eq!(
        status, 409,
        "a cloud computer is not a native computer: {body}"
    );
    for bad in [
        json!({ "target_id": TARGET }),
        json!({ "target_id": TARGET, "scope": "admin" }),
        json!({ "target_id": "run_x", "scope": "view" }),
        json!({ "target_id": "tgt_01J8Z3K6F1N8VQ2X5W9YADP0NO", "scope": "view" }),
    ] {
        let (status, body) = command(&h, VIEWER, "RequestRemoteControl", bad.clone()).await;
        assert!(status == 400 || status == 404, "{bad}: {status} {body}");
    }
    // A service principal cannot ask, decide or stop.
    let (status, created) = command(
        &h,
        OWNER,
        "CreateServicePrincipal",
        json!({ "name": "bot", "scopes": ["Browser/computer"] }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    let key = created["result"]["secret"]
        .as_str()
        .expect("key")
        .to_string();
    for (name, params) in [
        (
            "RequestRemoteControl",
            json!({ "target_id": TARGET, "scope": "view" }),
        ),
        (
            "GrantRemoteControl",
            json!({ "target_id": TARGET, "viewer_user_id": VIEWER }),
        ),
        (
            "DenyRemoteControl",
            json!({ "target_id": TARGET, "viewer_user_id": VIEWER }),
        ),
        ("StopRemoteControl", json!({ "target_id": TARGET })),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/v1/commands/{name}"))
            .header("x-quansio-tenant", TENANT)
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({ "command_id": command_id(), "params": params }).to_string(),
            ))
            .expect("request");
        let response = h.router.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status().as_u16(), 403, "{name}");
    }
    // A second request from a viewer who already has one open is a conflict, not a second session.
    request(&h, "view").await;
    let (status, body) = command(
        &h,
        VIEWER,
        "RequestRemoteControl",
        json!({ "target_id": TARGET, "scope": "control" }),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_or_lapsed_request_ends_and_a_stranger_cannot_stop_a_session() {
    let h = harness!("rc_deny", "user");
    request(&h, "control").await;
    let (status, body) = command(
        &h,
        STRANGER,
        "StopRemoteControl",
        json!({ "target_id": TARGET, "viewer_user_id": VIEWER }),
    )
    .await;
    assert_eq!(status, 403, "a stranger cannot stop it: {body}");
    let (status, body) = command(
        &h,
        OWNER,
        "DenyRemoteControl",
        json!({ "target_id": TARGET, "viewer_user_id": VIEWER }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["session"]["reason"], "denied");
    let (status, body) = grant(&h, None).await;
    assert_eq!(
        status, 409,
        "a denied request is not revived by a later yes: {body}"
    );
    // A new request is a new session, and the viewer can stop their own.
    request(&h, "view").await;
    let (status, body) = command(
        &h,
        VIEWER,
        "StopRemoteControl",
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["sessions"][0]["reason"], "viewer_stopped");
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_needs_the_person_to_hold_native_input_and_the_agent_taking_it_back_ends_the_session(
) {
    // The agent is driving: a control grant is refused until the person has taken over.
    let h = harness!("rc_fence", "agent");
    request(&h, "control").await;
    let (status, body) = grant(&h, None).await;
    assert_eq!(status, 409, "{body}");
    assert!(body["message"]
        .as_str()
        .unwrap_or_default()
        .contains("RequestTakeover"));
    // View needs no fence: the agent keeps driving while a person watches.
    let (status, body) = grant(&h, Some("view")).await;
    assert_eq!(status, 200, "{body}");
    drop_pool(&h.pool, &h.name).await;

    // The person holds input; a control session starts; handing input back to the agent ends it for both sides.
    let h = harness!("rc_handback", "user");
    let (mut viewer, mut host, _) = authorized(&h, "control").await;
    viewer.send(click(1)).await;
    assert!(host
        .remote_within(500)
        .await
        .iter()
        .any(|f| f["type"] == "remote.input"));
    let (status, body) = command(&h, OWNER, "Handback", json!({ "target_id": TARGET })).await;
    assert_eq!(status, 200, "{body}");
    for peer in [&mut viewer, &mut host] {
        let told = peer.remote_within(800).await;
        assert!(
            told.iter().any(|f| f["type"] == "remote.state"
                && f["state"] == "stopped"
                && f["reason"] == "agent_resumed"),
            "{told:?}"
        );
    }
    viewer.send(click(2)).await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "stopped:agent_resumed"));
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_that_runs_out_and_a_request_that_waits_too_long_end_for_both_sides() {
    let h = harness!("rc_expiry", "user");
    let mut viewer = Peer::connect(&h, VIEWER).await;
    let mut host = Peer::connect(&h, OWNER).await;
    request(&h, "view").await;
    let (status, body) = command(
        &h,
        OWNER,
        "GrantRemoteControl",
        json!({ "target_id": TARGET, "viewer_user_id": VIEWER, "scope": "view", "ttl_seconds": 1 }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["result"]["session"]["ends_at"]
        .as_str()
        .is_some_and(|ends| !ends.is_empty()));
    // No command runs after the grant: the open sockets' own clocks end the session, and both sides are told why.
    for peer in [&mut viewer, &mut host] {
        let mut ended = false;
        for frame in peer.remote_within(3000).await {
            ended |= frame["type"] == "remote.state"
                && frame["state"] == "stopped"
                && frame["reason"] == "session_expired";
        }
        assert!(ended, "both sides are told the grant ran out");
    }
    let audit: Vec<String> =
        sqlx::query_scalar("SELECT reason FROM audit_entries WHERE action = 'StopRemoteControl'")
            .fetch_all(&h.pool)
            .await
            .expect("audit");
    assert_eq!(
        audit,
        ["session_expired"],
        "the expiry is in the audit chain"
    );
    // The viewer's input after it ran out is refused and relayed to no one.
    host.send(frame(1)).await;
    viewer.send(click(1)).await;
    assert!(viewer.remote_within(400).await.iter().any(|f| f["reason"]
        .as_str()
        .is_some_and(|reason| reason.starts_with("stopped:"))));
    drop_pool(&h.pool, &h.name).await;

    // A request nobody answers lapses by itself, and a late yes is refused.
    let h = harness!("rc_lapse", "user");
    let mut viewer = Peer::connect(&h, VIEWER).await;
    request(&h, "control").await;
    let mut lapsed = false;
    for frame in viewer.remote_within(10_000 + 3000).await {
        lapsed |= frame["type"] == "remote.state" && frame["reason"] == "consent_expired";
    }
    assert!(lapsed, "the viewer is told the request lapsed");
    let (status, body) = grant(&h, None).await;
    assert_eq!(status, 409, "a late yes is refused: {body}");
    drop_pool(&h.pool, &h.name).await;
}

/// The computer's side reports again, so its readiness is current.
async fn fresh(host: &mut Peer) {
    host.send(ready()).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
}

fn clip(direction: &str, seq: u64, text: &str) -> Value {
    json!({ "remote": { "kind": "clipboard", "target_id": TARGET, "viewer_user_id": VIEWER, "direction": direction, "seq": seq, "text": text } })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_clipboard_is_its_own_bounded_channel_that_reaches_one_side_only() {
    let h = harness!("rc_clip", "user");
    let (mut viewer, mut host, mut stranger) = authorized(&h, "control").await;
    // Toward the computer: the viewer sends, the computer's own side alone receives, the bystander and the sender do not.
    viewer.send(clip("to_host", 1, "from the viewer")).await;
    let at_host = host.remote_within(600).await;
    let said = viewer.remote_within(100).await;
    assert!(
        at_host.iter().any(|f| f["type"] == "remote.clipboard"
            && f["direction"] == "to_host"
            && f["text"] == "from the viewer"),
        "{at_host:?} {said:?}"
    );
    assert!(stranger
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.clipboard"));
    assert!(viewer
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.clipboard"));
    // Toward the viewer: only the computer's side may send it, and only the viewer receives it.
    fresh(&mut host).await;
    host.send(clip("to_viewer", 1, "from the computer")).await;
    let at_viewer = viewer.remote_within(600).await;
    assert!(
        at_viewer
            .iter()
            .any(|f| f["type"] == "remote.clipboard" && f["direction"] == "to_viewer"),
        "{at_viewer:?}"
    );
    assert!(stranger
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.clipboard"));
    // The wrong side, the bystander, a repeated number, an oversize transfer and a path-like name are each refused and relayed to no one.
    fresh(&mut host).await;
    viewer
        .send(clip("to_viewer", 2, "viewer posing as the computer"))
        .await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "wrong_party"));
    fresh(&mut host).await;
    host.send(clip("to_host", 2, "host posing as the viewer"))
        .await;
    assert!(host
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "wrong_party"));
    stranger.send(clip("to_host", 3, "a bystander")).await;
    assert!(stranger
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "no_session" || f["reason"] == "wrong_party"));
    fresh(&mut host).await;
    viewer.send(clip("to_host", 1, "repeat")).await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "stale_seq"));
    let big = "x".repeat(RemoteConfig::builtin().clipboard_max_bytes + 1);
    fresh(&mut host).await;
    viewer.send(clip("to_host", 4, &big)).await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "invalid_message"));
    fresh(&mut host).await;
    viewer
        .send(json!({ "remote": { "kind": "clipboard", "target_id": TARGET, "viewer_user_id": VIEWER, "direction": "to_host", "seq": 5, "files": [{ "name": "../../etc/passwd", "data": STANDARD.encode(b"x") }] } }))
        .await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "invalid_message"));
    let leaked = host.remote_within(300).await;
    assert!(
        leaked.iter().all(|f| f["type"] != "remote.clipboard"),
        "nothing refused reached the machine: {leaked:?}"
    );
    // A locked screen takes and gives no clipboard.
    host.send(conditions(true, false, "granted", "granted"))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    viewer.send(clip("to_host", 6, "while locked")).await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "locked"));
    drop_pool(&h.pool, &h.name).await;

    // A view grant has no clipboard at all.
    let h = harness!("rc_clip_view", "user");
    let (mut viewer, mut host, _) = authorized(&h, "view").await;
    viewer.send(clip("to_host", 1, "view only")).await;
    assert!(viewer
        .remote_within(400)
        .await
        .iter()
        .any(|f| f["reason"] == "view_only"));
    assert!(host
        .remote_within(300)
        .await
        .iter()
        .all(|f| f["type"] != "remote.clipboard"));
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_computer_has_one_session_at_a_time_and_a_viewer_who_goes_quiet_loses_it() {
    let h = harness!("rc_busy", "user");
    request(&h, "view").await;
    // A second viewer is refused while the first is waiting, and again once the first is authorized.
    let (status, body) = command(
        &h,
        SECOND,
        "RequestRemoteControl",
        json!({ "target_id": TARGET, "scope": "view" }),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("busy"),
        "{body}"
    );
    let (status, _) = grant(&h, None).await;
    assert_eq!(status, 200);
    let (status, _) = command(
        &h,
        SECOND,
        "RequestRemoteControl",
        json!({ "target_id": TARGET, "scope": "view" }),
    )
    .await;
    assert_eq!(status, 409);
    // Stop frees the computer; then the second viewer may ask.
    let (status, body) = command(
        &h,
        OWNER,
        "StopRemoteControl",
        json!({ "target_id": TARGET }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = command(
        &h,
        SECOND,
        "RequestRemoteControl",
        json!({ "target_id": TARGET, "scope": "view" }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    drop_pool(&h.pool, &h.name).await;

    // A viewer who sends nothing loses the session after the idle limit; a ping keeps it.
    let h2 = match prepare_with("rc_idle2", "user", |config| config.session_idle_seconds = 4).await
    {
        Some(h2) => h2,
        None => {
            blocked_marker();
            return;
        }
    };
    let (mut viewer, mut host, _) = authorized(&h2, "view").await;
    let ping = json!({ "remote": { "kind": "ping", "target_id": TARGET } });
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        viewer.send(ping.clone()).await;
    }
    let alive = viewer.remote_within(300).await;
    assert!(
        alive.iter().all(|f| f["state"] != "stopped"),
        "pings kept it alive: {alive:?}"
    );
    // The host cannot keep it alive for the viewer.
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        host.send(ping.clone()).await;
    }
    let mut ended = false;
    for frame in viewer.remote_within(1500).await {
        ended |= frame["type"] == "remote.state"
            && frame["state"] == "stopped"
            && frame["reason"] == "idle";
    }
    assert!(ended, "a viewer who went quiet lost the session");
    drop_pool(&h2.pool, &h2.name).await;
}
