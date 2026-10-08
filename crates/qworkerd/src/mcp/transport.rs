//! What a transport must do, the JSON-RPC framing both transports share and the bounded line reader.
//!
//! A transport carries one JSON-RPC 2.0 conversation with one server. It knows nothing about tools: the
//! client above it speaks MCP. The two facts the layers above need from a failed request are whether the
//! request **reached the server** (`sent`) and whether the server **answered**, because an unknown outcome
//! is decided by them: a call that was sent and not answered may have happened.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Why a request produced no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The server did not answer before the deadline.
    Timeout {
        /// Whether the request had been handed to the server.
        sent: bool,
    },
    /// The connection ended (the process exited, the stream closed) before an answer.
    Closed {
        /// Whether the request had been handed to the server.
        sent: bool,
    },
    /// A message was longer than the bound.
    TooLarge {
        /// Whether the request had been handed to the server.
        sent: bool,
    },
    /// The server answered with a JSON-RPC error: it ran nothing, or said it did not.
    Remote {
        /// The JSON-RPC error code.
        code: i64,
        /// The server's message, bounded and never trusted.
        message: String,
    },
    /// The server broke the protocol (a response that is neither a result nor an error, an HTTP status).
    Protocol(String),
    /// The egress broker refused the destination; nothing was sent.
    Denied(String),
    /// The server could not be started or reached; nothing was sent.
    Unreachable(String),
}

impl TransportError {
    /// Whether the request had reached the server when this happened.
    #[must_use]
    pub const fn sent(&self) -> bool {
        match self {
            Self::Timeout { sent } | Self::Closed { sent } | Self::TooLarge { sent } => *sent,
            Self::Remote { .. } | Self::Protocol(_) => true,
            Self::Denied(_) | Self::Unreachable(_) => false,
        }
    }
}

/// One JSON-RPC conversation.
#[async_trait]
pub(crate) trait Transport: Send + Sync {
    /// Send a request and wait for its result.
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError>;

    /// Send a notification (no answer).
    async fn notify(&self, method: &str, params: Value) -> Result<(), TransportError>;

    /// Whether the server said its tool list changed since this was last asked. Reading clears it.
    fn take_list_changed(&self) -> bool;

    /// Whether the conversation is over (the process exited, the session was dropped).
    fn is_closed(&self) -> bool;

    /// The protocol revision `initialize` settled on. Only the HTTP transport states it on every request.
    fn set_protocol(&self, _version: &str) {}

    /// End the conversation and release what it holds (kill the process, end the session).
    async fn close(&self);
}

/// The JSON-RPC 2.0 request object.
pub(crate) fn request_message(id: u64, method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// The JSON-RPC 2.0 notification object.
pub(crate) fn notification_message(method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// The answer to a request a server made of the client. The client declares no capability (no sampling, no
/// roots, no elicitation), so every request a server makes is one it must not make.
pub(crate) fn refusal_message(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32601, "message": "this client offers no capability a server may call" }
    })
}

/// What one incoming message is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Incoming {
    /// A response to a request the client made.
    Response {
        /// The request id it answers.
        id: u64,
        /// The result or the error.
        outcome: Result<Value, TransportError>,
    },
    /// A request the server made of the client.
    ServerRequest {
        /// Its id, to answer.
        id: Value,
    },
    /// A notification: whether it says the tool list changed.
    Notification {
        /// `notifications/tools/list_changed`.
        list_changed: bool,
    },
    /// Not a JSON-RPC message this client can place.
    Unplaceable,
}

/// Classify one decoded message.
pub(crate) fn classify(message: &Value, max_error_chars: usize) -> Incoming {
    let Some(object) = message.as_object() else {
        return Incoming::Unplaceable;
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Incoming::Unplaceable;
    }
    let method = object.get("method").and_then(Value::as_str);
    match (method, object.get("id")) {
        (Some(_), Some(id)) if id.is_string() || id.is_number() => {
            Incoming::ServerRequest { id: id.clone() }
        }
        (Some(method), None) => Incoming::Notification {
            list_changed: method == "notifications/tools/list_changed",
        },
        (None, Some(id)) => {
            let Some(id) = id.as_u64() else {
                return Incoming::Unplaceable;
            };
            if let Some(error) = object.get("error") {
                let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(max_error_chars)
                    .collect();
                return Incoming::Response {
                    id,
                    outcome: Err(TransportError::Remote { code, message }),
                };
            }
            match object.get("result") {
                Some(result) => Incoming::Response {
                    id,
                    outcome: Ok(result.clone()),
                },
                None => Incoming::Response {
                    id,
                    outcome: Err(TransportError::Protocol(
                        "a response carries neither a result nor an error".to_string(),
                    )),
                },
            }
        }
        _ => Incoming::Unplaceable,
    }
}

/// Why a bounded line could not be read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LineError {
    /// The line was longer than the bound; the stream is no longer in step and must be closed.
    TooLong,
    /// The read failed.
    Io,
}

/// Read one `\n`-terminated line of at most `max` bytes, or `None` at the end of the stream.
///
/// The bound holds **while reading**: a server that never sends a newline cannot make the worker buffer
/// without limit, which reading a line and then checking its length would.
pub(crate) async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, LineError> {
    let mut line: Vec<u8> = Vec::new();
    // `fill_buf` hands out what is buffered without reading past it, so each step consumes at most the
    // bytes up to the next newline and the line never grows beyond `max`.
    while let Ok(available) = reader.fill_buf().await {
        if available.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        let content = newline.map_or(take, |index| index);
        if line.len() + content > max {
            return Err(LineError::TooLong);
        }
        line.extend_from_slice(&available[..content]);
        reader.consume(take);
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
    Err(LineError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_line_is_read_up_to_its_newline_and_never_past_the_bound() {
        let mut reader = tokio::io::BufReader::with_capacity(8, &b"abc\r\nsecond\nrest"[..]);
        assert_eq!(
            read_bounded_line(&mut reader, 100).await,
            Ok(Some(b"abc".to_vec()))
        );
        assert_eq!(
            read_bounded_line(&mut reader, 100).await,
            Ok(Some(b"second".to_vec()))
        );
        assert_eq!(
            read_bounded_line(&mut reader, 100).await,
            Ok(Some(b"rest".to_vec()))
        );
        assert_eq!(read_bounded_line(&mut reader, 100).await, Ok(None));
        let mut long = tokio::io::BufReader::with_capacity(8, &b"0123456789abcdef\n"[..]);
        assert_eq!(
            read_bounded_line(&mut long, 10).await,
            Err(LineError::TooLong)
        );
        let mut exact = tokio::io::BufReader::with_capacity(8, &b"0123456789\n"[..]);
        assert_eq!(
            read_bounded_line(&mut exact, 10).await,
            Ok(Some(b"0123456789".to_vec()))
        );
    }

    #[test]
    fn messages_are_placed_by_their_shape_and_the_bad_ones_are_unplaceable() {
        let ok = json!({"jsonrpc": "2.0", "id": 3, "result": {"x": 1}});
        assert_eq!(
            classify(&ok, 10),
            Incoming::Response {
                id: 3,
                outcome: Ok(json!({"x": 1}))
            }
        );
        let err = json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32000, "message": "abcdefghijklmnop"}});
        assert_eq!(
            classify(&err, 5),
            Incoming::Response {
                id: 4,
                outcome: Err(TransportError::Remote {
                    code: -32000,
                    message: "abcde".to_string()
                })
            }
        );
        let request =
            json!({"jsonrpc": "2.0", "id": "s1", "method": "sampling/createMessage", "params": {}});
        assert_eq!(
            classify(&request, 10),
            Incoming::ServerRequest { id: json!("s1") }
        );
        let changed = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        assert_eq!(
            classify(&changed, 10),
            Incoming::Notification { list_changed: true }
        );
        let other = json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}});
        assert_eq!(
            classify(&other, 10),
            Incoming::Notification {
                list_changed: false
            }
        );
        for bad in [
            json!([]),
            json!({"id": 1, "result": {}}),
            json!({"jsonrpc": "1.0", "id": 1, "result": {}}),
            json!({"jsonrpc": "2.0", "id": 1}),
        ] {
            let placed = classify(&bad, 10);
            assert!(
                matches!(
                    placed,
                    Incoming::Unplaceable
                        | Incoming::Response {
                            outcome: Err(TransportError::Protocol(_)),
                            ..
                        }
                ),
                "{bad}: {placed:?}"
            );
        }
        assert!(TransportError::Timeout { sent: true }.sent());
        assert!(!TransportError::Unreachable("x".into()).sent());
    }
}
