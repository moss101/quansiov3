//! The stdio transport: a server is a subprocess the worker starts, and the conversation is
//! newline-delimited JSON-RPC on its standard streams.
//!
//! The process is started with a **cleared environment** and only the names the configuration lists (and
//! the values the entry itself sets), so it never inherits a provider key, a connector token or the worker's
//! own configuration. One reader task owns the child's output: it routes each response to the call waiting
//! for it, answers every request the server makes of the client with a refusal, records a
//! `tools/list_changed` notification, and, when the stream ends, fails every call still waiting with
//! `Closed` — so a crashed server never leaves a call hanging until its deadline.

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use quansio_tools::mcp::McpServerConfig;
use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use super::transport::{
    classify, notification_message, read_bounded_line, refusal_message, request_message, Incoming,
    Transport, TransportError,
};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, TransportError>>>>>;

/// Garbage a server may print before it is closed: a stream of non-JSON lines is a server that is not
/// speaking the protocol.
const MAX_UNPLACEABLE_LINES: usize = 64;

/// The environment a server's child gets: the inherited names that exist in `parent`, then the entry's
/// own values. Nothing else.
pub(crate) fn child_environment(
    server: &McpServerConfig,
    inherit: &std::collections::BTreeSet<String>,
    parent: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = parent
        .iter()
        .filter(|(name, _)| inherit.contains(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    environment.extend(server.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    environment
}

/// A running stdio server.
pub(crate) struct StdioTransport {
    child: AsyncMutex<Child>,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    list_changed: Arc<AtomicBool>,
    max_message_bytes: usize,
}

impl StdioTransport {
    /// Start the server's process.
    pub(crate) fn spawn(
        server: &McpServerConfig,
        environment: &BTreeMap<String, String>,
        max_message_bytes: usize,
    ) -> Result<Self, TransportError> {
        let command = server.command.as_deref().ok_or_else(|| {
            TransportError::Unreachable("a stdio server has no command".to_string())
        })?;
        let mut process = Command::new(command);
        process
            .args(&server.args)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = &server.cwd {
            process.current_dir(cwd);
        }
        let mut child = process.spawn().map_err(|error| {
            TransportError::Unreachable(format!("the server could not be started: {error}"))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::Unreachable("the server has no input".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::Unreachable("the server has no output".to_string()))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let list_changed = Arc::new(AtomicBool::new(false));
        let stdin = Arc::new(AsyncMutex::new(stdin));
        let transport = Self {
            child: AsyncMutex::new(child),
            stdin: Arc::clone(&stdin),
            pending: Arc::clone(&pending),
            next_id: AtomicU64::new(1),
            closed: Arc::clone(&closed),
            list_changed: Arc::clone(&list_changed),
            max_message_bytes,
        };
        // The reader answers the requests a server makes of the client, so it shares the input.
        let reader = Reader {
            pending,
            closed,
            list_changed,
            max_message_bytes,
        };
        tokio::spawn(reader.run(BufReader::new(stdout), stdin));
        Ok(transport)
    }

    async fn write_line(&self, message: &Value) -> Result<(), TransportError> {
        let mut bytes = serde_json::to_vec(message)
            .map_err(|error| TransportError::Protocol(error.to_string()))?;
        if bytes.len() > self.max_message_bytes {
            return Err(TransportError::TooLarge { sent: false });
        }
        bytes.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|_| TransportError::Closed { sent: false })?;
        stdin
            .flush()
            .await
            .map_err(|_| TransportError::Closed { sent: false })
    }

    fn forget(&self, id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }
}

struct Reader {
    pending: Pending,
    closed: Arc<AtomicBool>,
    list_changed: Arc<AtomicBool>,
    max_message_bytes: usize,
}

impl Reader {
    async fn run(
        self,
        mut output: BufReader<tokio::process::ChildStdout>,
        input: Arc<AsyncMutex<ChildStdin>>,
    ) {
        let mut unplaceable = 0usize;
        // An over-long line (`LineError`) ends the loop too: the stream is no longer in step.
        while let Ok(next) = read_bounded_line(&mut output, self.max_message_bytes).await {
            let Some(line) = next else {
                break;
            };
            let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                unplaceable += 1;
                if unplaceable > MAX_UNPLACEABLE_LINES {
                    break;
                }
                continue;
            };
            match classify(&message, 512) {
                Incoming::Response { id, outcome } => {
                    let waiting = self
                        .pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&id);
                    if let Some(waiting) = waiting {
                        let _ = waiting.send(outcome);
                    }
                }
                Incoming::ServerRequest { id } => {
                    let mut bytes = serde_json::to_vec(&refusal_message(&id)).unwrap_or_default();
                    bytes.push(b'\n');
                    let mut stdin = input.lock().await;
                    let _ = stdin.write_all(&bytes).await;
                    let _ = stdin.flush().await;
                }
                Incoming::Notification { list_changed } => {
                    if list_changed {
                        self.list_changed.store(true, Ordering::SeqCst);
                    }
                }
                Incoming::Unplaceable => {
                    unplaceable += 1;
                    if unplaceable > MAX_UNPLACEABLE_LINES {
                        break;
                    }
                }
            }
        }
        self.closed.store(true, Ordering::SeqCst);
        let waiting: Vec<_> = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, sender)| sender)
            .collect();
        for sender in waiting {
            let _ = sender.send(Err(TransportError::Closed { sent: true }));
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        if self.is_closed() {
            return Err(TransportError::Closed { sent: false });
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, sender);
        let written = tokio::time::timeout(
            timeout,
            self.write_line(&request_message(id, method, &params)),
        )
        .await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.forget(id);
                return Err(error);
            }
            Err(_) => {
                // The server stopped reading its input: part of the request may be written and the stream is
                // out of step, so the conversation is over and the request counts as sent.
                self.forget(id);
                self.closed.store(true, Ordering::SeqCst);
                return Err(TransportError::Timeout { sent: true });
            }
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(TransportError::Closed { sent: true }),
            Err(_) => {
                self.forget(id);
                // Tell the server it may stop; whether it does is its business, and the outcome stays unknown.
                let _ = self
                    .write_line(&notification_message(
                        "notifications/cancelled",
                        &serde_json::json!({ "requestId": id, "reason": "deadline" }),
                    ))
                    .await;
                Err(TransportError::Timeout { sent: true })
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), TransportError> {
        self.write_line(&notification_message(method, &params))
            .await
    }

    fn take_list_changed(&self) -> bool {
        self.list_changed.swap(false, Ordering::SeqCst)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}
