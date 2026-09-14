//! The tool host: typed local and guest tools with bounded output and evidence (EXEC-006).
//!
//! qworkerd executes tools; it does not decide whether it may. What reaches this module is a tool name
//! and validated arguments that the runtime already resolved through capability, policy and approval,
//! and what it returns is an outcome plus the evidence the call produced. Two rules follow from that,
//! and both are enforced here:
//!
//! * **a mutating operation carries its context.** [`ToolContext`] names the capability the call runs
//!   under, the effect it settles, the dispatch token that fences it and the idempotency key the runtime
//!   derived. A mutation that cannot say what it settles is not a mutation this host performs, so the
//!   parameter is not optional and there is no overload that omits it.
//! * **the root is chosen, never inferred.** Every file operation names a [`RootScope`] and is refused if
//!   the sandbox has no root for it — so reaching the host filesystem is a decision the runtime made,
//!   not a fallback a tool picked.
//!
//! Output is bounded by the tool declaration's `max_output_bytes` and cancellation is observed while a
//! command runs, not only before it starts.

pub mod files;
pub mod process;
pub mod sandbox;
pub mod terminal;

pub use files::{DirEntry, DirListing, FileHost, FilePatch, FileRead, FileWrite};
pub use process::{CancelToken, ProcessHandle, ProcessHost, ProcessOutcome, SpawnRequest};
pub use sandbox::{PathRefusal, RootScope, Sandbox, SandboxPath};
pub use terminal::{
    advance_cursor, decide, Attachment, TerminalCommand, TerminalConduit, TerminalSession,
    TerminalStatus, TerminalStoreRef, DEFAULT_SCROLLBACK_BYTES,
};

use crate::host::HostFailure;

/// The governing context of one tool call.
///
/// This is the part of the runtime's decision the worker is allowed to know: enough to settle the effect
/// and to prove the call is the one that was authorized, and nothing about *why* it was authorized —
/// the worker evaluates no policy (EXEC-002).
#[derive(Debug, Clone, Copy)]
pub struct ToolContext<'a> {
    /// The tenant.
    pub tenant_id: &'a str,
    /// The execution target.
    pub target_id: &'a str,
    /// The run the call belongs to.
    pub run_id: &'a str,
    /// The step the call is recorded under.
    pub step_id: &'a str,
    /// The capability projection the call runs under.
    pub capability_id: &'a str,
    /// The effect this call settles.
    pub effect_id: &'a str,
    /// The dispatch token that fences it (EXEC-002's reservation).
    pub dispatch_token: &'a str,
    /// The idempotency key the runtime derived for the call.
    pub idempotency_key: &'a str,
}

/// Why a tool call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// The call is not one this host may settle.
    #[error("the call is not dispatchable: {detail}")]
    NotDispatchable {
        /// What is wrong with it.
        detail: String,
    },
    /// The tool is not one this host implements.
    #[error("no tool named {0:?}")]
    UnknownTool(String),
    /// The arguments are not the ones the declaration accepts.
    #[error("{tool} was called with arguments it does not accept: {detail}")]
    ArgumentsInvalid {
        /// The tool.
        tool: String,
        /// What is wrong with them.
        detail: String,
    },
    /// The path was refused before any I/O.
    #[error("{0}")]
    Path(#[from] PathRefusal),
    /// There is no such file or directory.
    #[error("no such path: {path}")]
    NotFound {
        /// The path.
        path: String,
    },
    /// The path is not a regular file.
    #[error("{path} is not a file")]
    NotAFile {
        /// The path.
        path: String,
    },
    /// The path is not a directory.
    #[error("{path} is not a directory")]
    NotADirectory {
        /// The path.
        path: String,
    },
    /// The content does not hash to the digest the call declared.
    #[error("the content hashes to {actual}, not to the declared {declared}")]
    DigestMismatch {
        /// What the call declared.
        declared: String,
        /// What the content hashes to.
        actual: String,
    },
    /// A patch did not apply.
    #[error("the patch did not apply to {path}: {detail}")]
    PatchRejected {
        /// The path.
        path: String,
        /// Why not.
        detail: String,
    },
    /// An I/O operation failed.
    #[error("{path}: {detail}")]
    Io {
        /// The path.
        path: String,
        /// What the operating system said.
        detail: String,
    },
    /// The command did not finish inside its declaration's timeout.
    #[error("the command was still running after {timeout_ms} ms and was stopped")]
    Timeout {
        /// The declared timeout.
        timeout_ms: u64,
    },
    /// The command was cancelled.
    #[error("the command was cancelled")]
    Cancelled,
    /// The command could not be started.
    #[error("the command could not be started: {detail}")]
    SpawnFailed {
        /// What the operating system said.
        detail: String,
    },
    /// The terminal session is not usable.
    #[error("terminal session {id} is {status}")]
    SessionNotOpen {
        /// The session.
        id: String,
        /// Its status.
        status: &'static str,
    },
    /// A cursor was not one this host issued.
    #[error("{cursor:?} is not a cursor this session issued")]
    CursorInvalid {
        /// The cursor.
        cursor: String,
    },
    /// The conduit failed.
    #[error("the terminal conduit failed: {detail}")]
    ConduitFailed {
        /// What went wrong.
        detail: String,
    },
}

impl From<ToolError> for HostFailure {
    fn from(error: ToolError) -> Self {
        HostFailure::ToolFailed {
            tool: String::new(),
            detail: error.to_string(),
        }
    }
}

/// The implementation of one tool.
pub trait Tool {
    /// The registered name (`fs.read`, `terminal.exec`, …).
    fn name(&self) -> &'static str;

    /// Run the tool.
    ///
    /// # Errors
    /// Returns [`ToolError`] naming the rule that refused the call.
    fn run(&self, arguments: &serde_json::Value) -> Result<ToolResult, ToolError>;
}

/// What a tool produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The tool's structured output.
    pub output: serde_json::Value,
    /// Bytes to upload as evidence, when the call captured any.
    pub evidence: Option<ToolEvidence>,
}

/// Evidence a tool captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEvidence {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// The media type.
    pub media_type: String,
}

impl ToolResult {
    /// A result with no evidence.
    #[must_use]
    pub const fn new(output: serde_json::Value) -> Self {
        Self {
            output,
            evidence: None,
        }
    }

    /// Attach evidence.
    #[must_use]
    pub fn with_evidence(mut self, bytes: impl Into<Vec<u8>>, media_type: &str) -> Self {
        self.evidence = Some(ToolEvidence {
            bytes: bytes.into(),
            media_type: media_type.to_string(),
        });
        self
    }
}

/// The tools the runtime registered for this revision, and the map a dispatch is resolved through.
#[derive(Default)]
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Register a tool. A later registration of the same name replaces the earlier one, so a revision
    /// is the set of names currently registered.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools
            .retain(|registered| registered.name() != tool.name());
        self.tools.push(tool);
    }

    /// The registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.tools.iter().map(|tool| tool.name()).collect();
        names.sort_unstable();
        names
    }

    /// Run the tool named by a dispatch.
    ///
    /// # Errors
    /// Returns [`ToolError::UnknownTool`] for a name this revision does not register, and whatever the
    /// tool refused with.
    pub fn run(&self, name: &str, arguments: &serde_json::Value) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .ok_or_else(|| ToolError::UnknownTool(name.to_string()))?;
        tool.run(arguments)
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("tools", &self.names())
            .finish()
    }
}
