//! The MCP client: external tool servers reached from inside the execution target (ADP-009, DOMAIN.md §7.5
//! "External tools", DOSSIER.md D-019).
//!
//! The worker connects to the servers `config/mcp.yaml` names, over stdio (a subprocess it starts) or
//! streamable HTTP (a destination the egress broker allows), and does three things:
//!
//! * **discovery** ([`McpHost::sync`], [`McpHost::refresh_server`]): it lists a server's tools, hands them to
//!   `quansio_tools::mcp` to be reduced, classified by the configuration and declared, and replaces that
//!   server's tools in the shared [`RegistryHandle`]. A tool list that changes while a run is going is read
//!   again the same way; a tool that was added is registered at the class the configuration gives it (the
//!   unclassified default needs an approval) and a projection decides whether anyone sees it, so nothing is
//!   ever granted by appearing;
//! * **dispatch** ([`McpHost::dispatch`]): a call the runtime already planned, authorized and reserved on the
//!   Effect Ledger is sent to the server and its answer is reduced to the one result shape every external tool
//!   has (`UNTRUSTED_EXTERNAL`, text bounded, other content counted and dropped);
//! * **outcome**: a call that was **sent** and then not answered (a deadline, a closed connection, an answer
//!   over the bound) is `in_doubt` for any tool that is not a read, and settles as `OUTCOME_UNKNOWN`; nothing
//!   here retries it, and a re-delivered dispatch token is answered from the journal and never sent again.
//!
//! The worker decides nothing about authority: the registry's declaration says what the call is, the runtime
//! reserved the effect before it arrived, and a server's own claims (annotations, descriptions) never classify
//! anything. A server that asks the client for something (sampling, roots, elicitation) is refused: the client
//! declares no capability.

mod http;
mod stdio;
mod transport;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use quansio_tools::mcp::{
    build_server_tools, DiscoveredTool, McpConfig, McpServerConfig, McpTransportKind, Rejection,
    ToolListChange, READ_ONLY_CLASSES,
};
use quansio_tools::{
    canonical_json, first_violation, EffectClassRule, RegistryHandle, ToolDeclaration, ToolOrigin,
};
use serde_json::{json, Map, Value};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

pub use http::{broker_gate, BrokerEgressGate, EgressGate};
use transport::{Transport, TransportError};

use crate::browser::tools::ReplyStatus;
use crate::replay::{Replay, ReplayJournal};

/// The label every external result carries (DOMAIN.md §12).
pub const RESULT_TRUST: &str = "UNTRUSTED_EXTERNAL";

/// Protocol revisions the client speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Replies the journal keeps for re-delivered dispatch tokens.
const MAX_JOURNAL: usize = 2048;

/// What one `tools/call` came to, in the vocabulary the host reports to the runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct McpReply {
    /// The qualified tool name.
    pub tool: String,
    /// How it ended.
    pub status: ReplyStatus,
    /// The result for `ok`; the reduced result (when there is one) or `null` otherwise.
    pub output: Value,
    /// The typed error: `{code, message, retryable, requires_new_observation}`.
    pub error: Option<Value>,
    /// Evidence bytes: the call's record (digests, never the arguments or the result).
    pub evidence: Option<Vec<u8>>,
    /// Whether this answered a re-delivered dispatch token.
    pub replayed: bool,
}

impl McpReply {
    /// What the Effect Ledger records for this reply.
    #[must_use]
    pub const fn ledger_outcome(&self) -> &'static str {
        match self.status {
            ReplyStatus::Ok => "SETTLED_SUCCESS",
            ReplyStatus::Error => "SETTLED_FAILED",
            ReplyStatus::InDoubt => "OUTCOME_UNKNOWN",
        }
    }

    /// The wire form returned to the runtime.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut reply = json!({
            "tool": self.tool,
            "status": self.status.as_str(),
            "trust": RESULT_TRUST,
            "output": self.output,
            "ledger_outcome": self.ledger_outcome(),
            "replayed": self.replayed,
        });
        if let Some(error) = &self.error {
            reply["error"] = error.clone();
        }
        reply
    }

    fn failure(tool: &str, code: &str, message: &str, retryable: bool) -> Self {
        Self {
            tool: tool.to_string(),
            status: ReplyStatus::Error,
            output: Value::Null,
            error: Some(error_object(code, message, retryable)),
            evidence: None,
            replayed: false,
        }
    }

    fn in_doubt(tool: &str, message: &str) -> Self {
        Self {
            tool: tool.to_string(),
            status: ReplyStatus::InDoubt,
            output: json!({
                "status": "in_doubt",
                "retryable": false,
                "message": message,
            }),
            error: Some(error_object("MCP_CALL_IN_DOUBT", message, false)),
            evidence: None,
            replayed: false,
        }
    }
}

fn error_object(code: &str, message: &str, retryable: bool) -> Value {
    json!({
        "code": code,
        "message": message,
        "retryable": retryable,
        "requires_new_observation": false,
    })
}

/// The identity of one dispatch, which the worker's journal is keyed by.
#[derive(Debug, Clone, Copy)]
pub struct McpCallContext<'a> {
    /// The token that fences the call and names its effect; a re-delivery carries the same one.
    pub dispatch_token: &'a str,
    /// The effect the call settles.
    pub effect_id: &'a str,
}

/// What a discovery pass over one server found.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSync {
    /// The server.
    pub server: String,
    /// Whether the server answered and its tools were read.
    pub connected: bool,
    /// Why it did not, when it did not (a transport rule, never a server's own words at length).
    pub failure: Option<String>,
    /// Tools registered after the pass.
    pub registered: usize,
    /// What changed against what was registered before.
    pub change: ToolListChange,
    /// Tools the pass refused, with the rule.
    pub rejected: Vec<Rejection>,
}

/// What a discovery pass over every configured server found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncReport {
    /// One entry per configured server, in configuration order.
    pub servers: Vec<ServerSync>,
}

/// One configured server and what the host holds for it.
struct Slot {
    server: McpServerConfig,
    link: AsyncMutex<Link>,
    permits: Semaphore,
    /// The server's own names of the tools its last accepted list holds.
    accepted: Mutex<BTreeSet<String>>,
}

#[derive(Default)]
struct Link {
    transport: Option<Arc<dyn Transport>>,
    starts: u32,
    restarts_used: u32,
    unavailable: bool,
}

/// The MCP client of one worker.
pub struct McpHost {
    config: Arc<McpConfig>,
    registry: RegistryHandle,
    egress: Arc<dyn EgressGate>,
    /// The worker's environment, cut down to the names some server may inherit: nothing else is ever kept.
    environment: BTreeMap<String, String>,
    slots: BTreeMap<String, Arc<Slot>>,
    journal: ReplayJournal<McpReply>,
}

impl std::fmt::Debug for McpHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHost")
            .field("servers", &self.slots.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl McpHost {
    /// A host for `config`'s servers. `registry` is the handle the dispatcher and the model's tool offer
    /// share; `egress` decides which HTTP destinations may be reached.
    #[must_use]
    pub fn new(config: McpConfig, registry: RegistryHandle, egress: Arc<dyn EgressGate>) -> Self {
        let mut inheritable: BTreeSet<String> = config.stdio.inherit_env.iter().cloned().collect();
        for server in &config.servers {
            inheritable.extend(server.inherit_env.iter().cloned());
        }
        let environment: BTreeMap<String, String> = std::env::vars()
            .filter(|(name, _)| inheritable.contains(name))
            .collect();
        let slots = config
            .servers
            .iter()
            .map(|server| {
                (
                    server.name.clone(),
                    Arc::new(Slot {
                        server: server.clone(),
                        link: AsyncMutex::new(Link::default()),
                        permits: Semaphore::new(server.max_concurrent_calls(&config.limits)),
                        accepted: Mutex::new(BTreeSet::new()),
                    }),
                )
            })
            .collect();
        Self {
            config: Arc::new(config),
            registry,
            egress,
            environment,
            slots,
            journal: ReplayJournal::new(MAX_JOURNAL),
        }
    }

    /// The registry handle this host keeps current.
    #[must_use]
    pub fn registry(&self) -> &RegistryHandle {
        &self.registry
    }

    /// Read every configured server's tools into the registry.
    pub async fn sync(&self) -> SyncReport {
        let mut report = SyncReport::default();
        for server in &self.config.servers {
            report.servers.push(self.refresh_server(&server.name).await);
        }
        report
    }

    /// Read one server's tool list again, re-validate every tool and replace the server's tools in the
    /// registry. A tool that appeared is registered at the class the configuration gives it; one that
    /// changed is registered at the next version; one that went away is retired.
    pub async fn refresh_server(&self, name: &str) -> ServerSync {
        let mut sync = ServerSync {
            server: name.to_string(),
            connected: false,
            failure: None,
            registered: 0,
            change: ToolListChange::default(),
            rejected: Vec::new(),
        };
        let Some(slot) = self.slots.get(name) else {
            sync.failure = Some("mcp.server.unknown".to_string());
            return sync;
        };
        if !self.config.enabled {
            sync.failure = Some("mcp.disabled".to_string());
            return sync;
        }
        let discovered = match self.discover(slot).await {
            Ok(discovered) => discovered,
            Err(error) => {
                sync.failure = Some(transport_rule(&error).to_string());
                return sync;
            }
        };
        sync.connected = true;
        let snapshot = self.registry.current();
        let previous: Vec<ToolDeclaration> =
            snapshot.external_of(name).into_iter().cloned().collect();
        let previous_refs: Vec<&ToolDeclaration> = previous.iter().collect();
        let built = build_server_tools(&self.config, &slot.server, &discovered, &previous_refs);
        sync.rejected = built.rejected;
        let accepted: BTreeSet<String> = built
            .declarations
            .iter()
            .filter_map(|declaration| match &declaration.origin {
                ToolOrigin::External { remote_name, .. } => Some(remote_name.clone()),
                ToolOrigin::Builtin => None,
            })
            .collect();
        sync.registered = built.declarations.len();
        match self.registry.replace_server_tools(name, built.declarations) {
            Ok(change) => {
                sync.change = change;
                *slot.accepted.lock().unwrap_or_else(PoisonError::into_inner) = accepted;
            }
            Err(error) => {
                // Not registered: the registry is unchanged and the server's previous tools stay as they were.
                sync.connected = false;
                sync.failure = Some(format!("mcp.registry: {error}"));
            }
        }
        sync
    }

    /// Run one `tools/call` the runtime has planned and reserved.
    pub async fn dispatch(
        &self,
        tool: &str,
        version: u32,
        args: &Value,
        context: &McpCallContext<'_>,
    ) -> McpReply {
        match self.journal.lookup(context.dispatch_token) {
            Replay::Done(mut stored) => {
                stored.replayed = true;
                stored
            }
            Replay::Started => {
                // A delivery that was sent and has no recorded outcome: it may have reached the server.
                let mut reply = McpReply::in_doubt(
                    tool,
                    "this dispatch was already sent to the server and has no recorded outcome; it is not sent again",
                );
                reply.replayed = true;
                reply
            }
            Replay::Fresh => self.dispatch_once(tool, version, args, context).await,
        }
    }

    async fn dispatch_once(
        &self,
        tool: &str,
        version: u32,
        args: &Value,
        context: &McpCallContext<'_>,
    ) -> McpReply {
        let fail = |code: &str, message: &str, retryable: bool| {
            McpReply::failure(tool, code, message, retryable)
        };
        if !self.config.enabled {
            return fail(
                "MCP_SERVER_UNAVAILABLE",
                "the MCP client is switched off",
                false,
            );
        }
        let snapshot = self.registry.current();
        let Some(declaration) = snapshot.get_version(tool, version) else {
            return fail(
                "MCP_TOOL_NOT_REGISTERED",
                "no external tool is registered under this name and version",
                false,
            );
        };
        let ToolOrigin::External {
            server,
            remote_name,
        } = &declaration.origin
        else {
            return fail(
                "MCP_TOOL_NOT_REGISTERED",
                "the tool is not an external tool",
                false,
            );
        };
        let (server, remote_name) = (server.clone(), remote_name.clone());
        let Some(slot) = self.slots.get(&server) else {
            return fail(
                "MCP_TOOL_NOT_REGISTERED",
                "the tool's server is not configured",
                false,
            );
        };
        let latest = match snapshot.get(tool) {
            Some(latest) => latest.version,
            None => {
                return fail(
                    "MCP_TOOL_NOT_REGISTERED",
                    "the server no longer offers this tool",
                    false,
                )
            }
        };
        if latest != version {
            return McpReply::failure(
                tool,
                "MCP_TOOL_CHANGED",
                "the server changed this tool after the call was planned; plan the call again",
                true,
            );
        }
        if let Some(violation) = first_violation(&declaration.input_schema, args) {
            return fail(
                "MCP_INVALID_REQUEST",
                &format!(
                    "the arguments do not match {tool}: {} {}",
                    violation.path, violation.detail
                ),
                false,
            );
        }
        let read_only = matches!(&declaration.effect_class, EffectClassRule::Static(class)
            if READ_ONLY_CLASSES.contains(&class.as_str()));
        let deadline = Duration::from_millis(
            declaration
                .timeout_ms
                .min(self.config.limits.call_timeout_max_ms),
        );
        let max_result = self.config.limits.max_result_bytes;
        let output_schema = declaration.output_schema.clone();
        drop(snapshot);

        // A place at the server: calls beyond the server's concurrency wait, and then are refused.
        let queue = Duration::from_millis(self.config.limits.queue_timeout_ms);
        let Ok(Ok(_permit)) = tokio::time::timeout(queue, slot.permits.acquire()).await else {
            return fail(
                "MCP_BUSY",
                "the server is serving as many calls as it may; try again",
                true,
            );
        };
        let transport = match self.connect(slot).await {
            Ok(transport) => transport,
            Err(error) => return unreachable_reply(tool, &error),
        };
        // The server said its list changed: read it again before this call goes out. A tool that changed is a
        // call planned against a list that is no longer the server's.
        if transport.take_list_changed() {
            let refreshed = self.refresh_server(&server).await;
            let now = self.registry.current();
            if refreshed.connected && now.get(tool).map(|d| d.version) != Some(version) {
                return fail(
                    "MCP_TOOL_CHANGED",
                    "the server's tool list changed and this tool with it; plan the call again",
                    true,
                );
            }
        }
        if !slot
            .accepted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&remote_name)
        {
            return fail(
                "MCP_TOOL_NOT_REGISTERED",
                "the server's accepted tool list does not hold this tool",
                false,
            );
        }

        if !self.journal.start(context.dispatch_token) {
            return McpReply::in_doubt(
                tool,
                "this dispatch is already in flight; it is not sent twice",
            );
        }
        let params = json!({ "name": remote_name, "arguments": args });
        let sent_at = std::time::Instant::now();
        let answer = transport.request("tools/call", params, deadline).await;
        let record = json!({
            "server": server,
            "tool": remote_name,
            "arguments_digest": quansio_core::Digest::of_canonical_json(&canonical_json(args)).to_string(),
            "elapsed_ms": u64::try_from(sent_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            "effect_id": context.effect_id,
        });
        let evidence = Some(serde_json::to_vec(&record).unwrap_or_default());
        let reached = answer.is_ok() || answer.as_ref().is_err_and(TransportError::sent);
        let mut reply = match answer {
            Ok(result) => {
                self.reset_restarts(slot).await;
                reduce_reply(tool, &result, max_result, &output_schema)
            }
            Err(error) => failure_reply(tool, &error, read_only),
        };
        reply.evidence = evidence;
        if reached {
            self.journal.finish(context.dispatch_token, reply.clone());
        } else {
            // Nothing reached the server, so the same dispatch may be delivered again.
            self.journal.abandon(context.dispatch_token);
        }
        reply
    }

    async fn reset_restarts(&self, slot: &Slot) {
        slot.link.lock().await.restarts_used = 0;
    }

    /// The transport to a server, starting (or restarting, with backoff) it when there is none.
    async fn connect(&self, slot: &Slot) -> Result<Arc<dyn Transport>, TransportError> {
        let mut link = slot.link.lock().await;
        if let Some(transport) = &link.transport {
            if !transport.is_closed() {
                return Ok(Arc::clone(transport));
            }
            link.transport = None;
        }
        if link.unavailable {
            return Err(TransportError::Unreachable(
                "the server exited more often than its restart budget allows".to_string(),
            ));
        }
        if link.starts > 0 {
            if link.restarts_used >= self.config.restart.max_restarts {
                link.unavailable = true;
                return Err(TransportError::Unreachable(
                    "the server exited more often than its restart budget allows".to_string(),
                ));
            }
            link.restarts_used += 1;
            let delay = self.config.restart.delay_ms(link.restarts_used);
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        link.starts += 1;
        let transport: Arc<dyn Transport> = match slot.server.transport {
            McpTransportKind::Stdio => {
                let inherit = slot.server.inherited_names(&self.config.stdio);
                let environment =
                    stdio::child_environment(&slot.server, &inherit, &self.environment);
                Arc::new(stdio::StdioTransport::spawn(
                    &slot.server,
                    &environment,
                    self.config.limits.max_message_bytes,
                )?)
            }
            McpTransportKind::Http => Arc::new(http::HttpTransport::new(
                slot.server.url.as_deref().unwrap_or_default(),
                Arc::clone(&self.egress),
                self.config.limits.max_message_bytes,
            )),
        };
        if let Err(error) = self.initialize(&*transport).await {
            transport.close().await;
            return Err(error);
        }
        link.transport = Some(Arc::clone(&transport));
        Ok(transport)
    }

    async fn initialize(&self, transport: &dyn Transport) -> Result<(), TransportError> {
        let timeout = Duration::from_millis(self.config.limits.initialize_timeout_ms);
        let result = transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSIONS[0],
                    // No capability is offered: a server may not ask the client for sampling, roots or input.
                    "capabilities": {},
                    "clientInfo": { "name": "quansio-qworkerd", "version": env!("CARGO_PKG_VERSION") },
                }),
                timeout,
            )
            .await?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|version| PROTOCOL_VERSIONS.contains(version))
            .ok_or_else(|| {
                TransportError::Protocol(
                    "the server's protocol revision is not one this client speaks".to_string(),
                )
            })?;
        transport.set_protocol(version);
        transport
            .notify("notifications/initialized", json!({}))
            .await
    }

    /// `tools/list`, every page, bounded.
    async fn discover(&self, slot: &Slot) -> Result<Vec<DiscoveredTool>, TransportError> {
        let transport = self.connect(slot).await?;
        let limits = &self.config.limits;
        let timeout = Duration::from_millis(limits.initialize_timeout_ms);
        let mut found: Vec<DiscoveredTool> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..limits.max_list_pages {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |c| json!({ "cursor": c }));
            let page = transport.request("tools/list", params, timeout).await?;
            let tools = page.get("tools").and_then(Value::as_array).ok_or_else(|| {
                TransportError::Protocol("tools/list carries no tools list".to_string())
            })?;
            for tool in tools {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                found.push(DiscoveredTool {
                    name: name.to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input_schema: tool.get("inputSchema").cloned().unwrap_or(Value::Null),
                });
            }
            if found.len() > limits.max_tools_per_server.saturating_mul(2) {
                break;
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        // A listing that was read is the new baseline: whatever notification preceded it is answered.
        let _ = transport.take_list_changed();
        Ok(found)
    }

    /// End every conversation (kill every server process).
    pub async fn close_all(&self) {
        for slot in self.slots.values() {
            let mut link = slot.link.lock().await;
            if let Some(transport) = link.transport.take() {
                transport.close().await;
            }
        }
    }
}

fn transport_rule(error: &TransportError) -> &'static str {
    match error {
        TransportError::Timeout { .. } => "mcp.transport.timeout",
        TransportError::Closed { .. } => "mcp.transport.closed",
        TransportError::TooLarge { .. } => "mcp.transport.too_large",
        TransportError::Remote { .. } => "mcp.transport.remote_error",
        TransportError::Protocol(_) => "mcp.transport.protocol",
        TransportError::Denied(_) => "mcp.transport.egress_denied",
        TransportError::Unreachable(_) => "mcp.transport.unreachable",
    }
}

fn unreachable_reply(tool: &str, error: &TransportError) -> McpReply {
    match error {
        TransportError::Denied(rule) => McpReply::failure(
            tool,
            "EGRESS_DENIED",
            &format!("the egress broker refused the server's destination ({rule})"),
            false,
        ),
        other => McpReply::failure(
            tool,
            "MCP_SERVER_UNAVAILABLE",
            &format!(
                "the server could not be reached ({})",
                transport_rule(other)
            ),
            true,
        ),
    }
}

/// What a request that produced no result means for the call.
fn failure_reply(tool: &str, error: &TransportError, read_only: bool) -> McpReply {
    match error {
        TransportError::Remote { code, message } => McpReply::failure(
            tool,
            "MCP_PROTOCOL_ERROR",
            &format!("the server answered with an error ({code}): {message}"),
            false,
        ),
        TransportError::Denied(_) | TransportError::Unreachable(_) => unreachable_reply(tool, error),
        // From here the request had reached the server and no answer came: it may have happened.
        _ if error.sent() && !read_only => McpReply::in_doubt(
            tool,
            "the request reached the server and no answer came back, so the outcome cannot be established; it is not retried",
        ),
        TransportError::Timeout { .. } => McpReply::failure(
            tool,
            "TOOL_TIMEOUT",
            "the server did not answer inside the tool's deadline",
            true,
        ),
        TransportError::TooLarge { .. } => McpReply::failure(
            tool,
            "MCP_RESULT_INVALID",
            "the server's answer was larger than the bound",
            false,
        ),
        TransportError::Protocol(detail) => McpReply::failure(tool, "MCP_PROTOCOL_ERROR", detail, false),
        TransportError::Closed { .. } => McpReply::failure(
            tool,
            "MCP_SERVER_UNAVAILABLE",
            "the server's connection ended before it answered",
            true,
        ),
    }
}

/// Reduce a `tools/call` result to the one result shape, and say how it settles.
fn reduce_reply(tool: &str, result: &Value, max_bytes: usize, output_schema: &Value) -> McpReply {
    let invalid = |detail: &str| McpReply::failure(tool, "MCP_RESULT_INVALID", detail, false);
    let Some(object) = result.as_object() else {
        return invalid("the result is not an object");
    };
    let is_error = match object.get("isError") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return invalid("isError is not a boolean"),
    };
    let mut text = String::new();
    let mut omitted = 0u64;
    match object.get("content") {
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match (
                    block.get("type").and_then(Value::as_str),
                    block.get("text").and_then(Value::as_str),
                ) {
                    (Some("text"), Some(part)) => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(part);
                    }
                    _ => omitted += 1,
                }
                if text.len() > max_bytes.saturating_mul(2) {
                    break;
                }
            }
        }
        None | Some(Value::Null) => {}
        Some(_) => return invalid("content is not a list"),
    }
    if text.is_empty() {
        if let Some(structured) = object.get("structuredContent") {
            text = canonical_json(structured);
        }
    }
    let truncated = text.len() > max_bytes;
    if truncated {
        let mut cut = max_bytes;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    let mut output = Map::new();
    output.insert("text".to_string(), Value::String(text.clone()));
    output.insert("is_error".to_string(), Value::Bool(is_error));
    output.insert("truncated".to_string(), Value::Bool(truncated));
    output.insert("omitted_blocks".to_string(), json!(omitted));
    output.insert("trust".to_string(), Value::String(RESULT_TRUST.to_string()));
    let output = Value::Object(output);
    if let Some(violation) = first_violation(output_schema, &output) {
        return invalid(&format!(
            "the reduced result does not match the declaration: {} {}",
            violation.path, violation.detail
        ));
    }
    if is_error {
        // The tool ran and reported a failure: the outcome is known and nothing happened.
        let message: String = text.chars().take(512).collect();
        let mut reply = McpReply::failure(tool, "MCP_TOOL_ERROR", &message, false);
        reply.output = output;
        return reply;
    }
    McpReply {
        tool: tool.to_string(),
        status: ReplyStatus::Ok,
        output,
        error: None,
        evidence: None,
        replayed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quansio_tools::mcp::result_schema;

    #[test]
    fn a_result_is_reduced_to_text_and_counts_and_never_more_than_the_bound() {
        let schema = result_schema();
        let result = json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "resource", "resource": {"uri": "file:///x", "text": "hidden"}},
                {"type": "text", "text": "world"}
            ]
        });
        let reply = reduce_reply("mcp.demo.t", &result, 1024, &schema);
        assert_eq!(reply.status, ReplyStatus::Ok);
        assert_eq!(reply.output["text"], json!("hello\nworld"));
        assert_eq!(reply.output["omitted_blocks"], json!(2));
        assert_eq!(reply.output["trust"], json!("UNTRUSTED_EXTERNAL"));
        let long = json!({"content": [{"type": "text", "text": "é".repeat(100)}]});
        let cut = reduce_reply("mcp.demo.t", &long, 11, &schema);
        assert_eq!(cut.output["truncated"], json!(true));
        assert!(
            cut.output["text"].as_str().unwrap().len() <= 11,
            "cut at a character boundary"
        );
        let errored = reduce_reply(
            "mcp.demo.t",
            &json!({"isError": true, "content": [{"type": "text", "text": "nope"}]}),
            100,
            &schema,
        );
        assert_eq!(errored.status, ReplyStatus::Error);
        assert_eq!(errored.ledger_outcome(), "SETTLED_FAILED");
        for bad in [
            json!([]),
            json!({"content": "x"}),
            json!({"isError": "yes"}),
        ] {
            let reply = reduce_reply("mcp.demo.t", &bad, 100, &schema);
            assert_eq!(
                reply.error.as_ref().unwrap()["code"],
                json!("MCP_RESULT_INVALID"),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_request_that_was_sent_and_not_answered_is_in_doubt_unless_the_tool_only_reads() {
        for error in [
            TransportError::Timeout { sent: true },
            TransportError::Closed { sent: true },
            TransportError::TooLarge { sent: true },
        ] {
            let write = failure_reply("mcp.demo.w", &error, false);
            assert_eq!(write.status, ReplyStatus::InDoubt, "{error:?}");
            assert_eq!(write.ledger_outcome(), "OUTCOME_UNKNOWN");
            assert_eq!(write.error.as_ref().unwrap()["retryable"], json!(false));
            let read = failure_reply("mcp.demo.r", &error, true);
            assert_eq!(read.status, ReplyStatus::Error, "{error:?}");
        }
        // Nothing was sent: the outcome is known whatever the tool does.
        let unsent = failure_reply("mcp.demo.w", &TransportError::Closed { sent: false }, false);
        assert_eq!(unsent.status, ReplyStatus::Error);
        let denied = failure_reply(
            "mcp.demo.w",
            &TransportError::Denied("no_grant".into()),
            false,
        );
        assert_eq!(
            denied.error.as_ref().unwrap()["code"],
            json!("EGRESS_DENIED")
        );
    }
}
