//! A Chrome DevTools Protocol driver (EXEC-009).
//!
//! CDP is a WebSocket protocol: a client sends `{id, method, params}` and receives a `{id, result}` for
//! each, interleaved with `{method, params}` events that nobody asked for. Two things make a naive client
//! wrong, and this one handles both:
//!
//! * **responses and events share a channel.** A reader that assumes the next frame answers the request
//!   it just sent will mis-associate the moment the page emits a `Network.requestWillBeSent`. Every frame
//!   is read on one task, responses are matched by `id`, and events are queued for whoever asks.
//! * **a request may never be answered.** A page that hangs leaves the socket open, so every call is
//!   bounded by a deadline and a timeout is a typed failure rather than a hang.
//!
//! The browser's endpoint is discovered over HTTP — `/json/version` for the browser socket and
//! `/json/list` for page targets — which is a plain GET to loopback, so no HTTP client dependency is
//! needed for it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// How long one CDP call may take before it is abandoned.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// How long one call to the browser's HTTP endpoint may take.
const CONTROL_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a CDP call failed.
#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    /// The browser's debugging endpoint could not be reached.
    #[error("the browser's debugging endpoint is not reachable: {0}")]
    Unreachable(String),
    /// The endpoint answered, but not with what was asked for.
    #[error("the debugging endpoint answered unexpectedly: {0}")]
    Endpoint(String),
    /// The WebSocket transport failed.
    #[error("the CDP transport failed: {0}")]
    Transport(String),
    /// The call did not answer inside [`CALL_TIMEOUT`].
    #[error("CDP {method} did not answer within {}s", CALL_TIMEOUT.as_secs())]
    Timeout {
        /// The method that did not answer.
        method: String,
    },
    /// The browser answered a call with an error.
    #[error("CDP {method} failed: {message}")]
    Protocol {
        /// The method that failed.
        method: String,
        /// What the browser said.
        message: String,
    },
    /// Nobody is driving the browser any more.
    #[error("the CDP connection is closed")]
    Closed,
}

/// A page the browser has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTarget {
    /// The CDP target id.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Its current url.
    pub url: String,
    /// The per-page WebSocket endpoint to drive it on.
    pub websocket_url: String,
}

/// An event the browser emitted.
#[derive(Debug, Clone, PartialEq)]
pub struct CdpEvent {
    /// The event's method (`Page.loadEventFired`, `Network.responseReceived`, …).
    pub method: String,
    /// Its parameters.
    pub params: Value,
}

/// A live CDP connection to one target.
pub struct CdpConnection {
    calls: mpsc::Sender<Call>,
    events: Arc<Mutex<mpsc::Receiver<CdpEvent>>>,
    page: PageTarget,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

struct Call {
    method: String,
    params: Value,
    reply: oneshot::Sender<Result<Value, CdpError>>,
}

impl std::fmt::Debug for CdpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpConnection")
            .field("page", &self.page.id)
            .field("url", &self.page.url)
            .finish()
    }
}

impl CdpConnection {
    /// The page this connection drives.
    #[must_use]
    pub const fn page(&self) -> &PageTarget {
        &self.page
    }

    /// The cursor a page's DOM interaction would start from, for a session's metadata.
    #[must_use]
    pub fn current_url(&self) -> &str {
        &self.page.url
    }

    /// Whether the transport has closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Call a CDP method and wait for its result.
    ///
    /// # Errors
    /// Returns [`CdpError::Timeout`] when the browser does not answer in [`CALL_TIMEOUT`],
    /// [`CdpError::Protocol`] when it answers with an error, and [`CdpError::Closed`] when the
    /// connection is gone.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        let (reply, answered) = oneshot::channel();
        self.calls
            .send(Call {
                method: method.to_string(),
                params,
                reply,
            })
            .await
            .map_err(|_| CdpError::Closed)?;
        match tokio::time::timeout(CALL_TIMEOUT, answered).await {
            Ok(Ok(result)) => result,
            // The writer dropped the reply: the connection died mid-call.
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => Err(CdpError::Timeout {
                method: method.to_string(),
            }),
        }
    }

    /// The next event the browser emitted, if one is already queued.
    ///
    /// # Errors
    /// Returns [`CdpError::Closed`] when the connection is gone.
    pub async fn next_event(&self) -> Result<Option<CdpEvent>, CdpError> {
        let mut events = self.events.lock().await;
        Ok(events.try_recv().ok())
    }

    /// Wait for the next event, up to `within`.
    ///
    /// # Errors
    /// Returns [`CdpError::Timeout`] when nothing arrives, [`CdpError::Closed`] when the connection is
    /// gone.
    pub async fn wait_for_event(
        &self,
        method: &str,
        within: Duration,
    ) -> Result<CdpEvent, CdpError> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(CdpError::Timeout {
                    method: method.to_string(),
                });
            }
            let mut events = self.events.lock().await;
            match tokio::time::timeout(remaining, events.recv()).await {
                Ok(Some(event)) if event.method == method => return Ok(event),
                Ok(Some(_)) => continue,
                Ok(None) => return Err(CdpError::Closed),
                Err(_) => {
                    return Err(CdpError::Timeout {
                        method: method.to_string(),
                    })
                }
            }
        }
    }
}

/// Discover the browser endpoint from a running browser's debugging port.
///
/// # Errors
/// Returns [`CdpError::Unreachable`] when nothing is listening, and [`CdpError::Endpoint`] when the
/// answer is not the JSON the protocol describes.
pub async fn discover(port: u16) -> Result<String, CdpError> {
    let body = http_request("GET", port, "/json/version").await?;
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| CdpError::Endpoint(format!("/json/version: {error}")))?;
    parsed
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CdpError::Endpoint("/json/version named no browser socket".to_string()))
}

/// Every page the browser has open.
///
/// # Errors
/// As [`discover`].
pub async fn pages(port: u16) -> Result<Vec<PageTarget>, CdpError> {
    let body = http_request("GET", port, "/json/list").await?;
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| CdpError::Endpoint(format!("/json/list: {error}")))?;
    let listed = parsed
        .as_array()
        .ok_or_else(|| CdpError::Endpoint("/json/list was not a list".to_string()))?;
    Ok(listed
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("page"))
        .filter_map(|entry| {
            Some(PageTarget {
                id: entry.get("id")?.as_str()?.to_string(),
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                url: entry
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                websocket_url: entry.get("webSocketDebuggerUrl")?.as_str()?.to_string(),
            })
        })
        .collect())
}

/// Open a page and return it, so a session never drives a target it did not choose.
///
/// # Errors
/// As [`discover`], plus [`CdpError::Endpoint`] when the browser declines to open one.
pub async fn open_page(port: u16, url: &str) -> Result<PageTarget, CdpError> {
    let body = match http_request("PUT", port, &format!("/json/new?{url}")).await {
        Ok(body) => body,
        // Older Chrome answered GET; a build that refuses PUT is worth one more try before failing.
        Err(_) => http_request("GET", port, &format!("/json/new?{url}")).await?,
    };
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| CdpError::Endpoint(format!("/json/new: {error}")))?;
    Ok(PageTarget {
        id: parsed
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| CdpError::Endpoint("/json/new named no target".to_string()))?
            .to_string(),
        title: parsed
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        url: parsed
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(url)
            .to_string(),
        websocket_url: parsed
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| CdpError::Endpoint("/json/new named no socket".to_string()))?
            .to_string(),
    })
}

/// Connect to a page target and start pumping its frames.
///
/// One frame loop owns the socket in both directions, and one registry holds the calls awaiting an
/// answer. The writer and the reader must share that registry: a response is matched by the id the
/// writer chose, so two separate maps would leave every call unanswered until it timed out.
///
/// # Errors
/// Returns [`CdpError::Transport`] when the socket cannot be established.
pub async fn connect(target: PageTarget) -> Result<CdpConnection, CdpError> {
    let (socket, _) = tokio::time::timeout(
        CALL_TIMEOUT,
        tokio_tungstenite::connect_async(&target.websocket_url),
    )
    .await
    .map_err(|_| CdpError::Timeout {
        method: "the CDP handshake".to_string(),
    })?
    .map_err(|error| CdpError::Transport(error.to_string()))?;
    let (mut sink, mut stream) = socket.split();
    let (calls_tx, mut calls_rx) = mpsc::channel::<Call>(32);
    let (events_tx, events_rx) = mpsc::channel::<CdpEvent>(256);
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let awaiting: Awaiting = Arc::new(Mutex::new(HashMap::new()));

    // Writer.
    {
        let closed = Arc::clone(&closed);
        let awaiting = Arc::clone(&awaiting);
        tokio::spawn(async move {
            let mut next: i64 = 1;
            while let Some(call) = calls_rx.recv().await {
                let id = next;
                next += 1;
                let frame = json!({ "id": id, "method": call.method, "params": call.params });
                awaiting.lock().await.insert(id, call.reply);
                if sink
                    .send(Message::Text(frame.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // Closing the channel ends the reader; every call still waiting is answered by the reader's
            // drain, so nobody waits for a reply that cannot come.
            closed.store(true, std::sync::atomic::Ordering::SeqCst);
            for (_, reply) in awaiting.lock().await.drain() {
                let _ = reply.send(Err(CdpError::Closed));
            }
        });
    }

    // Reader: match responses by id, queue events, and keep going on anything unrecognised.
    {
        let closed = Arc::clone(&closed);
        let awaiting = Arc::clone(&awaiting);
        tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                let Ok(message) = frame else { break };
                let text = match message {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = parsed.get("id").and_then(Value::as_i64) {
                    if let Some(reply) = awaiting.lock().await.remove(&id) {
                        let outcome = match parsed.get("error") {
                            Some(error) => Err(CdpError::Protocol {
                                method: format!("call {id}"),
                                message: error.to_string(),
                            }),
                            None => Ok(parsed.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = reply.send(outcome);
                    }
                    continue;
                }
                if let Some(method) = parsed.get("method").and_then(Value::as_str) {
                    let event = CdpEvent {
                        method: method.to_string(),
                        params: parsed.get("params").cloned().unwrap_or(Value::Null),
                    };
                    if events_tx.send(event).await.is_err() {
                        break;
                    }
                }
            }
            closed.store(true, std::sync::atomic::Ordering::SeqCst);
            for (_, reply) in awaiting.lock().await.drain() {
                let _ = reply.send(Err(CdpError::Closed));
            }
        });
    }

    Ok(CdpConnection {
        calls: calls_tx,
        events: Arc::new(Mutex::new(events_rx)),
        page: target,
        closed,
    })
}

/// A minimal HTTP request to the browser's own loopback debugging endpoint.
///
/// The body is read to a deadline and stops as soon as `Content-Length` is satisfied. Reading to EOF
/// would make the worker hang whenever the endpoint keeps the connection alive, which Chrome does: a
/// request that is never answered is indistinguishable from a request whose answer was already read.
async fn http_request(method: &str, port: u16, path: &str) -> Result<String, CdpError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|error| CdpError::Unreachable(format!("127.0.0.1:{port}: {error}")))?;
    let request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| CdpError::Unreachable(error.to_string()))?;

    let mut response = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + CONTROL_CALL_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            // EOF: Chrome honoured `Connection: close`.
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(read)) => {
                response.extend_from_slice(&chunk[..read]);
                if body_is_complete(&response) {
                    break;
                }
            }
            // The deadline passed: judge what arrived rather than waiting for the rest.
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&response).into_owned();
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| CdpError::Endpoint("the response had no body".to_string()))?;
    let status = headers.lines().next().unwrap_or_default();
    if !status.contains(" 200") {
        return Err(CdpError::Endpoint(status.to_string()));
    }
    Ok(body.to_string())
}

/// Whether the headers name a length the body has already reached, so reading can stop.
fn body_is_complete(response: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(response) else {
        return false;
    };
    let Some((headers, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())?
    });
    matches!(length, Some(length) if body.len() >= length)
}

/// The calls awaiting an answer, shared by the two halves of the frame loop.
type Awaiting = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, CdpError>>>>>;
