//! The composition sandbox (ADP-009, DOMAIN.md §7.5 "Composition sandbox", DOSSIER.md D-019).
//!
//! A model that wants to compose several browser, computer or tool calls submits one script as a single `compose.run`
//! call. The script runs in an embedded interpreter hosted here, inside `qworkerd` — not a second runtime, service or
//! store — with no ambient I/O, a memory and stack limit, a wall-clock deadline, a host-call budget and an output cap
//! (`config/composition.yaml`). Everything the script does, it does through `call(tool, args)`, and each `call` is **the
//! same governed call** a model proposing that tool directly would make: the [`HostCaller`] the worker is given hands
//! it to the runtime's dispatcher, which validates it against the Tool's schema, authorizes it against the run's
//! Capability Projection, evaluates policy, reserves and settles it on the Effect Ledger, and returns the tool's own
//! result (a computer action's receipt included). The sandbox can only add limits to that path.
//!
//! * [`ComposeHost::run`] is the `compose.run` tool host: it clamps what the caller asked for to the configured
//!   ceilings, runs one cell on a blocking thread, keeps the session's `state` between cells, and answers with the
//!   one result shape the declaration says (`UNTRUSTED_EXTERNAL`: the script's output is made of tool results).
//! * [`engine`] runs a cell. It is where a terminal condition is made uncatchable and where every limit is enforced.
//! * A re-delivered dispatch token is answered from the journal and the cell is never run twice: the cell's host
//!   calls are effects.

pub mod config;
pub mod engine;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use quansio_tools::{first_violation, ToolRegistry};
use serde_json::{json, Value};

pub use config::{ComposeConfig, ComposeLimits, ConfigError};
use engine::{run_cell, CellFailure, CellOutput, CellRequest};

use crate::browser::tools::ReplyStatus;
use crate::replay::{Replay, ReplayJournal};

/// The label every cell's output carries (DOMAIN.md §12): it is made of tool results.
pub const RESULT_TRUST: &str = "UNTRUSTED_EXTERNAL";

/// The tool this host serves.
pub const TOOL: &str = "compose.run";

/// What a host call came to, as the runtime's dispatcher reports it.
#[derive(Debug, Clone, PartialEq)]
pub enum HostResult {
    /// The call ran: the tool's own result (receipt included).
    Ok(Value),
    /// The call was refused or failed with a known outcome. Whether it ends the cell is `terminal_codes`' to say.
    Failed {
        /// The Quansio error code.
        code: String,
        /// What the runtime said.
        message: String,
        /// Whether the same request could succeed later.
        retryable: bool,
        /// Structured detail the tool gave.
        details: Option<Value>,
    },
    /// The call parked for an approval. The cell ends: nothing in a script waits for a person.
    Parked {
        /// What it waits for.
        detail: String,
    },
    /// The call's outcome is unknown. The cell ends: a script does not continue past a doubt.
    Unknown {
        /// Why it is unknown.
        detail: String,
    },
}

/// The runtime's dispatcher, as the sandbox sees it: the only way out of a cell.
///
/// An implementation hands the call to the same Tool Registry dispatch, Capability Projection, policy and Effect Ledger
/// as a call the model proposed, with the model as the caller (a tool that is not model-visible is not reachable from
/// a script). The sandbox does not authorize, decide or settle anything itself.
#[async_trait]
pub trait HostCaller: Send + Sync {
    /// Dispatch `tool` with `args`.
    async fn call(&self, tool: &str, args: Value) -> HostResult;
}

/// The identity of one `compose.run` dispatch.
#[derive(Debug, Clone, Copy)]
pub struct ComposeContext<'a> {
    /// The run the script acts for; a session belongs to one run.
    pub run_id: &'a str,
    /// The token that fences the dispatch; a re-delivery carries the same one.
    pub dispatch_token: &'a str,
}

/// What `compose.run` came to.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposeReply {
    /// How it ended: `ok` (the cell ran, successfully or not: see `output.ok`) or `error` (the call was malformed).
    pub status: ReplyStatus,
    /// The result for `ok`.
    pub output: Value,
    /// The typed error for `error`.
    pub error: Option<Value>,
    /// Whether this answered a re-delivered dispatch.
    pub replayed: bool,
}

impl ComposeReply {
    /// What the Effect Ledger records for the `compose.run` call itself.
    #[must_use]
    pub const fn ledger_outcome(&self) -> &'static str {
        match self.status {
            ReplyStatus::Ok => "SETTLED_SUCCESS",
            ReplyStatus::Error => "SETTLED_FAILED",
            ReplyStatus::InDoubt => "OUTCOME_UNKNOWN",
        }
    }

    fn failure(code: &str, message: &str) -> Self {
        Self {
            status: ReplyStatus::Error,
            output: Value::Null,
            error: Some(json!({
                "code": code,
                "message": message,
                "retryable": false,
                "requires_new_observation": false,
            })),
            replayed: false,
        }
    }
}

struct Session {
    state: Option<String>,
    last_used: Instant,
}

/// The `compose.run` tool host of one worker.
pub struct ComposeHost {
    config: Arc<ComposeConfig>,
    sessions: Mutex<BTreeMap<String, Session>>,
    journal: ReplayJournal<ComposeReply>,
}

impl std::fmt::Debug for ComposeHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposeHost")
            .field("sessions", &self.session_count())
            .finish_non_exhaustive()
    }
}

impl ComposeHost {
    /// A host for this configuration.
    #[must_use]
    pub fn new(config: ComposeConfig) -> Self {
        Self {
            config: Arc::new(config),
            sessions: Mutex::new(BTreeMap::new()),
            journal: ReplayJournal::new(512),
        }
    }

    /// The configuration in force.
    #[must_use]
    pub fn config(&self) -> &ComposeConfig {
        &self.config
    }

    /// Sessions that hold state now.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Run one `compose.run` call.
    pub async fn run(
        &self,
        args: &Value,
        context: &ComposeContext<'_>,
        caller: Arc<dyn HostCaller>,
    ) -> ComposeReply {
        match self.journal.lookup(context.dispatch_token) {
            Replay::Done(mut stored) => {
                stored.replayed = true;
                return stored;
            }
            Replay::Started => {
                // The cell's host calls are effects: a cell that started and has no recorded outcome is not run again.
                let mut reply = ComposeReply::failure(
                    "COMPOSE_TERMINATED",
                    "this script was already started and has no recorded outcome; it is not run again",
                );
                reply.replayed = true;
                return reply;
            }
            Replay::Fresh => {}
        }
        if !self.config.enabled {
            return ComposeReply::failure(
                "COMPOSE_LIMIT_EXCEEDED",
                "the composition sandbox is switched off",
            );
        }
        let Some(declaration) = ToolRegistry::builtin().get(TOOL) else {
            return ComposeReply::failure("INTERNAL", "compose.run is not declared");
        };
        if let Some(violation) = first_violation(&declaration.input_schema, args) {
            return ComposeReply::failure(
                "VALIDATION_SCHEMA",
                &format!(
                    "the arguments do not match compose.run: {} {}",
                    violation.path, violation.detail
                ),
            );
        }
        let limits = &self.config.limits;
        let script = args
            .get("script")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let session_id = args
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let key = format!("{}/{}", context.run_id, session_id);
        let reset = args.get("reset").and_then(Value::as_bool).unwrap_or(false);
        let deadline_ms = limits.deadline_for(args.get("deadline_ms").and_then(Value::as_u64));
        let budget = limits.budget_for(
            args.get("max_host_calls")
                .and_then(Value::as_u64)
                .map(|asked| u32::try_from(asked).unwrap_or(u32::MAX)),
        );

        let state_in = self.take_state(&key, reset);
        if !self.journal.start(context.dispatch_token) {
            return ComposeReply::failure("COMPOSE_TERMINATED", "this script is already running");
        }
        let request = CellRequest {
            script,
            state_json: state_in,
            deadline: Duration::from_millis(deadline_ms),
            host_call_budget: budget,
        };
        let config = Arc::clone(&self.config);
        let handle = tokio::runtime::Handle::current();
        let cell =
            tokio::task::spawn_blocking(move || run_cell(&config, caller, handle, &request)).await;
        let reply = if let Ok(output) = cell {
            self.finish(&key, session_id, &output)
        } else {
            // The blocking task itself failed (a panic in the interpreter binding): the host calls it made are
            // effects of unknown extent, so the session ends and the dispatch is not run again.
            self.clear(&key);
            ComposeReply::failure("INTERNAL", "the interpreter stopped unexpectedly")
        };
        self.journal.finish(context.dispatch_token, reply.clone());
        reply
    }

    /// The state a session goes into its next cell with: none after a reset or once it has been idle too long.
    fn take_state(&self, key: &str, reset: bool) -> Option<String> {
        let idle = Duration::from_millis(self.config.limits.idle_timeout_ms);
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        // Idle sessions are dropped when any session is next used: no timer, no daemon.
        sessions.retain(|_, session| session.last_used.elapsed() < idle);
        if reset {
            sessions.remove(key);
            return None;
        }
        sessions.get(key).and_then(|session| session.state.clone())
    }

    fn clear(&self, key: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }

    fn store(&self, key: &str, state: Option<String>) {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        if !sessions.contains_key(key) && sessions.len() >= self.config.limits.max_sessions {
            // The least recently used session makes room.
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.last_used)
                .map(|(key, _)| key.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            key.to_string(),
            Session {
                state,
                last_used: Instant::now(),
            },
        );
    }

    /// Shape a cell's output as the declaration's result and decide what happens to the session's state.
    fn finish(&self, key: &str, session_id: &str, cell: &CellOutput) -> ComposeReply {
        let mut error: Value = Value::Null;
        match &cell.failure {
            None => {
                // The cell succeeded: its state is the session's state.
                self.store(key, cell.state_json.clone());
            }
            Some(CellFailure::Script { message }) => {
                // A mistake in the script: the state the cell mutated is discarded, the one before it stays.
                error = json!({ "code": "COMPOSE_SCRIPT_ERROR", "message": message });
            }
            Some(CellFailure::Limit { limit, detail }) => {
                self.clear(key);
                error =
                    json!({ "code": "COMPOSE_LIMIT_EXCEEDED", "message": detail, "limit": limit });
            }
            Some(CellFailure::Terminated(terminal)) => {
                self.clear(key);
                error = json!({
                    "code": "COMPOSE_TERMINATED",
                    "message": terminal.detail,
                    "condition": terminal.condition,
                    "host_code": terminal.code,
                });
            }
        }
        let state_bytes = cell.state_json.as_ref().map_or(0, String::len);
        ComposeReply {
            status: ReplyStatus::Ok,
            output: json!({
                "ok": cell.failure.is_none(),
                "result_json": cell.result_json,
                "logs": cell.logs,
                "images": cell.images,
                "host_calls": cell.host_calls,
                "truncated": cell.logs_truncated,
                "state_bytes": state_bytes,
                "session_id": session_id,
                "error": error,
                "trust": RESULT_TRUST,
            }),
            error: None,
            replayed: false,
        }
    }
}
