//! Process and terminal-command execution (EXEC-006, `terminal.exec` / `process.spawn`).
//!
//! A command runs under three bounds that come from its declaration, and all three are enforced while
//! it runs rather than checked once it has finished:
//!
//! * **output is bounded as it arrives.** The bound is the declaration's `max_output_bytes`; a command
//!   that floods stdout must not make the worker hold the flood first, which is what reading to the end
//!   and then truncating would do.
//! * **the timeout is a deadline.** A command still running at its deadline is stopped and reported as
//!   [`ToolError::Timeout`], not left running while the worker returns something else.
//! * **cancellation is observed.** [`CancelToken`] is checked while the command runs, so a `cancel`
//!   envelope stops the work — and the child process is killed, because leaving it running would make
//!   "cancelled" a statement about this worker rather than about the effect.
//!
//! The child is started with `std::process` and its pipes are drained on blocking threads, which is what
//! lets the read half respect the bound. Nothing here sleeps or polls: the deadline is a `select!` arm.

use std::process::Stdio;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use super::sandbox::SandboxPath;
use super::{ToolContext, ToolError};

/// A cooperative cancellation signal, shared by the caller that receives a `cancel` envelope and the
/// command that has to stop.
#[derive(Debug, Clone)]
pub struct CancelToken {
    sender: Arc<watch::Sender<bool>>,
    receiver: watch::Receiver<bool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// A fresh, uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(false);
        Self {
            sender: Arc::new(sender),
            receiver,
        }
    }

    /// Cancel. Idempotent, and safe to call from another task.
    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    /// Whether cancellation has been signalled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Wait until cancellation is signalled.
    pub async fn cancelled(&mut self) {
        if self.is_cancelled() {
            return;
        }
        // A closed channel means nobody can cancel any more, which is not a cancellation.
        while self.receiver.changed().await.is_ok() {
            if self.is_cancelled() {
                return;
            }
        }
        std::future::pending::<()>().await;
    }
}

/// What to run.
#[derive(Debug)]
pub struct SpawnRequest<'a> {
    /// The command line, run through the target's shell.
    pub command: &'a str,
    /// The working directory, already proven to be inside its root.
    pub cwd: &'a SandboxPath,
    /// Upper bound on captured output.
    pub max_output_bytes: usize,
    /// How long the command may run.
    pub timeout_ms: u64,
}

/// A started process.
///
/// It owns the child rather than only its pid, because a process nobody waits on becomes a zombie: the
/// pid stays allocated and answers `kill -0` long after the process is gone, so a handle holding only a
/// number could not tell a running process from a reaped one — and nothing would ever reap it.
pub struct ProcessHandle {
    child: std::process::Child,
    /// The command that was started.
    pub command: String,
    /// The effect the start settles.
    pub effect_id: String,
    /// The idempotency key the start carries.
    pub idempotency_key: String,
}

impl std::fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessHandle")
            .field("pid", &self.child.id())
            .field("command", &self.command)
            .field("effect_id", &self.effect_id)
            .finish()
    }
}

impl ProcessHandle {
    /// The operating system's process id, while the process is this host's to reap.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutcome {
    /// The exit code, when the process exited.
    pub exit_code: Option<i32>,
    /// Captured output, truncated to the bound.
    pub output: Vec<u8>,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Total bytes the command produced, whether or not they were kept.
    pub output_bytes: u64,
    /// Whether the command was stopped at its deadline.
    pub timed_out: bool,
    /// Whether the command was cancelled.
    pub cancelled: bool,
    /// The effect the run settled, echoed for the ledger.
    pub effect_id: String,
}

impl ProcessOutcome {
    /// Whether the command succeeded.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && !self.cancelled && self.exit_code == Some(0)
    }
}

/// Runs commands.
#[derive(Debug, Clone, Default)]
pub struct ProcessHost {
    /// The shell commands are run through. An explicit shell rather than a bare `sh` lookup, so the
    /// worker's behaviour does not depend on the target's `PATH`.
    shell: Option<String>,
}

impl ProcessHost {
    /// A host that runs commands through the target's `/bin/sh`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A host that runs commands through a specific shell.
    #[must_use]
    pub fn with_shell(shell: impl Into<String>) -> Self {
        Self {
            shell: Some(shell.into()),
        }
    }

    fn shell(&self) -> &str {
        self.shell.as_deref().unwrap_or("/bin/sh")
    }

    /// Start a process and wait for it, bounded and cancellable.
    ///
    /// # Errors
    /// Returns [`ToolError::SpawnFailed`] when the shell cannot be started,
    /// [`ToolError::Timeout`] when the deadline passed, [`ToolError::Cancelled`] when the token fired,
    /// and [`ToolError::Io`] when a pipe fails.
    pub async fn exec(
        &self,
        context: &ToolContext<'_>,
        request: &SpawnRequest<'_>,
        cancel: &mut CancelToken,
    ) -> Result<ProcessOutcome, ToolError> {
        if request.command.trim().is_empty() {
            return Err(ToolError::ArgumentsInvalid {
                tool: "terminal.exec".to_string(),
                detail: "the command is empty".to_string(),
            });
        }
        if !request.cwd.absolute().is_dir() {
            return Err(ToolError::NotADirectory {
                path: request.cwd.relative().to_string(),
            });
        }
        let mut child = tokio::process::Command::new(self.shell())
            .arg("-c")
            .arg(request.command)
            .current_dir(request.cwd.absolute())
            // The child gets no stdin: a tool call is not an interactive session, and a command that
            // blocks on input would otherwise sit until its deadline.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| ToolError::SpawnFailed {
                detail: error.to_string(),
            })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let bound = request.max_output_bytes;
        let stdout_task = tokio::spawn(read_bounded(stdout, bound));
        let stderr_task = tokio::spawn(read_bounded(stderr, bound));
        let deadline = std::time::Duration::from_millis(request.timeout_ms);

        let (mut timed_out, mut cancelled) = (false, false);
        let status = tokio::select! {
            waited = tokio::time::timeout(deadline, child.wait()) => match waited {
                Ok(Ok(status)) => Some(status),
                Ok(Err(error)) => {
                    return Err(ToolError::Io {
                        path: request.cwd.relative().to_string(),
                        detail: error.to_string(),
                    })
                }
                Err(_) => {
                    timed_out = true;
                    None
                }
            },
            () = cancel.cancelled() => {
                cancelled = true;
                None
            }
        };
        if status.is_none() {
            // Stopping the command is part of the outcome: a worker that returned "timed out" while the
            // process kept running would have reported the wrong thing about the effect.
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        let status = match status {
            Some(status) => Some(status),
            None => child.wait().await.ok(),
        };

        if timed_out || cancelled {
            // The command was stopped, so its output is not what the caller asked for and the pipes are
            // abandoned: a descendant that outlived the kill still holds the write end, and waiting for
            // EOF would block until it exits, which is exactly the thing the deadline just refused to
            // wait for.
            stdout_task.abort();
            stderr_task.abort();
            return Err(if timed_out {
                ToolError::Timeout {
                    timeout_ms: request.timeout_ms,
                }
            } else {
                ToolError::Cancelled
            });
        }

        let (stdout, stdout_truncated, stdout_offered) = join_read(stdout_task, request).await?;
        let (stderr, _, stderr_offered) = join_read(stderr_task, request).await?;

        let mut output = stdout;
        let truncated = stdout_truncated;
        // stderr is appended when the bound leaves room, so a failing command's message is visible
        // without either stream being able to exceed the bound.
        if !stderr.is_empty() && output.len() < bound {
            let room = bound - output.len();
            output.extend_from_slice(&stderr[..stderr.len().min(room)]);
        }
        Ok(ProcessOutcome {
            exit_code: status.and_then(|status| status.code()),
            output,
            truncated: truncated || stderr_offered > 0,
            output_bytes: stdout_offered + stderr_offered,
            timed_out: false,
            cancelled: false,
            effect_id: context.effect_id.to_string(),
        })
    }

    /// Start a long-running process without waiting for it (`process.spawn`).
    ///
    /// The handle is what a later `cancel` needs, and the process is left running on purpose: this is
    /// the tool for a server or a watcher, and its output is the terminal session's business.
    ///
    /// # Errors
    /// Returns [`ToolError::SpawnFailed`] when the shell cannot be started, and
    /// [`ToolError::NotADirectory`] for a working directory that is not one.
    pub fn spawn(
        &self,
        context: &ToolContext<'_>,
        request: &SpawnRequest<'_>,
    ) -> Result<ProcessHandle, ToolError> {
        if request.command.trim().is_empty() {
            return Err(ToolError::ArgumentsInvalid {
                tool: "process.spawn".to_string(),
                detail: "the command is empty".to_string(),
            });
        }
        if !request.cwd.absolute().is_dir() {
            return Err(ToolError::NotADirectory {
                path: request.cwd.relative().to_string(),
            });
        }
        let child = std::process::Command::new(self.shell())
            .arg("-c")
            .arg(request.command)
            .current_dir(request.cwd.absolute())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| ToolError::SpawnFailed {
                detail: error.to_string(),
            })?;
        Ok(ProcessHandle {
            child,
            command: request.command.to_string(),
            effect_id: context.effect_id.to_string(),
            idempotency_key: context.idempotency_key.to_string(),
        })
    }

    /// Stop a process this host started, and reap it.
    ///
    /// The child is signalled *and* waited on, so the pid is released rather than left as a zombie. A
    /// cancel that only signalled would leave a pid that still answers `kill -0`, and a subsequent check
    /// would conclude the process is alive when the kernel has already finished with it.
    ///
    /// Children of the command are not necessarily stopped with it: a shell that spawns a grandchild and
    /// exits leaves that grandchild running. Containing those is the execution target's job — a process
    /// group or cgroup around the target, not a signal from here — and this method does not claim it.
    ///
    /// # Errors
    /// Returns [`ToolError::Io`] when the process cannot be signalled or reaped.
    pub fn cancel(&mut self, handle: &mut ProcessHandle) -> Result<(), ToolError> {
        if handle.child.try_wait().ok().flatten().is_some() {
            return Ok(());
        }
        handle.child.kill().map_err(|error| ToolError::Io {
            path: handle.command.clone(),
            detail: error.to_string(),
        })?;
        handle.child.wait().map_err(|error| ToolError::Io {
            path: handle.command.clone(),
            detail: error.to_string(),
        })?;
        Ok(())
    }
}

/// What a bounded pipe read produced: the kept bytes, whether anything was dropped, and how much was
/// offered in total.
type BoundedRead = (Vec<u8>, bool, u64);

/// Read a pipe into a bounded buffer, counting everything offered.
async fn read_bounded(
    reader: Option<impl AsyncReadExt + Unpin + Send + 'static>,
    bound: usize,
) -> Result<BoundedRead, ToolError> {
    let Some(mut reader) = reader else {
        return Ok((Vec::new(), false, 0));
    };
    let mut kept = Vec::new();
    let mut offered: u64 = 0;
    let mut chunk = [0u8; 4096];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|error| ToolError::Io {
                path: "pipe".to_string(),
                detail: error.to_string(),
            })?;
        if read == 0 {
            break;
        }
        offered += read as u64;
        let room = bound.saturating_sub(kept.len());
        if room > 0 {
            kept.extend_from_slice(&chunk[..read.min(room)]);
        }
    }
    let truncated = offered > kept.len() as u64;
    Ok((kept, truncated, offered))
}

async fn join_read(
    task: tokio::task::JoinHandle<Result<BoundedRead, ToolError>>,
    request: &SpawnRequest<'_>,
) -> Result<BoundedRead, ToolError> {
    task.await.map_err(|error| ToolError::Io {
        path: request.cwd.relative().to_string(),
        detail: error.to_string(),
    })?
}
