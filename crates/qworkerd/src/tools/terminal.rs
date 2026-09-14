//! Terminal sessions and their replay contract (EXEC-006, DOMAIN.md §8.5).
//!
//! A terminal session is a byte stream with a durable cursor: the control plane keeps the session row
//! — `pty_ref`, `cursor`, `status`, `last_command_id` — and this module is the worker's half, the
//! conduit that produces the bytes and the rule that decides whether an attach runs a command.
//!
//! The rule is the acceptance statement, and it is one decision:
//!
//! * an attach whose command is the session's `last_command_id` is a **replay**. The command already ran;
//!   running it again would be a duplicate effect, so the attach resumes from the durable cursor and the
//!   caller is told which offset to read from.
//! * an attach naming a different command is a **dispatch**. The command runs once, its output is emitted
//!   from the session's cursor, and the session records the new command id — which is what makes the next
//!   attach of the same command a replay.
//!
//! The cursor is monotonic: bytes are numbered absolutely, and a caller cannot move it backwards. That
//! is what lets a reconnecting client ask for "everything after what I have" and be given exactly that,
//! rather than a re-send it would have to de-duplicate itself.
//!
//! The conduit is a trait because terminal *semantics* are the target's business — a pipe is enough for a
//! command's output, a PTY adds a line discipline and a window size — and the replay contract above is
//! the same either way. The shipped conduit is pipe-backed; see this task's evidence notes.

use std::path::PathBuf;

use super::sandbox::SandboxPath;
use super::{ToolContext, ToolError};

/// How much session output the worker keeps for a replay that asks for an offset it no longer holds.
pub const DEFAULT_SCROLLBACK_BYTES: usize = 262_144;

/// A terminal session's lifecycle (the schema's `terminal_sessions.status` vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalStatus {
    /// Usable.
    Open,
    /// Closed by the caller.
    Closed,
    /// The conduit died; the session is not usable and its command is not re-run.
    Lost,
}

impl TerminalStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Lost => "lost",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }

    /// Whether a command may run on a session in this state.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// The session state the control plane holds, as the worker sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSession {
    /// `tsn_…` identity.
    pub id: String,
    /// The execution target it runs on.
    pub target_id: String,
    /// The run it belongs to, when it belongs to one.
    pub run_id: Option<String>,
    /// The conduit's reference, when the target has one.
    pub pty_ref: Option<String>,
    /// The durable byte offset the client has consumed up to.
    pub cursor: u64,
    /// The lifecycle state.
    pub status: TerminalStatus,
    /// The last command dispatched on this session.
    pub last_command_id: Option<String>,
}

impl TerminalSession {
    /// An open session at the start of its stream.
    #[must_use]
    pub fn open(id: impl Into<String>, target_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            target_id: target_id.into(),
            run_id: None,
            pty_ref: None,
            cursor: 0,
            status: TerminalStatus::Open,
            last_command_id: None,
        }
    }
}

/// A command an attach asks the session to run.
#[derive(Debug, Clone)]
pub struct TerminalCommand<'a> {
    /// The command's identity. Supplied by the runtime, so a retry of the same call carries the same id
    /// — which is what makes replay recognisable.
    pub command_id: &'a str,
    /// The command line.
    pub command: &'a str,
    /// The working directory.
    pub cwd: PathBuf,
    /// Upper bound on captured output.
    pub max_output_bytes: usize,
    /// How long the command may run.
    pub timeout_ms: u64,
}

/// What an attach decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// The command already ran; read the stream from `from`.
    Replay {
        /// The command that already ran.
        command_id: String,
        /// The absolute offset to read from — the session's durable cursor.
        from: u64,
    },
    /// The command must run.
    Dispatch {
        /// The command to run.
        command_id: String,
        /// The absolute offset its output starts at.
        from: u64,
    },
}

/// Decide whether an attach runs a command, and from which offset its output is read.
///
/// This is the whole replay rule and it is pure, so the decision can be tested without a conduit and the
/// conduit cannot influence it.
///
/// # Errors
/// Returns [`ToolError::SessionNotOpen`] for a session that is not open and
/// [`ToolError::ArgumentsInvalid`] for an empty command id or line.
pub fn decide(
    session: &TerminalSession,
    command: &TerminalCommand<'_>,
) -> Result<Attachment, ToolError> {
    if !session.status.is_open() {
        return Err(ToolError::SessionNotOpen {
            id: session.id.clone(),
            status: session.status.as_str(),
        });
    }
    if command.command_id.trim().is_empty() || command.command.trim().is_empty() {
        return Err(ToolError::ArgumentsInvalid {
            tool: "terminal.exec".to_string(),
            detail: "the command id and line are both required".to_string(),
        });
    }
    Ok(
        if session.last_command_id.as_deref() == Some(command.command_id) {
            Attachment::Replay {
                command_id: command.command_id.to_string(),
                from: session.cursor,
            }
        } else {
            Attachment::Dispatch {
                command_id: command.command_id.to_string(),
                from: session.cursor,
            }
        },
    )
}

/// Move a cursor forward, refusing a backwards or out-of-range move.
///
/// # Errors
/// Returns [`ToolError::CursorInvalid`] when the requested offset is behind the durable one, which is a
/// client asking to be given bytes it has already consumed.
pub fn advance_cursor(session: &TerminalSession, to: u64) -> Result<u64, ToolError> {
    if to < session.cursor {
        return Err(ToolError::CursorInvalid {
            cursor: to.to_string(),
        });
    }
    Ok(to)
}

/// A Conduit that reads what a terminal has emitted since an offset.
///
/// Implemented by whichever mechanism the target provides: a pipe from a command, or a PTY. The contract
/// is the same, and it is deliberately *not* about how the bytes were produced.
pub trait TerminalConduit: Send + Sync {
    /// The conduit's reference, recorded on the session as `pty_ref`.
    fn reference(&self) -> Option<String>;

    /// What the terminal has emitted between `from` and the current end of the stream, and the offset
    /// the stream is now at. Returning the offset rather than appending to it keeps the numbering in one
    /// place, so a dropped chunk cannot silently renumber the rest.
    ///
    /// # Errors
    /// Returns [`ToolError::ConduitFailed`] when the conduit cannot be read.
    fn read_from(&self, from: u64, max_bytes: usize) -> Result<(Vec<u8>, u64), ToolError>;

    /// Stop the conduit.
    ///
    /// # Errors
    /// Returns [`ToolError::ConduitFailed`] when it cannot be stopped. Stopping is best-effort by nature
    /// — the process may already be gone — so a caller reports a failure rather than retrying forever.
    fn close(&self) -> Result<(), ToolError>;
}

/// The parts of a session the worker needs to run a command, kept together so the replay decision and
/// the durable record cannot be applied to different sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalStoreRef {
    /// The session.
    pub session: TerminalSession,
    /// The effect a dispatch on this session settles.
    pub effect_id: String,
}

impl TerminalStoreRef {
    /// A reference to a session.
    #[must_use]
    pub fn new(session: TerminalSession, effect_id: impl Into<String>) -> Self {
        Self {
            session,
            effect_id: effect_id.into(),
        }
    }

    /// The context a command on this session runs under.
    #[must_use]
    pub fn context<'a>(&'a self, tenant_id: &'a str, idempotency_key: &'a str) -> ToolContext<'a> {
        ToolContext {
            tenant_id,
            target_id: &self.session.target_id,
            run_id: self.session.run_id.as_deref().unwrap_or(""),
            step_id: "",
            capability_id: "",
            effect_id: &self.effect_id,
            dispatch_token: "",
            idempotency_key,
        }
    }
}

/// A path a terminal command runs from, proven to be inside its root.
#[derive(Debug, Clone)]
pub struct TerminalCwd {
    /// The proven path.
    pub path: SandboxPath,
}

impl TerminalCwd {
    /// Wrap a proven path.
    #[must_use]
    pub const fn new(path: SandboxPath) -> Self {
        Self { path }
    }
}
