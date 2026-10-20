//! WebSocket `GET /v1/stream` (APP-001, DOMAIN.md §9.3).
//!
//! Transport only: subscribe, replay from a cursor, and disconnect a slow consumer with
//! `STREAM_BACKPRESSURE`. Durable frames come from [`quansio_events::EventStore`].
//!
//! `GET /v1/stream/events?channel=…&cursor=…` (APP-015) is the same stream as Server-Sent
//! Events for read-only consumers — the web fallback and integrators: the same channels, the
//! same frames (`data:`), the same gate, and the cursor as the SSE `id:`, so a reconnect with
//! `Last-Event-ID` resumes where it left off.
//!
//! Every frame passes the subscriber's [`ThreadGate`] (APP-009): a protected thread's
//! `thread.*` and `question.*` events and its live frames reach only its active
//! participants, and a `thread:` channel of a protected thread cannot be subscribed to by
//! anyone else. The cursor still advances over a withheld event, so a resume does not
//! replay what the subscriber was not allowed to see.

use std::time::Duration;

use std::convert::Infallible;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{RawQuery, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use quansio_core::{CanonicalId, Cursor, Prefix, UlidGenerator};
use quansio_events::{
    Channel, EventFrame, EventStore, StreamConfig, StreamError, StreamFrame, StreamSubscription,
};
use serde::Deserialize;
use serde_json::json;

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::collaboration::{thread_access, ThreadGate};
use crate::policy::rbac::ActionFamily;

#[derive(Debug, Deserialize)]
struct SubscribeRequest {
    subscribe: Vec<SubscribeChannel>,
    cursor: Option<String>,
    consumer: Option<String>,
    /// Ask for a browser picture as a binary message ([`binary_picture`]) instead of base64 inside JSON.
    #[serde(default)]
    binary_frames: bool,
}

#[derive(Debug, Deserialize)]
struct SubscribeChannel {
    channel: String,
}

/// Upgrade the connection after the tenant is resolved.
pub async fn stream(
    ws: WebSocketUpgrade,
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let consumer = headers
        .get("x-quansio-consumer")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, scope, headers, consumer)))
}

/// How often an open stream re-checks the credential it was opened with.
///
/// A stream is authenticated when it opens; without a re-check, a session revoked afterwards
/// (`RevokeUserSessions`, a removed or suspended member, an expired access token) would keep
/// receiving the tenant's events for as long as the connection stayed open.
pub const REVALIDATE_EVERY: Duration = Duration::from_secs(2);

/// The credential an open stream re-checks, and when it next does.
struct Standing {
    state: ApiState,
    headers: HeaderMap,
    tenant_id: String,
    next: tokio::time::Instant,
}

impl Standing {
    fn new(state: &ApiState, headers: HeaderMap, tenant_id: &str) -> Self {
        Self {
            state: state.clone(),
            headers,
            tenant_id: tenant_id.to_string(),
            next: tokio::time::Instant::now() + REVALIDATE_EVERY,
        }
    }

    /// `Some(error frame)` when the credential no longer admits this stream.
    async fn lapsed(&mut self) -> Option<serde_json::Value> {
        if tokio::time::Instant::now() < self.next {
            return None;
        }
        self.next = tokio::time::Instant::now() + REVALIDATE_EVERY;
        match TenantScope::resolve(&self.state, &self.headers).await {
            Ok(scope) if scope.tenant_id == self.tenant_id => None,
            Ok(_) => Some(json!({
                "code": ApiErrorCode::TenantMismatch.as_str(),
                "message": "the session now resolves to another tenant",
                "correlation_id": "stream",
                "retryable": false,
            })),
            Err(error) => Some(json!({
                "code": error.code,
                "message": error.message,
                "correlation_id": "stream",
                "retryable": false,
            })),
        }
    }
}

async fn handle_socket(
    mut socket: WebSocket,
    state: ApiState,
    scope: TenantScope,
    headers: HeaderMap,
    consumer: Option<String>,
) {
    let mut standing = Standing::new(&state, headers, &scope.tenant_id);
    let Some(text) = next_text(&mut socket).await else {
        return;
    };
    let request: SubscribeRequest = match serde_json::from_str(&text) {
        Ok(request) => request,
        Err(error) => {
            let _ = socket
                .send(Message::Text(
                    json!({
                        "code": "VALIDATION_SCHEMA",
                        "message": error.to_string(),
                        "correlation_id": "stream",
                        "retryable": false,
                    })
                    .to_string(),
                ))
                .await;
            return;
        }
    };
    let mut channels = Vec::new();
    for item in &request.subscribe {
        match Channel::parse(&scope.tenant_id, &item.channel) {
            Ok(channel) => channels.push(channel),
            Err(error) => {
                let _ = socket
                    .send(Message::Text(stream_error_json(&error).to_string()))
                    .await;
                return;
            }
        }
    }
    // A target's channel is its workspace's readers' (APP-007).
    for item in &request.subscribe {
        if let Some(target_id) = item.channel.strip_prefix("target:") {
            if let Err(error) = target_admitted(&state, &scope, target_id).await {
                let _ = socket
                    .send(Message::Text(
                        json!({
                            "code": error.code,
                            "message": error.message,
                            "correlation_id": "stream",
                            "retryable": false,
                            "details": { "kind": "target", "target_id": target_id },
                        })
                        .to_string(),
                    ))
                    .await;
                return;
            }
        }
    }
    // A protected thread's channel is its participants' (APP-009).
    for item in &request.subscribe {
        if let Some(thread_id) = item.channel.strip_prefix("thread:") {
            let admitted = thread_access(
                state.pool(),
                &scope.tenant_id,
                thread_id,
                scope.user_id.as_deref(),
            )
            .await
            .is_ok_and(|access| access.admits());
            if !admitted {
                let _ = socket
                    .send(Message::Text(
                        json!({
                            "code": ApiErrorCode::ScopeForbidden.as_str(),
                            "message": format!("thread {thread_id} is protected and this user is not one of its participants"),
                            "correlation_id": "stream",
                            "retryable": false,
                            "details": { "kind": "thread", "thread_id": thread_id, "reason": "not_a_participant" },
                        })
                        .to_string(),
                    ))
                    .await;
                return;
            }
        }
    }
    let mut gate = ThreadGate::new(
        state.pool().clone(),
        &scope.tenant_id,
        scope.user_id.as_deref(),
    );
    // What this socket brings to the remote relay (ADP-009): its account and sign-in.
    let relay = super::remote::SocketRelay::new(&state, &scope).await;
    let consumer = request.consumer.or(consumer).unwrap_or_else(|| {
        format!(
            "csm_{}",
            CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new())
                .to_string()
                .trim_start_matches("cmd_")
        )
    });
    // A socket watching a target's channel is a viewer of its browsers: frames flow while one is connected and stop when
    // the last has gone, however the socket ends (the guards drop with this task).
    let _viewing: Vec<crate::worker::Viewer> = state
        .worker_hub()
        .map(|hub| {
            request
                .subscribe
                .iter()
                .filter_map(|item| item.channel.strip_prefix("target:"))
                .map(|target_id| hub.viewer_joined(&scope.tenant_id, target_id))
                .collect()
        })
        .unwrap_or_default();
    let mut subscription = match StreamSubscription::new(&scope.tenant_id, &consumer, channels) {
        Ok(subscription) => subscription,
        Err(error) => {
            let _ = socket
                .send(Message::Text(stream_error_json(&error).to_string()))
                .await;
            return;
        }
    };
    let mut last_cursor = request.cursor.clone();
    if let Some(cursor) = request.cursor {
        subscription = subscription.with_cursor(cursor);
    }
    let stream_id = subscription.stream_id();
    let store = EventStore::new(state.pool().clone());
    // The live sender is dropped at once. Nothing on this path produces live frames yet, and
    // a live channel that stays open keeps the replay session waiting on it forever: the
    // initial replay would never end, the tail below would never run, and a client that
    // subscribed before an event was committed would never receive it. Live frames are
    // transient by contract (DOMAIN.md §9.3), so their absence is tolerated; durable events
    // are not optional.
    let (mut session, live) = match store
        .subscribe(subscription.clone(), StreamConfig::default())
        .await
    {
        Ok(pair) => pair,
        Err(error) => {
            let _ = socket
                .send(Message::Text(stream_error_json(&error).to_string()))
                .await;
            return;
        }
    };
    drop(live);
    loop {
        match session.next_frame().await {
            Ok(frame) => {
                if let Some(cursor) = frame.cursor() {
                    last_cursor = Some(cursor.to_string());
                }
                let admitted = match &frame {
                    StreamFrame::Event(event) => gate.admits_event(&event.event).await,
                    StreamFrame::Live(live) if super::remote::is_remote(&live.frame) => {
                        relay.admits(&state, &live.frame)
                    }
                    StreamFrame::Live(live) => gate.admits_live(live).await,
                };
                if !admitted {
                    continue;
                }
                if socket
                    .send(Message::Text(frame.to_json_value().to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(StreamError::Closed) => break,
            Err(error) => {
                let _ = socket
                    .send(Message::Text(stream_error_json(&error).to_string()))
                    .await;
                return;
            }
        }
    }
    let live = state.live().subscribe(&scope.tenant_id);
    let mut shed = state.live().shed_signal(&scope.tenant_id);
    // The durable switch decides after a restart too (OPS-008 `stream.shed`).
    let engaged = crate::control::ops::OpsGate::load(state.pool(), &scope.tenant_id)
        .await
        .is_ok_and(|gate| gate.shedding());
    shed.mark_unchanged();
    tail_events(
        &mut socket,
        &store,
        &subscription,
        &stream_id,
        &consumer,
        last_cursor,
        live,
        &mut gate,
        Shedding {
            signal: shed,
            engaged,
        },
        &mut standing,
        &state,
        &relay,
        request.binary_frames,
    )
    .await;
}

/// Magic that opens a binary picture message.
pub const BINARY_PICTURE_MAGIC: &[u8; 4] = b"QFR1";

/// A `browser.frame` as one binary WebSocket message instead of base64 inside JSON (OD-008): the magic `QFR1`, the length of
/// the header as a big-endian `u32`, the header (the live-frame JSON exactly as the text form carries it, without `data`),
/// then the picture's bytes. `None` for any frame that is not a browser picture with valid base64, which is sent as text.
#[must_use]
pub fn binary_picture(frame: &quansio_events::LiveFrame) -> Option<Vec<u8>> {
    use base64::Engine as _;
    if frame.frame.get("kind")?.as_str()? != "browser.frame" {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(frame.frame.get("data")?.as_str()?.as_bytes())
        .ok()?;
    let mut header = StreamFrame::Live(frame.clone()).to_json_value();
    header["frame"].as_object_mut()?.remove("data");
    let header = header.to_string().into_bytes();
    let mut message = Vec::with_capacity(8 + header.len() + bytes.len());
    message.extend_from_slice(BINARY_PICTURE_MAGIC);
    message.extend_from_slice(&u32::try_from(header.len()).ok()?.to_be_bytes());
    message.extend_from_slice(&header);
    message.extend_from_slice(&bytes);
    Some(message)
}

#[allow(clippy::too_many_arguments)]
async fn tail_events(
    socket: &mut WebSocket,
    store: &EventStore,
    subscription: &StreamSubscription,
    stream_id: &str,
    consumer: &str,
    mut last_cursor: Option<String>,
    mut live: tokio::sync::broadcast::Receiver<quansio_events::LiveFrame>,
    gate: &mut ThreadGate,
    mut shedding: Shedding,
    standing: &mut Standing,
    state: &ApiState,
    relay: &super::remote::SocketRelay,
    binary_frames: bool,
) {
    let subscribed: Vec<String> = subscription
        .channels()
        .iter()
        .map(|channel| channel.as_str())
        .collect();
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(Message::Ping(payload))) => {
                    let _ = socket.send(Message::Pong(payload)).await;
                }
                // The one message a socket may send after it subscribed: a remote-control message (ADP-009). It is parsed
                // strictly, authorized against the session and relayed; a refusal is answered, never queued.
                Some(Ok(Message::Text(text))) => {
                    if let Some(reply) = relay.upstream(state, &text) {
                        let _ = socket.send(Message::Text(reply.to_string())).await;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
            frame = live.recv() => match frame {
                Ok(frame) => {
                    // Live frames are transient (DOMAIN.md §9.3): forwarded when on a subscribed
                    // channel, never buffered beyond the hub's capacity, never persisted. A `remote.*` frame
                    // is the two parties of a remote session's alone, whoever else watches the target.
                    let admitted = if super::remote::is_remote(&frame.frame) {
                        relay.admits(state, &frame.frame)
                    } else {
                        gate.admits_live(&frame).await
                    };
                    if subscribed.iter().any(|channel| channel == &frame.channel) && admitted {
                        let message = binary_frames
                            .then(|| binary_picture(&frame))
                            .flatten()
                            .map_or_else(
                                || Message::Text(StreamFrame::Live(frame).to_json_value().to_string()),
                                Message::Binary,
                            );
                        if socket.send(message).await.is_err() {
                            return;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // A laggard misses transient frames, unless the operator is shedding.
                    if shedding.engaged || shedding.signal.borrow().shedding {
                        let _ = socket.send(Message::Text(shed_error_json().to_string())).await;
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
            },
            changed = shedding.signal.changed() => {
                if changed.is_ok() {
                    let state = *shedding.signal.borrow_and_update();
                    shedding.engaged = state.shedding;
                    if state.shedding {
                        let _ = socket.send(Message::Text(shed_error_json().to_string())).await;
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                }
            },
            () = tokio::time::sleep(Duration::from_millis(50)) => {
                if let Some(refusal) = standing.lapsed().await {
                    // A sign-in that no longer stands ends the sessions it held.
                    relay.lapsed(state).await;
                    let _ = socket.send(Message::Text(refusal.to_string())).await;
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
                relay.sweep(state).await;
                let batch = match store
                    .resume(
                        subscription.tenant_id(),
                        stream_id,
                        consumer,
                        last_cursor.as_deref(),
                        256,
                    )
                    .await
                {
                    Ok(batch) => batch,
                    Err(error) => {
                        let _ = socket
                            .send(Message::Text(
                                stream_error_json(&StreamError::Store(error)).to_string(),
                            ))
                            .await;
                        return;
                    }
                };
                for event in batch.events {
                    let cursor = Cursor::new(stream_id.to_string(), event.sequence).encode();
                    last_cursor = Some(cursor.clone());
                    if !subscription.matches(&event) || !gate.admits_event(&event).await {
                        continue;
                    }
                    let frame = StreamFrame::Event(Box::new(EventFrame { cursor, event }));
                    if socket
                        .send(Message::Text(frame.to_json_value().to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
}

/// A live session's view of the tenant's `stream.shed` switch.
struct Shedding {
    signal: tokio::sync::watch::Receiver<crate::api::live::ShedState>,
    engaged: bool,
}

/// The frame a shed session receives before it is closed.
fn shed_error_json() -> serde_json::Value {
    stream_error_json(&StreamError::Backpressure {
        capacity: crate::api::live::LIVE_CAPACITY,
    })
}

async fn next_text(socket: &mut WebSocket) -> Option<String> {
    while let Some(frame) = socket.next().await {
        match frame {
            Ok(Message::Text(text)) => return Some(text),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(Message::Ping(payload)) => {
                let _ = socket.send(Message::Pong(payload)).await;
            }
            _ => {}
        }
    }
    None
}

fn stream_error_json(error: &StreamError) -> serde_json::Value {
    let code = match error.error_code() {
        "STREAM_BACKPRESSURE" => ApiErrorCode::StreamBackpressure,
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        _ => ApiErrorCode::Internal,
    };
    json!({
        "code": code.as_str(),
        "message": error.to_string(),
        "correlation_id": "stream",
        "retryable": error.is_retryable(),
    })
}

/// Decode one `application/x-www-form-urlencoded` component. A `%` not followed by two hex
/// digits is kept as it is.
fn form_decode(value: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let escaped = (byte == b'%')
            .then(|| Some((hex(*bytes.get(index + 1)?)?, hex(*bytes.get(index + 2)?)?)))
            .flatten();
        match (byte, escaped) {
            (_, Some((high, low))) => {
                out.push(high * 16 + low);
                index += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                index += 1;
            }
            (other, None) => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The SSE subscription as the query and headers give it.
#[derive(Debug, Default, PartialEq, Eq)]
struct SseRequest {
    channels: Vec<String>,
    cursor: Option<String>,
    consumer: Option<String>,
}

fn sse_request(query: Option<&str>, headers: &HeaderMap) -> SseRequest {
    let mut request = SseRequest::default();
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = form_decode(value);
        match key {
            "channel" => request.channels.extend(
                value
                    .split(',')
                    .filter(|channel| !channel.is_empty())
                    .map(str::to_string),
            ),
            "cursor" if !value.is_empty() => request.cursor = Some(value),
            "consumer" if !value.is_empty() => request.consumer = Some(value),
            _ => {}
        }
    }
    // A reconnecting EventSource sends the last `id:` it saw; it wins over the original URL.
    if let Some(last) = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        request.cursor = Some(last.to_string());
    }
    request
}

struct SseTail {
    store: EventStore,
    subscription: StreamSubscription,
    stream_id: String,
    consumer: String,
    cursor: Option<String>,
    gate: ThreadGate,
    pending: std::collections::VecDeque<SseEvent>,
    failed: bool,
    shed: tokio::sync::watch::Receiver<crate::api::live::ShedState>,
    standing: Standing,
}

/// A target's channel carries its browsers' pictures and its terminals' bytes, which are the workspace's to see: the caller
/// must be able to read the workspace the target belongs to. A target that does not exist for the tenant is refused the same
/// way, so the answer does not say which targets there are.
async fn target_admitted(
    state: &ApiState,
    scope: &TenantScope,
    target_id: &str,
) -> Result<(), ApiError> {
    let forbidden = || {
        ApiError::new(
            ApiErrorCode::ScopeForbidden,
            format!("target {target_id} is not one this user can watch"),
            "stream",
        )
    };
    let workspace: Option<String> = async {
        let mut tx = state.pool().begin().await.ok()?;
        crate::control::schema::set_tenant_context(&mut tx, &scope.tenant_id)
            .await
            .ok()?;
        sqlx::query_scalar(
            "SELECT workspace_id FROM execution_targets WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&scope.tenant_id)
        .bind(target_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()?
    }
    .await;
    let workspace = workspace.ok_or_else(forbidden)?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&workspace),
        ActionFamily::Read,
        "stream",
    )
    .await
    .map(|_| ())
    .map_err(|_| forbidden())
}

/// `GET /v1/stream/events?channel=…&cursor=…&consumer=…`: the stream as Server-Sent Events.
///
/// # Errors
/// `VALIDATION_SCHEMA` for no channel or a malformed one, `SCOPE_FORBIDDEN` for a protected
/// thread the caller does not participate in.
pub async fn stream_events(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let request = sse_request(query.as_deref(), &headers);
    if request.channels.is_empty() {
        return Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            "channel is required (workspace:ws_…, thread:thr_…, run:run_… or target:tgt_…)",
            "stream",
        ));
    }
    let mut channels = Vec::with_capacity(request.channels.len());
    for channel in &request.channels {
        channels.push(Channel::parse(&scope.tenant_id, channel).map_err(|error| {
            ApiError::new(ApiErrorCode::ValidationSchema, error.to_string(), "stream")
        })?);
        if let Some(target_id) = channel.strip_prefix("target:") {
            target_admitted(&state, &scope, target_id).await?;
        }
        if let Some(thread_id) = channel.strip_prefix("thread:") {
            let admitted = thread_access(
                state.pool(),
                &scope.tenant_id,
                thread_id,
                scope.user_id.as_deref(),
            )
            .await
            .is_ok_and(|access| access.admits());
            if !admitted {
                return Err(ApiError::new(
                    ApiErrorCode::ScopeForbidden,
                    format!("thread {thread_id} is protected and this user is not one of its participants"),
                    "stream",
                ));
            }
        }
    }
    let consumer = request.consumer.unwrap_or_else(|| {
        format!(
            "csm_{}",
            CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new())
                .to_string()
                .trim_start_matches("cmd_")
        )
    });
    let subscription =
        StreamSubscription::new(&scope.tenant_id, &consumer, channels).map_err(|error| {
            ApiError::new(ApiErrorCode::ValidationSchema, error.to_string(), "stream")
        })?;
    let tail = SseTail {
        store: EventStore::new(state.pool().clone()),
        stream_id: subscription.stream_id(),
        subscription,
        consumer,
        cursor: request.cursor,
        gate: ThreadGate::new(
            state.pool().clone(),
            &scope.tenant_id,
            scope.user_id.as_deref(),
        ),
        pending: std::collections::VecDeque::new(),
        failed: false,
        shed: {
            let mut signal = state.live().shed_signal(&scope.tenant_id);
            signal.mark_unchanged();
            signal
        },
        standing: Standing::new(&state, headers.clone(), &scope.tenant_id),
    };
    let stream = futures_util::stream::unfold(tail, |mut tail| async move {
        loop {
            if let Some(event) = tail.pending.pop_front() {
                return Some((Ok(event), tail));
            }
            if tail.failed {
                return None;
            }
            // The operator shed the tenant's sessions (OPS-008): close with the typed error.
            if tail.shed.has_changed().unwrap_or(false) && tail.shed.borrow_and_update().shedding {
                tail.failed = true;
                tail.pending.push_back(
                    SseEvent::default()
                        .event("error")
                        .data(shed_error_json().to_string()),
                );
                continue;
            }
            // The credential the stream was opened with still admits it (revocation, removal,
            // expiry end an open stream too).
            if let Some(refusal) = tail.standing.lapsed().await {
                tail.failed = true;
                tail.pending
                    .push_back(SseEvent::default().event("error").data(refusal.to_string()));
                continue;
            }
            match tail
                .store
                .resume(
                    tail.subscription.tenant_id(),
                    &tail.stream_id,
                    &tail.consumer,
                    tail.cursor.as_deref(),
                    256,
                )
                .await
            {
                Ok(batch) if batch.events.is_empty() => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Ok(batch) => {
                    for event in batch.events {
                        let cursor = Cursor::new(tail.stream_id.clone(), event.sequence).encode();
                        tail.cursor = Some(cursor.clone());
                        if !tail.subscription.matches(&event)
                            || !tail.gate.admits_event(&event).await
                        {
                            continue;
                        }
                        let frame = StreamFrame::Event(Box::new(EventFrame {
                            cursor: cursor.clone(),
                            event,
                        }));
                        tail.pending.push_back(
                            SseEvent::default()
                                .id(cursor)
                                .event("event")
                                .data(frame.to_json_value().to_string()),
                        );
                    }
                }
                Err(error) => {
                    tail.failed = true;
                    tail.pending.push_back(
                        SseEvent::default()
                            .event("error")
                            .data(stream_error_json(&StreamError::Store(error)).to_string()),
                    );
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod sse_tests {
    use super::*;

    #[test]
    fn the_query_names_channels_a_cursor_and_last_event_id_wins() {
        let mut headers = HeaderMap::new();
        let request = sse_request(
            Some("channel=workspace%3Aws_A&channel=run:run_B,thread:thr_C&cursor=abc&consumer=me"),
            &headers,
        );
        assert_eq!(
            request.channels,
            vec!["workspace:ws_A", "run:run_B", "thread:thr_C"]
        );
        assert_eq!(request.cursor.as_deref(), Some("abc"));
        assert_eq!(request.consumer.as_deref(), Some("me"));
        headers.insert("last-event-id", "def".parse().expect("header"));
        assert_eq!(
            sse_request(Some("channel=workspace:ws_A&cursor=abc"), &headers)
                .cursor
                .as_deref(),
            Some("def")
        );
        assert_eq!(form_decode("a%2Bb+c%zz"), "a+b c%zz");
    }
}
