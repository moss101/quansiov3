//! WebSocket `GET /v1/stream` (APP-001, DOMAIN.md §9.3).
//!
//! Transport only: subscribe, replay from a cursor, and disconnect a slow consumer with
//! `STREAM_BACKPRESSURE`. Durable frames come from [`quansio_events::EventStore`].

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use futures_util::StreamExt;
use quansio_core::{CanonicalId, Cursor, Prefix, UlidGenerator};
use quansio_events::{
    Channel, EventFrame, EventStore, StreamConfig, StreamError, StreamFrame, StreamSubscription,
};
use serde::Deserialize;
use serde_json::json;

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};

#[derive(Debug, Deserialize)]
struct SubscribeRequest {
    subscribe: Vec<SubscribeChannel>,
    cursor: Option<String>,
    consumer: Option<String>,
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
    let scope = TenantScope::from_headers(&headers)?;
    let consumer = headers
        .get("x-quansio-consumer")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, scope, consumer)))
}

async fn handle_socket(
    mut socket: WebSocket,
    state: ApiState,
    scope: TenantScope,
    consumer: Option<String>,
) {
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
    let consumer = request.consumer.or(consumer).unwrap_or_else(|| {
        format!(
            "csm_{}",
            CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new())
                .to_string()
                .trim_start_matches("cmd_")
        )
    });
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
    let (mut session, _live) = match store
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
    loop {
        match session.next_frame().await {
            Ok(frame) => {
                if let Some(cursor) = frame.cursor() {
                    last_cursor = Some(cursor.to_string());
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
    tail_events(
        &mut socket,
        &store,
        &subscription,
        &stream_id,
        &consumer,
        last_cursor,
    )
    .await;
}

async fn tail_events(
    socket: &mut WebSocket,
    store: &EventStore,
    subscription: &StreamSubscription,
    stream_id: &str,
    consumer: &str,
    mut last_cursor: Option<String>,
) {
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(Message::Ping(payload))) => {
                    let _ = socket.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
            () = tokio::time::sleep(Duration::from_millis(50)) => {
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
                    if !subscription.matches(&event) {
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
