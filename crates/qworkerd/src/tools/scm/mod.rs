//! Local Git source-control tools (EXEC-012, `scm.status/diff/branch/commit`).
//!
//! The local read surface works offline in the execution target: every read runs
//! `git` inside a [`SandboxPath`]-resolved repository, so the path policy is not
//! skippable and a call cannot point at another tenant's checkout. Remote writes
//! (`push`/`PR`/`comment`/`merge`) are *not* implemented here: they are remote
//! effects that reserve and settle through the Effect Ledger under
//! approval policy (DOMAIN.md §7.1 — push-to-own-branch allow+log,
//! PR/comment/merge ask by default), and this module only classifies them and
//! derives their idempotency key so a retry of the same write is the same call.

use std::path::Path;
use std::process::Command;

use quansio_core::Digest;

use super::sandbox::SandboxPath;
use super::{ToolError, ToolResult};

/// How `git` is invoked. Sealed so tests can point it at a fixture repository
/// while production always uses the sandbox-resolved path.
#[derive(Debug, Clone, Copy)]
pub struct Git<'a> {
    repo: &'a Path,
}

/// One local read result, bounded like every other tool output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRead {
    /// The git subcommand that produced this output.
    pub op: &'static str,
    /// The repository the op ran in (the sandbox-relative root).
    pub repo: String,
    /// Raw stdout, already bounded by the caller's budget.
    pub output: String,
    /// Whether the output was truncated to the bound.
    pub truncated: bool,
}

/// The remote-write classification (DOMAIN.md §7.1 defaults for SCM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteWrite {
    /// Push to the caller's own branch: allow + log.
    PushOwnBranch,
    /// Open or comment on a PR: ask by default.
    PullRequest,
    /// Merge a PR: ask by default.
    Merge,
}

impl RemoteWrite {
    /// The policy default for this write.
    #[must_use]
    pub const fn default_decision(self) -> &'static str {
        match self {
            Self::PushOwnBranch => "allow_log",
            Self::PullRequest | Self::Merge => "ask",
        }
    }

    /// The op name the Effect Ledger sees.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PushOwnBranch => "scm.remote.push",
            Self::PullRequest => "scm.remote.pr",
            Self::Merge => "scm.remote.merge",
        }
    }
}

/// Derive the idempotency key a remote write would reserve under: the same
/// (op, repo, ref-shape) is the same call, so a retry is a replay of one
/// EffectRecord rather than a second write.
#[must_use]
pub fn remote_write_key(write: RemoteWrite, repo: &str, target: &str) -> String {
    let material = format!("{}\u{0}{}\u{0}{}", write.as_str(), repo, target);
    Digest::of(material.as_bytes()).as_str().to_string()
}

impl<'a> Git<'a> {
    /// Bind git to a sandbox-resolved repository root.
    #[must_use]
    pub fn in_repo(repo: &'a SandboxPath) -> Self {
        Self {
            repo: repo.absolute(),
        }
    }

    fn run(&self, args: &[&str], budget: usize) -> Result<GitRead, ToolError> {
        let op: &'static str = match args.first().copied() {
            Some("status") => "scm.status",
            Some("diff") => "scm.diff",
            Some("branch") => "scm.branch",
            Some("log") => "scm.log",
            Some("commit") => "scm.commit",
            Some("worktree") => "scm.worktree",
            Some(other) => {
                return Err(ToolError::NotDispatchable {
                    detail: format!(
                        "unsupported local git op {other}; remote writes go through the ledger"
                    ),
                });
            }
            None => {
                return Err(ToolError::ArgumentsInvalid {
                    tool: "scm".to_string(),
                    detail: "a git op is required".to_string(),
                })
            }
        };
        let output = Command::new("git")
            .arg("-C")
            .arg(self.repo)
            .args(args)
            .output()
            .map_err(|error| ToolError::SpawnFailed {
                detail: format!("git {}: {error}", args[0]),
            })?;
        if !output.status.success() {
            return Err(ToolError::ConduitFailed {
                detail: format!(
                    "git {}: {}",
                    args[0],
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let truncated = raw.len() > budget;
        let cut = if truncated { budget } else { raw.len() };
        Ok(GitRead {
            op,
            repo: self.repo.display().to_string(),
            output: raw[..cut].to_string(),
            truncated,
        })
    }

    /// `scm.status` — working-tree state, offline.
    ///
    /// # Errors
    /// [`ToolError::Policy`] when the repository is unreadable.
    pub fn status(&self, budget: usize) -> Result<GitRead, ToolError> {
        self.run(&["status", "--porcelain=v1", "-b"], budget)
    }

    /// `scm.diff` — unstaged diff against HEAD.
    ///
    /// # Errors
    /// [`ToolError::Policy`] when the repository is unreadable.
    pub fn diff(&self, budget: usize) -> Result<GitRead, ToolError> {
        self.run(&["diff"], budget)
    }

    /// `scm.branch` — local branches with their tips.
    ///
    /// # Errors
    /// [`ToolError::Policy`] when the repository is unreadable.
    pub fn branches(&self, budget: usize) -> Result<GitRead, ToolError> {
        self.run(&["branch", "-v", "--no-abbrev"], budget)
    }

    /// `scm.commit` — create a commit on the current branch. This is a *local*
    /// write inside the execution target: it mutates the checkout, so it takes
    /// the [`ToolContext`] and settles the caller's effect like any mutating tool.
    ///
    /// # Errors
    /// [`ToolError::Policy`] when there is nothing to commit or git refuses.
    pub fn commit(
        &self,
        ctx: &super::ToolContext<'_>,
        message: &str,
        budget: usize,
    ) -> Result<GitRead, ToolError> {
        if message.trim().is_empty() {
            return Err(ToolError::ArgumentsInvalid {
                tool: "scm.commit".to_string(),
                detail: "a commit needs a message".to_string(),
            });
        }
        let staged = self.run(&["diff", "--cached", "--name-only"], budget)?;
        if staged.output.trim().is_empty() {
            return Err(ToolError::ArgumentsInvalid {
                tool: "scm.commit".to_string(),
                detail: "nothing staged; refusing an empty commit".to_string(),
            });
        }
        let read = self.run(&["commit", "-m", message], budget)?;
        Ok(GitRead {
            op: "scm.commit",
            repo: read.repo,
            output: format!("effect={} {}", ctx.effect_id, read.output),
            truncated: read.truncated,
        })
    }

    /// `scm.worktree` — list worktrees linked to this checkout (the worktree test's surface).
    ///
    /// # Errors
    /// [`ToolError::Policy`] when the repository is unreadable.
    pub fn worktrees(&self, budget: usize) -> Result<GitRead, ToolError> {
        self.run(&["worktree", "list", "--porcelain"], budget)
    }
}

/// Result shape alias kept for the tool-registry surface.
pub type ScmResult = ToolResult;

/// Repository path of this module's canonical owner.
pub const SCM_OWNER: &str = "crates/qworkerd/src/tools/scm";
