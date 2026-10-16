//! The local tool executor: the envelope's tool name and arguments become calls on the tool host
//! (EXEC-002 composition over EXEC-006).
//!
//! [`crate::host::WorkerHost`] validates an [`ActionEnvelope`] and runs it at most once, but it runs
//! nothing itself; it hands the envelope to a [`ToolExecutor`]. This is the production executor for
//! the tools the worker hosts: `fs.read`, `fs.write`, `fs.patch`, `terminal.exec` and `process.spawn`.
//! It parses the declared arguments (unknown fields are refused, as the declarations say), resolves
//! every path through the [`Sandbox`] before any I/O, and answers with one bounded JSON document.
//!
//! The executor decides nothing: capability, policy and approval were settled before the dispatch, and
//! the sandbox's roots are chosen by whoever started the daemon. A tool it does not host is refused, so
//! a `browser.*` envelope sent here fails closed rather than falling into a shell.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use quansio_machine::gateway::ActionEnvelope;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::host::{HostFailure, OutputSink, ToolExecutor, ToolOutput};
use crate::tools::{
    CancelToken, FileHost, ProcessHandle, ProcessHost, RootScope, Sandbox, SpawnRequest,
    ToolContext, ToolError,
};

/// Room left in the output bound for the JSON that wraps a payload, so a bounded payload never makes
/// the document itself exceed the envelope's limit.
const WRAPPER_ROOM: usize = 512;

/// The bound used when an envelope states none (`0` means unbounded on the wire; a worker still keeps
/// a ceiling on what it reads into memory).
const UNBOUNDED_CEILING: usize = 1 << 20;

/// The longest a terminal command may run when the envelope's own deadline allows more.
const DEFAULT_TERMINAL_TIMEOUT: Duration = Duration::from_millis(120_000);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    content_digest: String,
    base_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchArgs {
    path: String,
    patch: String,
    base_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandArgs {
    command: String,
    cwd: Option<String>,
}

/// Runs the tools a worker hosts against a [`Sandbox`].
pub struct LocalToolExecutor {
    sandbox: Sandbox,
    files: FileHost,
    process: ProcessHost,
    tenant_id: String,
    terminal_timeout: Duration,
    spawned: Mutex<Vec<ProcessHandle>>,
}

impl std::fmt::Debug for LocalToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalToolExecutor")
            .field("tenant_id", &self.tenant_id)
            .field("sandbox", &self.sandbox)
            .finish_non_exhaustive()
    }
}

impl LocalToolExecutor {
    /// An executor over `sandbox` for the tenant this worker serves.
    #[must_use]
    pub fn new(sandbox: Sandbox, tenant_id: impl Into<String>) -> Self {
        Self {
            files: FileHost::new(sandbox.clone()),
            sandbox,
            process: ProcessHost::new(),
            tenant_id: tenant_id.into(),
            terminal_timeout: DEFAULT_TERMINAL_TIMEOUT,
            spawned: Mutex::new(Vec::new()),
        }
    }

    /// Use a specific shell for commands.
    #[must_use]
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.process = ProcessHost::with_shell(shell);
        self
    }

    /// Cap how long a terminal command may run (the envelope's deadline can only shorten it).
    #[must_use]
    pub const fn with_terminal_timeout(mut self, timeout: Duration) -> Self {
        self.terminal_timeout = timeout;
        self
    }

    /// The processes started by `process.spawn` that this executor still owns.
    #[must_use]
    pub fn spawned_pids(&self) -> Vec<u32> {
        self.spawned
            .lock()
            .map(|handles| handles.iter().map(ProcessHandle::pid).collect())
            .unwrap_or_default()
    }

    fn context<'a>(&'a self, envelope: &'a ActionEnvelope) -> ToolContext<'a> {
        ToolContext {
            tenant_id: &self.tenant_id,
            target_id: &envelope.target_id,
            run_id: &envelope.run_id,
            step_id: &envelope.step_id,
            // The worker is not told which capability authorized the call and re-derives nothing:
            // the runtime decided that before it dispatched (EXEC-002).
            capability_id: "",
            effect_id: &envelope.effect_id,
            dispatch_token: &envelope.dispatch_token,
            idempotency_key: &envelope.dispatch_token,
        }
    }

    fn payload_bound(envelope: &ActionEnvelope) -> usize {
        let declared = if envelope.max_output_bytes == 0 {
            UNBOUNDED_CEILING
        } else {
            envelope.max_output_bytes
        };
        declared.saturating_sub(WRAPPER_ROOM).max(1)
    }

    /// The envelope's output bound, with `0` meaning the worker's own ceiling.
    fn limit(envelope: &ActionEnvelope) -> usize {
        if envelope.max_output_bytes == 0 {
            UNBOUNDED_CEILING
        } else {
            envelope.max_output_bytes
        }
    }

    /// Fit `document` inside `bound` serialized bytes by shortening its `text_key` string.
    ///
    /// Escaping makes the serialized form larger than the text it carries (a newline is two bytes, a control
    /// byte six), so the text is cut against the finished document rather than against a guess. A cut document
    /// is still one valid JSON value and says `truncated`; the sink's own cut would leave half a string.
    fn fit(mut document: Value, text_key: &str, bound: usize) -> Value {
        if document.to_string().len() <= bound {
            return document;
        }
        let Some(text) = document
            .get(text_key)
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return document;
        };
        let (mut low, mut high) = (0, text.len());
        while low < high {
            let middle = (low + high).div_ceil(2);
            let mut cut = middle;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            document[text_key] = json!(&text[..cut]);
            document["truncated"] = json!(true);
            if document.to_string().len() <= bound {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let mut cut = low;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        document[text_key] = json!(&text[..cut]);
        document["truncated"] = json!(true);
        document
    }

    fn time_left(envelope: &ActionEnvelope, ceiling: Duration) -> Result<Duration, ToolError> {
        let deadline = DateTime::parse_from_rfc3339(&envelope.deadline_at)
            .map_err(|error| ToolError::NotDispatchable {
                detail: format!("deadline_at is not an instant: {error}"),
            })?
            .with_timezone(&Utc);
        let left = (deadline - Utc::now())
            .to_std()
            .map_err(|_| ToolError::NotDispatchable {
                detail: "the deadline has passed".to_string(),
            })?;
        Ok(left.min(ceiling))
    }

    fn parse<T: for<'de> Deserialize<'de>>(envelope: &ActionEnvelope) -> Result<T, ToolError> {
        serde_json::from_str(&envelope.args_json).map_err(|error| ToolError::ArgumentsInvalid {
            tool: envelope.tool.clone(),
            detail: error.to_string(),
        })
    }

    fn read(&self, envelope: &ActionEnvelope) -> Result<Value, ToolError> {
        let args: ReadArgs = Self::parse(envelope)?;
        let read = self.files.read(
            RootScope::Workspace,
            &args.path,
            Self::payload_bound(envelope),
        )?;
        let mut document = json!({
            "path": read.path,
            "scope": read.scope,
            "truncated": read.truncated,
            "size_bytes": read.size_bytes,
            "content_digest": read.content_digest,
        });
        match String::from_utf8(read.content) {
            Ok(text) => {
                document["encoding"] = json!("utf8");
                document["content"] = json!(text);
            }
            // A cut in the middle of a multi-byte character is still text; anything else is bytes the
            // model cannot use as a string, so it is described rather than mangled into one.
            Err(error) => {
                let valid = error.utf8_error().valid_up_to();
                let bytes = error.into_bytes();
                if read.truncated && bytes.len() - valid < 4 {
                    document["encoding"] = json!("utf8");
                    document["content"] = json!(String::from_utf8_lossy(&bytes[..valid]));
                } else {
                    document["encoding"] = json!("binary");
                }
            }
        }
        Ok(Self::fit(document, "content", Self::limit(envelope)))
    }

    fn write(&self, envelope: &ActionEnvelope) -> Result<Value, ToolError> {
        let args: WriteArgs = Self::parse(envelope)?;
        let write = self.files.write_checked(
            &self.context(envelope),
            RootScope::Workspace,
            &args.path,
            args.content.as_bytes(),
            &args.content_digest,
            args.base_digest.as_deref(),
        )?;
        Ok(json!({
            "path": write.path,
            "scope": write.scope,
            "bytes_written": write.bytes_written,
            "content_digest": write.content_digest,
            "replaced": write.replaced,
        }))
    }

    fn patch(&self, envelope: &ActionEnvelope) -> Result<Value, ToolError> {
        let args: PatchArgs = Self::parse(envelope)?;
        let patch = self.files.patch_checked(
            &self.context(envelope),
            RootScope::Workspace,
            &args.path,
            &args.patch,
            args.base_digest.as_deref(),
        )?;
        Ok(json!({
            "path": patch.path,
            "scope": patch.scope,
            "hunks": patch.hunks,
            "added": patch.added,
            "removed": patch.removed,
            "content_digest": patch.content_digest,
        }))
    }

    async fn exec(&self, envelope: &ActionEnvelope) -> Result<Value, ToolError> {
        let args: CommandArgs = Self::parse(envelope)?;
        let cwd = self
            .sandbox
            .resolve(RootScope::Workspace, args.cwd.as_deref().unwrap_or("."))?;
        let timeout = Self::time_left(envelope, self.terminal_timeout)?;
        let mut cancel = CancelToken::new();
        let outcome = self
            .process
            .exec(
                &self.context(envelope),
                &SpawnRequest {
                    command: &args.command,
                    cwd: &cwd,
                    max_output_bytes: Self::payload_bound(envelope),
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                },
                &mut cancel,
            )
            .await?;
        Ok(Self::fit(
            json!({
                "exit_code": outcome.exit_code,
                "succeeded": outcome.succeeded(),
                "timed_out": outcome.timed_out,
                "cancelled": outcome.cancelled,
                "truncated": outcome.truncated,
                "output_bytes": outcome.output_bytes,
                "output": String::from_utf8_lossy(&outcome.output),
            }),
            "output",
            Self::limit(envelope),
        ))
    }

    fn spawn(&self, envelope: &ActionEnvelope) -> Result<Value, ToolError> {
        let args: CommandArgs = Self::parse(envelope)?;
        let cwd = self
            .sandbox
            .resolve(RootScope::Workspace, args.cwd.as_deref().unwrap_or("."))?;
        let timeout = Self::time_left(envelope, self.terminal_timeout)?;
        let handle = self.process.spawn(
            &self.context(envelope),
            &SpawnRequest {
                command: &args.command,
                cwd: &cwd,
                max_output_bytes: Self::payload_bound(envelope),
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            },
        )?;
        let pid = handle.pid();
        // The executor keeps the handle: a process nobody owns is a zombie once it exits.
        self.spawned
            .lock()
            .map_err(|_| ToolError::NotDispatchable {
                detail: "the process table is poisoned".to_string(),
            })?
            .push(handle);
        Ok(json!({ "pid": pid, "command": args.command }))
    }
}

#[async_trait::async_trait]
impl ToolExecutor for LocalToolExecutor {
    async fn run(
        &self,
        envelope: &ActionEnvelope,
        output: &mut OutputSink,
    ) -> Result<ToolOutput, HostFailure> {
        let outcome = match envelope.tool.as_str() {
            "fs.read" => self.read(envelope),
            "fs.write" => self.write(envelope),
            "fs.patch" => self.patch(envelope),
            "terminal.exec" => self.exec(envelope).await,
            "process.spawn" => self.spawn(envelope),
            other => Err(ToolError::UnknownTool(other.to_string())),
        };
        match outcome {
            Ok(document) => {
                output.push(document.to_string().as_bytes());
                Ok(ToolOutput::new())
            }
            Err(error) => Err(HostFailure::ToolFailed {
                tool: envelope.tool.clone(),
                detail: error.to_string(),
            }),
        }
    }
}
