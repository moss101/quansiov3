//! Worktree retention: two-phase cleanup with a recovery snapshot (ADP-005, DOMAIN.md §8.7).
//!
//! An agent that works on a repository does so in a **managed worktree**: a Git worktree the runtime
//! creates under the target's managed root, never inside the user's own checkout. This module decides
//! when one may be removed and makes sure that removing it never loses the person's work:
//!
//! * **two phases.** [`WorktreeRetention::prepare`] reads each worktree's revision, `HEAD`, activity and
//!   a fingerprint of its live index and status, and answers a 64-hex **retention token** bound to the
//!   exact set and those states. [`WorktreeRetention::cleanup`] takes the token and the same ids and
//!   accounts for **every** id as removed or skipped with a reason, rechecking each worktree against what
//!   was previewed immediately before it removes it.
//! * **a recovery snapshot first.** A manual cleanup writes private refs under
//!   `refs/quansio/recovery/<id>/` — the original `HEAD`, an index snapshot commit built from the live
//!   index with `git write-tree`, and a code snapshot commit built from the working tree with a
//!   **temporary index file** — and a durable recovery record, and only then removes the checkout. It
//!   never runs `git stash`, never writes the worktree's own index, never creates or moves a branch or a
//!   tag and never touches a ref outside its namespace.
//! * **an automatic sweep removes only what has nothing to lose:** idle, fully clean worktrees beyond
//!   `max_resident`, oldest first.
//! * **crash recovery.** State is a record file per worktree (written atomically) and the refs. Removal
//!   never starts before the recovery is complete, and [`WorktreeRetention::recover`] resolves whatever a
//!   crash left: a worktree is either still intact or restorable with [`WorktreeRetention::restore`].
//!
//! Every Git call goes through one function that clears the repository-selecting environment, disables
//! optional locks (a status read must not rewrite an index) and hooks, and fixes the commit identity, so
//! the result does not depend on the person's Git configuration. Retention is host-callable (a runtime
//! or a person's command), not a Tool the model sees; its policy is `config/worktree-retention.yaml`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use quansio_core::Digest;
use serde::{Deserialize, Serialize};

use super::super::sandbox::{RootScope, Sandbox};

/// The shipped policy, `config/worktree-retention.yaml`.
const SHIPPED_CONFIG: &str = include_str!("../../../../../config/worktree-retention.yaml");

/// The most worktrees one `prepare` may name.
pub const MAX_IDS: usize = 256;

/// The domain separator of a retention token.
const TOKEN_DOMAIN: &str = "quansio.worktree.retention.v1";

// --------------------------------------------------------------------------- configuration

/// Retention policy (`config/worktree-retention.yaml`, validated against its JSON schema).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionConfig {
    /// How long a prepared set is honoured.
    pub prepare_ttl_ms: u64,
    /// More managed worktrees than this makes the oldest idle clean ones candidates for the sweep.
    pub max_resident: usize,
    /// Where recovery refs live, e.g. `refs/quansio/recovery`.
    pub ref_namespace: String,
    /// The plain directory name, under the sandbox's workspace root, that holds every managed worktree.
    pub managed_dir: String,
    /// The directory under the managed root that holds records and prepared sets.
    pub state_dir: String,
    /// A worktree must have been idle this long before the sweep may remove it.
    pub sweep_min_idle_ms: u64,
    /// The sweep removes at most this many worktrees per run.
    pub sweep_max_per_run: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    prepare_ttl_ms: u64,
    max_resident: usize,
    ref_namespace: String,
    managed_dir: String,
    state_dir: String,
    sweep: RawSweep,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSweep {
    min_idle_ms: u64,
    max_per_run: usize,
}

impl RetentionConfig {
    /// The policy this build ships.
    ///
    /// # Panics
    /// Only if the committed `config/worktree-retention.yaml` is invalid, which the tests make impossible.
    #[must_use]
    pub fn shipped() -> Self {
        Self::from_yaml(SHIPPED_CONFIG).expect("config/worktree-retention.yaml is valid")
    }

    /// Parse and validate a policy document, with the bounds of its JSON schema.
    ///
    /// # Errors
    /// [`RetentionError::Config`] for an unparseable document, an unknown key, a value out of bounds, or a
    /// ref namespace that is not under `refs/quansio/` (a branch or tag namespace is never acceptable).
    pub fn from_yaml(text: &str) -> Result<Self, RetentionError> {
        let raw: RawConfig = serde_yaml::from_str(text)
            .map_err(|error| RetentionError::Config(error.to_string()))?;
        let bad = |what: &str| Err(RetentionError::Config(what.to_string()));
        if raw.version != 1 {
            return bad("version must be 1");
        }
        if !(1_000..=86_400_000).contains(&raw.prepare_ttl_ms) {
            return bad("prepare_ttl_ms must be between 1000 and 86400000");
        }
        if !(1..=256).contains(&raw.max_resident) {
            return bad("max_resident must be between 1 and 256");
        }
        if !valid_namespace(&raw.ref_namespace) {
            return bad("ref_namespace must be a refs/quansio/ namespace");
        }
        if !valid_plain(&raw.managed_dir, false) {
            return bad("managed_dir must be a plain lower-case name");
        }
        if !valid_plain(&raw.state_dir, true) {
            return bad("state_dir must be a plain dot-name");
        }
        if !(60_000..=2_592_000_000).contains(&raw.sweep.min_idle_ms) {
            return bad("sweep.min_idle_ms must be between 60000 and 2592000000");
        }
        if !(1..=64).contains(&raw.sweep.max_per_run) {
            return bad("sweep.max_per_run must be between 1 and 64");
        }
        Ok(Self {
            prepare_ttl_ms: raw.prepare_ttl_ms,
            max_resident: raw.max_resident,
            ref_namespace: raw.ref_namespace,
            managed_dir: raw.managed_dir,
            state_dir: raw.state_dir,
            sweep_min_idle_ms: raw.sweep.min_idle_ms,
            sweep_max_per_run: raw.sweep.max_per_run,
        })
    }
}

fn valid_namespace(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("refs/quansio/") else {
        return false;
    };
    value.len() <= 96
        && !rest.is_empty()
        && rest.split('/').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
}

fn valid_plain(value: &str, dotted: bool) -> bool {
    let body = if dotted {
        match value.strip_prefix('.') {
            Some(body) => body,
            None => return false,
        }
    } else {
        value
    };
    value.len() <= 32
        && body.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A worktree id: a runtime-chosen slug local to one target (`^[a-z0-9][a-z0-9_-]{0,62}$`).
#[must_use]
pub fn valid_worktree_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 63
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

// --------------------------------------------------------------------------------- errors

/// Why a retention call was refused or failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RetentionError {
    /// The policy document is not acceptable.
    #[error("worktree retention configuration: {0}")]
    Config(String),
    /// A worktree id is not a slug.
    #[error("{0:?} is not a worktree id")]
    InvalidId(String),
    /// No managed worktree has this id.
    #[error("no managed worktree {id}")]
    NotFound {
        /// The id.
        id: String,
    },
    /// A managed worktree's checkout is gone and a recovery record names it (`WORKTREE_MISSING`).
    #[error("WORKTREE_MISSING: the checkout of {id} is gone (state {state}); see the recovery record {recovery_record}")]
    WorktreeMissing {
        /// The id.
        id: String,
        /// The recorded state.
        state: String,
        /// The recovery record's path.
        recovery_record: String,
    },
    /// The retention token is not usable (`WORKTREE_TOKEN_INVALID`).
    #[error("WORKTREE_TOKEN_INVALID: {reason}")]
    TokenInvalid {
        /// `unknown`, `expired` or `set_mismatch`.
        reason: &'static str,
    },
    /// The id is already used by a worktree this host manages.
    #[error("the worktree id {id} is already in use")]
    IdInUse {
        /// The id.
        id: String,
    },
    /// The path a restore would create is occupied.
    #[error("{path} is occupied; a restore does not overwrite anything")]
    PathOccupied {
        /// The path.
        path: String,
    },
    /// The worktree cannot be restored from its state.
    #[error("{id} cannot be restored from state {state}")]
    NotRestorable {
        /// The id.
        id: String,
        /// The state it is in.
        state: String,
    },
    /// A Git command failed.
    #[error("git {op}: {detail}")]
    Git {
        /// The operation.
        op: String,
        /// What Git said.
        detail: String,
    },
    /// A file operation failed.
    #[error("{path}: {detail}")]
    Io {
        /// The path.
        path: String,
        /// What the operating system said.
        detail: String,
    },
    /// A record on disk is not one this host wrote.
    #[error("the record {path} is not readable: {detail}")]
    Corrupt {
        /// The record.
        path: String,
        /// Why.
        detail: String,
    },
}

impl RetentionError {
    /// The §15 code this refusal carries, when it has one.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::WorktreeMissing { .. } => Some("WORKTREE_MISSING"),
            Self::TokenInvalid { .. } => Some("WORKTREE_TOKEN_INVALID"),
            _ => None,
        }
    }
}

fn io_error(path: &Path, error: &std::io::Error) -> RetentionError {
    RetentionError::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    }
}

// -------------------------------------------------------------------------------- records

/// A managed worktree's state (DOMAIN.md §8.7 A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorktreeState {
    /// No Run holds it.
    Idle,
    /// A Run holds it.
    Active,
    /// A cleanup has begun; resolved by `recover`.
    Removing,
    /// The checkout is gone; the recovery record and refs remain.
    Removed,
}

impl WorktreeState {
    /// The canonical name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Active => "ACTIVE",
            Self::Removing => "REMOVING",
            Self::Removed => "REMOVED",
        }
    }
}

/// How a worktree is being removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalMode {
    /// A person's cleanup: may remove changes, after a recovery snapshot.
    Manual,
    /// The automatic sweep: only fully clean worktrees.
    Auto,
}

/// What a Run holds on a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    /// Runs holding the worktree.
    pub holders: u32,
    /// When it was last held, released or created (Unix milliseconds).
    pub last_activity_at_ms: u64,
}

/// The recovery state of a worktree (DOMAIN.md §8.7 C).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    /// The ref naming the original `HEAD` commit.
    pub head_ref: String,
    /// The ref naming the index snapshot commit (manual cleanup only).
    pub index_ref: Option<String>,
    /// The ref naming the code snapshot commit (manual cleanup only).
    pub snapshot_ref: Option<String>,
    /// When the refs were written (Unix milliseconds).
    pub taken_at_ms: u64,
    /// True only after every ref exists and names the commit written.
    pub complete: bool,
    /// How many ignored files the snapshot does not hold.
    pub ignored_files: u64,
}

/// A removal in progress or finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal {
    /// Manual or automatic.
    pub mode: RemovalMode,
    /// When it began (Unix milliseconds).
    pub started_at_ms: u64,
    /// The live fingerprint verified immediately before it began.
    pub fingerprint: String,
    /// When it finished.
    pub finished_at_ms: Option<u64>,
}

/// The durable record of one managed worktree (`<managed root>/.quansio/worktrees/<id>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRecord {
    /// The worktree id.
    pub id: String,
    /// The repository's working directory the worktree was created from.
    pub repo: String,
    /// The checkout's path.
    pub path: String,
    /// The branch `HEAD` was attached to when last observed, if any.
    pub branch: Option<String>,
    /// The commit `HEAD` named when last observed.
    pub head: String,
    /// Increases on every recorded change of `head`, `activity` or `state`.
    pub revision: u64,
    /// What Runs hold it.
    pub activity: Activity,
    /// Its state.
    pub state: WorktreeState,
    /// When it was created (Unix milliseconds).
    pub created_at_ms: u64,
    /// The recovery state, once any was taken.
    pub recovery: Option<Recovery>,
    /// The removal, once one began.
    pub removal: Option<Removal>,
}

/// What `prepare` saw of one worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedEntry {
    /// The worktree id.
    pub id: String,
    /// Whether a record exists.
    pub exists: bool,
    /// The recorded state (`NONE` when there is no record).
    pub state: String,
    /// `HEAD` as read from the checkout (empty when the checkout is missing).
    pub head: String,
    /// The record's revision.
    pub revision: u64,
    /// Runs holding it.
    pub holders: u32,
    /// Its last activity (Unix milliseconds).
    pub last_activity_at_ms: u64,
    /// Fingerprint of the live index and non-ignored status (empty when the checkout is missing).
    pub fingerprint: String,
    /// Whether it has staged, unstaged or untracked changes.
    pub dirty: bool,
    /// Whether the index has unmerged entries.
    pub unmerged: bool,
}

/// A previewed set, durable until its cleanup is fully accounted for (or it expires).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedSet {
    /// The 64-hex retention token.
    pub token: String,
    /// When it was prepared (Unix milliseconds).
    pub prepared_at_ms: u64,
    /// When it stops being honoured (Unix milliseconds).
    pub expires_at_ms: u64,
    /// What each worktree looked like, sorted by id.
    pub entries: Vec<PreparedEntry>,
}

impl PreparedSet {
    /// The ids, sorted.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.entries.iter().map(|entry| entry.id.clone()).collect()
    }
}

/// Why a worktree was skipped by a cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// No managed worktree has the id.
    NotFound,
    /// It was removed already.
    AlreadyRemoved,
    /// Another cleanup holds it.
    Busy,
    /// A Run holds it.
    Active,
    /// Its record changed since the preview.
    RevisionChanged,
    /// `HEAD` moved since the preview.
    HeadMoved,
    /// Its index or status changed since the preview.
    IndexChanged,
    /// Its index has unmerged entries.
    UnmergedIndex,
    /// The automatic sweep found changes.
    NotClean,
    /// The recovery snapshot could not be taken.
    RecoveryFailed,
    /// Git could not remove the checkout.
    RemovalFailed,
}

impl SkipReason {
    /// The canonical name (DOMAIN.md §8.7 B).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::AlreadyRemoved => "already_removed",
            Self::Busy => "busy",
            Self::Active => "active",
            Self::RevisionChanged => "revision_changed",
            Self::HeadMoved => "head_moved",
            Self::IndexChanged => "index_changed",
            Self::UnmergedIndex => "unmerged_index",
            Self::NotClean => "not_clean",
            Self::RecoveryFailed => "recovery_failed",
            Self::RemovalFailed => "removal_failed",
        }
    }
}

/// What a cleanup did with one id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The checkout was removed.
    Removed,
    /// It was left exactly as it was, for this reason.
    Skipped {
        /// The rule.
        reason: SkipReason,
        /// Detail for a person, never a path outside the managed root.
        detail: String,
    },
}

/// A cleanup's accounting: one outcome per requested id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    /// The token the cleanup ran under.
    pub token: String,
    /// Manual or automatic.
    pub mode: RemovalMode,
    /// One entry per requested id, sorted by id.
    pub outcomes: Vec<(String, Outcome)>,
}

impl CleanupReport {
    /// The effect class this cleanup settles.
    #[must_use]
    pub const fn effect_class(&self) -> &'static str {
        RetentionOp::removal(self.mode).effect_class()
    }

    /// The ids that were removed.
    #[must_use]
    pub fn removed(&self) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|(_, outcome)| matches!(outcome, Outcome::Removed))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// The ids that were skipped, with their reasons.
    #[must_use]
    pub fn skipped(&self) -> Vec<(&str, SkipReason)> {
        self.outcomes
            .iter()
            .filter_map(|(id, outcome)| match outcome {
                Outcome::Skipped { reason, .. } => Some((id.as_str(), *reason)),
                Outcome::Removed => None,
            })
            .collect()
    }

    /// Whether every id in `ids` has exactly one outcome and no other id does.
    #[must_use]
    pub fn accounts_for(&self, ids: &[String]) -> bool {
        let wanted: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
        let got: Vec<&str> = self.outcomes.iter().map(|(id, _)| id.as_str()).collect();
        let unique: BTreeSet<&str> = got.iter().copied().collect();
        got.len() == unique.len() && unique == wanted
    }
}

/// What `recover` resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Removals that were finished (the checkout was intact and unchanged, or already damaged or gone).
    pub completed: Vec<String>,
    /// Removals that were abandoned and the worktree returned to `IDLE` (nothing had been removed).
    pub rolled_back: Vec<String>,
}

/// An operation of the retention host and the effect class it settles (DOMAIN.md §8.7 G). The map is
/// `config/effects.yaml: worktree_operations`, and a test holds this enum to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionOp {
    /// Creating a managed worktree.
    Create,
    /// Previewing a set; it changes nothing.
    Prepare,
    /// A person's cleanup.
    CleanupManual,
    /// The automatic sweep.
    CleanupAuto,
    /// Restoring a removed worktree.
    Restore,
}

impl RetentionOp {
    /// Every operation.
    pub const ALL: [Self; 5] = [
        Self::Create,
        Self::Prepare,
        Self::CleanupManual,
        Self::CleanupAuto,
        Self::Restore,
    ];

    /// The key in `worktree_operations`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Prepare => "prepare",
            Self::CleanupManual => "cleanup_manual",
            Self::CleanupAuto => "cleanup_auto",
            Self::Restore => "restore",
        }
    }

    /// The effect class the operation settles.
    #[must_use]
    pub const fn effect_class(self) -> &'static str {
        match self {
            Self::Create | Self::Restore => "fs.write.workspace",
            Self::Prepare => "read.internal",
            Self::CleanupManual | Self::CleanupAuto => "fs.delete.workspace",
        }
    }

    /// The removal operation for a mode.
    #[must_use]
    pub const fn removal(mode: RemovalMode) -> Self {
        match mode {
            RemovalMode::Manual => Self::CleanupManual,
            RemovalMode::Auto => Self::CleanupAuto,
        }
    }
}

/// A point in a cleanup, for a crash test to stop at (a test seam; production sets none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// A prepared set was written.
    PrepareWritten,
    /// The recovery refs were written; the record does not yet say they are complete.
    RefsWritten,
    /// The record says the recovery is complete; the worktree is still `IDLE`.
    RecoveryRecorded,
    /// The record says `REMOVING`; nothing is removed yet.
    RemovingRecorded,
    /// Git removed the checkout; the record still says `REMOVING`.
    CheckoutRemoved,
}

type Hook = Arc<dyn Fn(Stage, &str) + Send + Sync>;
type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

// ------------------------------------------------------------------------------ the host

/// The retention host of one target's managed root.
#[derive(Clone)]
pub struct WorktreeRetention {
    root: PathBuf,
    state: PathBuf,
    config: RetentionConfig,
    clock: Clock,
    hook: Option<Hook>,
    lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for WorktreeRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorktreeRetention")
            .field("root", &self.root)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// The wall clock, in Unix milliseconds.
fn system_clock() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

impl WorktreeRetention {
    /// A host over the managed directory of the sandbox's workspace root. The directory is created if it
    /// does not exist and resolved through the sandbox, so it cannot be a link that leaves the root.
    ///
    /// # Errors
    /// [`RetentionError::Config`] when the sandbox has no workspace root or the managed directory
    /// resolves outside it, and [`RetentionError::Io`] when it cannot be created.
    pub fn new(sandbox: &Sandbox, config: RetentionConfig) -> Result<Self, RetentionError> {
        let resolve = || {
            sandbox
                .resolve(RootScope::Workspace, &config.managed_dir)
                .map_err(|error| RetentionError::Config(error.to_string()))
        };
        let first = resolve()?;
        std::fs::create_dir_all(first.absolute())
            .map_err(|error| io_error(first.absolute(), &error))?;
        let root = resolve()?.absolute().to_path_buf();
        let state = root.join(&config.state_dir);
        for directory in ["worktrees", "prepared", "tmp"] {
            let path = state.join(directory);
            std::fs::create_dir_all(&path).map_err(|error| io_error(&path, &error))?;
        }
        Ok(Self {
            root,
            state,
            config,
            clock: Arc::new(system_clock),
            hook: None,
            lock: Arc::new(Mutex::new(())),
        })
    }

    /// Replace the clock (Unix milliseconds); tests use it to age a worktree without waiting.
    #[must_use]
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Call `hook(stage, id)` at each [`Stage`] (a test seam: how a crash test stops a real process
    /// at an exact point). Production code sets none.
    #[must_use]
    pub fn with_hook(mut self, hook: impl Fn(Stage, &str) + Send + Sync + 'static) -> Self {
        self.hook = Some(Arc::new(hook));
        self
    }

    /// The managed root.
    #[must_use]
    pub fn managed_root(&self) -> &Path {
        &self.root
    }

    /// The policy in force.
    #[must_use]
    pub const fn config(&self) -> &RetentionConfig {
        &self.config
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn at(&self, stage: Stage, id: &str) {
        if let Some(hook) = &self.hook {
            hook(stage, id);
        }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.state.join("worktrees").join(format!("{id}.json"))
    }

    fn prepared_path(&self, token: &str) -> PathBuf {
        self.state.join("prepared").join(format!("{token}.json"))
    }

    fn ref_of(&self, id: &str, name: &str) -> String {
        format!("{}/{id}/{name}", self.config.ref_namespace)
    }

    // ------------------------------------------------------------- records

    fn load(&self, id: &str) -> Result<Option<WorktreeRecord>, RetentionError> {
        let path = self.record_path(id);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let record: WorktreeRecord =
                    serde_json::from_slice(&bytes).map_err(|error| RetentionError::Corrupt {
                        path: path.display().to_string(),
                        detail: error.to_string(),
                    })?;
                if record.id != id || !Path::new(&record.path).starts_with(&self.root) {
                    return Err(RetentionError::Corrupt {
                        path: path.display().to_string(),
                        detail: "the record does not belong to this managed root".to_string(),
                    });
                }
                Ok(Some(record))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(&path, &error)),
        }
    }

    /// Write a record atomically (temporary file, `fsync`, rename), bumping its revision.
    fn save(&self, record: &mut WorktreeRecord) -> Result<(), RetentionError> {
        record.revision += 1;
        self.write_json(&self.record_path(&record.id), record)
    }

    fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), RetentionError> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| RetentionError::Corrupt {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
        let temporary =
            self.state
                .join("tmp")
                .join(format!("{}-{}.json", std::process::id(), self.now()));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            if let Some(parent) = path.parent() {
                if let Ok(directory) = std::fs::File::open(parent) {
                    let _ = directory.sync_all();
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temporary);
            return Err(io_error(path, &error));
        }
        Ok(())
    }

    /// Every record, sorted by id.
    ///
    /// # Errors
    /// [`RetentionError::Io`] or [`RetentionError::Corrupt`].
    pub fn list(&self) -> Result<Vec<WorktreeRecord>, RetentionError> {
        let directory = self.state.join("worktrees");
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&directory).map_err(|error| io_error(&directory, &error))? {
            let entry = entry.map_err(|error| io_error(&directory, &error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".json") {
                if valid_worktree_id(id) {
                    ids.push(id.to_string());
                }
            }
        }
        ids.sort();
        let mut records = Vec::new();
        for id in ids {
            if let Some(record) = self.load(&id)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// One record.
    ///
    /// # Errors
    /// [`RetentionError::InvalidId`], [`RetentionError::NotFound`] and the read errors of [`list`](Self::list).
    pub fn get(&self, id: &str) -> Result<WorktreeRecord, RetentionError> {
        check_id(id)?;
        self.load(id)?
            .ok_or_else(|| RetentionError::NotFound { id: id.to_string() })
    }

    /// The checkout of a worktree, or [`RetentionError::WorktreeMissing`] pointing at its recovery record
    /// when it was removed or is gone.
    ///
    /// # Errors
    /// [`RetentionError::WorktreeMissing`] (`WORKTREE_MISSING`) and the errors of [`get`](Self::get).
    pub fn require_checkout(&self, id: &str) -> Result<PathBuf, RetentionError> {
        let record = self.get(id)?;
        let path = PathBuf::from(&record.path);
        if record.state == WorktreeState::Removed || !path.is_dir() {
            return Err(RetentionError::WorktreeMissing {
                id: id.to_string(),
                state: record.state.as_str().to_string(),
                recovery_record: self.record_path(id).display().to_string(),
            });
        }
        Ok(path)
    }

    // ------------------------------------------------------------ lifecycle

    /// Create a managed worktree of `repo` at `rev` (default `HEAD`), detached, under the managed root.
    ///
    /// # Errors
    /// [`RetentionError::IdInUse`], [`RetentionError::Git`] when `repo` is not a Git working directory
    /// or the revision does not exist, and the file errors above.
    pub fn create(
        &self,
        repo: &Path,
        id: &str,
        rev: Option<&str>,
    ) -> Result<WorktreeRecord, RetentionError> {
        check_id(id)?;
        let _guard = self.guard();
        if self.load(id)?.is_some() {
            return Err(RetentionError::IdInUse { id: id.to_string() });
        }
        let top = self.git(repo, &["rev-parse", "--show-toplevel"], &[])?;
        let top = std::fs::canonicalize(top.trim()).map_err(|error| io_error(repo, &error))?;
        let repo_digest = Digest::of(top.to_string_lossy().as_bytes());
        let parent = self.root.join(&repo_digest.as_str()[..12]);
        std::fs::create_dir_all(&parent).map_err(|error| io_error(&parent, &error))?;
        let path = parent.join(id);
        if path.exists() {
            return Err(RetentionError::PathOccupied {
                path: path.display().to_string(),
            });
        }
        let path_text = path.to_string_lossy().into_owned();
        let rev = rev.unwrap_or("HEAD");
        if rev.starts_with('-') || rev.contains('\0') {
            return Err(RetentionError::Git {
                op: "worktree add".to_string(),
                detail: format!("{rev:?} is not a revision"),
            });
        }
        self.git(&top, &["worktree", "add", "--detach", &path_text, rev], &[])?;
        let head = self.git(&path, &["rev-parse", "--verify", "HEAD^{commit}"], &[])?;
        let now = self.now();
        let mut record = WorktreeRecord {
            id: id.to_string(),
            repo: top.to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            branch: None,
            head: head.trim().to_string(),
            revision: 0,
            activity: Activity {
                holders: 0,
                last_activity_at_ms: now,
            },
            state: WorktreeState::Idle,
            created_at_ms: now,
            recovery: None,
            removal: None,
        };
        self.save(&mut record)?;
        Ok(record)
    }

    /// A Run takes hold of a worktree: it is `ACTIVE` until every holder releases it.
    ///
    /// # Errors
    /// [`RetentionError::WorktreeMissing`] when the checkout is gone, and the errors of [`get`](Self::get).
    pub fn acquire(&self, id: &str) -> Result<WorktreeRecord, RetentionError> {
        let _guard = self.guard();
        self.require_checkout(id)?;
        let mut record = self.get(id)?;
        if record.state == WorktreeState::Removing {
            return Err(RetentionError::WorktreeMissing {
                id: id.to_string(),
                state: record.state.as_str().to_string(),
                recovery_record: self.record_path(id).display().to_string(),
            });
        }
        record.activity.holders += 1;
        record.activity.last_activity_at_ms = self.now();
        record.state = WorktreeState::Active;
        self.save(&mut record)?;
        Ok(record)
    }

    /// A Run lets go of a worktree.
    ///
    /// # Errors
    /// The errors of [`get`](Self::get) and the file errors above.
    pub fn release(&self, id: &str) -> Result<WorktreeRecord, RetentionError> {
        let _guard = self.guard();
        let mut record = self.get(id)?;
        record.activity.holders = record.activity.holders.saturating_sub(1);
        record.activity.last_activity_at_ms = self.now();
        if record.activity.holders == 0 && record.state == WorktreeState::Active {
            record.state = WorktreeState::Idle;
        }
        self.save(&mut record)?;
        Ok(record)
    }

    /// Re-read `HEAD` from the checkout and record a change (a commit, a checkout) as a new revision.
    ///
    /// # Errors
    /// [`RetentionError::WorktreeMissing`], [`RetentionError::Git`] and the errors above.
    pub fn observe(&self, id: &str) -> Result<WorktreeRecord, RetentionError> {
        let _guard = self.guard();
        let path = self.require_checkout(id)?;
        let mut record = self.get(id)?;
        let head = self.head_of(&path)?;
        if head != record.head {
            record.head = head;
            self.save(&mut record)?;
        }
        Ok(record)
    }

    // ------------------------------------------------------------- phase one

    /// Preview a set: record what each worktree looks like and answer the retention token that binds a
    /// later [`cleanup`](Self::cleanup) to exactly this set and these states.
    ///
    /// # Errors
    /// [`RetentionError::InvalidId`] for a malformed or repeated-empty request (more than [`MAX_IDS`]
    /// ids, or none), and the Git and file errors above.
    pub fn prepare(&self, ids: &[String]) -> Result<PreparedSet, RetentionError> {
        let ids = normalize_ids(ids)?;
        let _guard = self.guard();
        let mut entries = Vec::new();
        for id in &ids {
            entries.push(self.observe_entry(id)?);
        }
        let token = token_of(&entries);
        let prepared_at_ms = self.now();
        let set = PreparedSet {
            token,
            prepared_at_ms,
            expires_at_ms: prepared_at_ms + self.config.prepare_ttl_ms,
            entries,
        };
        self.write_json(&self.prepared_path(&set.token), &set)?;
        self.at(Stage::PrepareWritten, "");
        Ok(set)
    }

    fn observe_entry(&self, id: &str) -> Result<PreparedEntry, RetentionError> {
        let Some(record) = self.load(id)? else {
            return Ok(PreparedEntry {
                id: id.to_string(),
                exists: false,
                state: "NONE".to_string(),
                head: String::new(),
                revision: 0,
                holders: 0,
                last_activity_at_ms: 0,
                fingerprint: String::new(),
                dirty: false,
                unmerged: false,
            });
        };
        let path = PathBuf::from(&record.path);
        let live = if record.state != WorktreeState::Removed && path.is_dir() {
            Some(self.observe_checkout(&path)?)
        } else {
            None
        };
        Ok(PreparedEntry {
            id: id.to_string(),
            exists: true,
            state: record.state.as_str().to_string(),
            head: live
                .as_ref()
                .map_or_else(String::new, |live| live.head.clone()),
            revision: record.revision,
            holders: record.activity.holders,
            last_activity_at_ms: record.activity.last_activity_at_ms,
            fingerprint: live
                .as_ref()
                .map_or_else(String::new, |live| live.fingerprint.clone()),
            dirty: live.as_ref().is_some_and(|live| live.dirty),
            unmerged: live.as_ref().is_some_and(|live| live.unmerged),
        })
    }

    fn observe_checkout(&self, path: &Path) -> Result<LiveState, RetentionError> {
        let head = self.head_of(path)?;
        let staged = self.git_raw(path, &["ls-files", "-s", "-z"], &[])?;
        let unmerged = self.git_raw(path, &["ls-files", "-u", "-z"], &[])?;
        let status = self.git_raw(
            path,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
                "--no-renames",
            ],
            &[],
        )?;
        let mut material = Vec::new();
        material.extend_from_slice(head.as_bytes());
        material.push(0);
        material.extend_from_slice(&staged);
        material.push(0);
        material.extend_from_slice(&status);
        // `status` names a changed or untracked file but not what is in it, and a file edited again
        // after a preview has the same status line: the content of each is part of the fingerprint.
        material.push(0);
        for relative in changed_paths(&status) {
            material.extend_from_slice(relative.as_bytes());
            material.push(0);
            material.extend_from_slice(content_marker(&path.join(&relative)).as_bytes());
            material.push(0);
        }
        Ok(LiveState {
            head,
            fingerprint: Digest::of(&material).as_str().to_string(),
            dirty: !status.is_empty(),
            unmerged: !unmerged.is_empty(),
        })
    }

    // ------------------------------------------------------------- phase two

    /// Remove the prepared set, accounting for every id.
    ///
    /// The call is refused whole, with [`RetentionError::TokenInvalid`], when the token is not one this
    /// host prepared, has expired, or `ids` is not exactly the prepared set. Past that, every id ends as
    /// `Removed` or `Skipped` with a reason, rechecked against the preview immediately before removal.
    /// A skipped worktree is left exactly as it was. The prepared set is deleted only once every id is
    /// accounted for, so repeating a crashed cleanup with the same token resumes it.
    ///
    /// # Errors
    /// [`RetentionError::TokenInvalid`] and the read errors above; a failure while handling one worktree
    /// is that worktree's `Skipped` outcome, not an error of the call.
    pub fn cleanup(
        &self,
        token: &str,
        ids: &[String],
        mode: RemovalMode,
    ) -> Result<CleanupReport, RetentionError> {
        let requested = normalize_ids(ids)?;
        let _guard = self.guard();
        // A cleanup that crashed leaves `REMOVING`; resolve it first so it is accounted for truthfully.
        self.recover_locked()?;
        let set = self.load_prepared(token)?;
        if set.ids() != requested {
            return Err(RetentionError::TokenInvalid {
                reason: "set_mismatch",
            });
        }
        let mut outcomes = Vec::new();
        for entry in &set.entries {
            let outcome = self.remove_one(entry, mode)?;
            outcomes.push((entry.id.clone(), outcome));
        }
        let report = CleanupReport {
            token: set.token.clone(),
            mode,
            outcomes,
        };
        // Every requested id has exactly one outcome: a cleanup that lost one would be a silent skip.
        assert!(
            report.accounts_for(&requested),
            "a cleanup must account for every requested worktree"
        );
        let _ = std::fs::remove_file(self.prepared_path(&set.token));
        Ok(report)
    }

    fn load_prepared(&self, token: &str) -> Result<PreparedSet, RetentionError> {
        if token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(RetentionError::TokenInvalid { reason: "unknown" });
        }
        let path = self.prepared_path(token);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(RetentionError::TokenInvalid { reason: "unknown" })
            }
            Err(error) => return Err(io_error(&path, &error)),
        };
        let set: PreparedSet =
            serde_json::from_slice(&bytes).map_err(|error| RetentionError::Corrupt {
                path: path.display().to_string(),
                detail: error.to_string(),
            })?;
        // The token must be what its contents hash to: a prepared file edited by hand is not a preview.
        if set.token != token || token_of(&set.entries) != token {
            return Err(RetentionError::TokenInvalid { reason: "unknown" });
        }
        if self.now() > set.expires_at_ms {
            return Err(RetentionError::TokenInvalid { reason: "expired" });
        }
        Ok(set)
    }

    fn skip(reason: SkipReason, detail: impl Into<String>) -> Outcome {
        Outcome::Skipped {
            reason,
            detail: detail.into(),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn remove_one(
        &self,
        entry: &PreparedEntry,
        mode: RemovalMode,
    ) -> Result<Outcome, RetentionError> {
        let id = entry.id.as_str();
        let Some(record) = self.load(id)? else {
            return Ok(Self::skip(
                SkipReason::NotFound,
                "no managed worktree has this id",
            ));
        };
        if !entry.exists {
            // The preview saw no record but one exists now: that is a change since the preview.
            return Ok(Self::skip(
                SkipReason::RevisionChanged,
                "a record appeared after the preview",
            ));
        }
        match record.state {
            WorktreeState::Removed => {
                return Ok(Self::skip(
                    SkipReason::AlreadyRemoved,
                    "the checkout was removed already",
                ))
            }
            WorktreeState::Removing => {
                return Ok(Self::skip(SkipReason::Busy, "another cleanup holds it"))
            }
            WorktreeState::Active => return Ok(Self::skip(SkipReason::Active, "a Run holds it")),
            WorktreeState::Idle => {}
        }
        if record.activity.holders > 0 {
            return Ok(Self::skip(SkipReason::Active, "a Run holds it"));
        }
        if record.revision != entry.revision
            || record.activity.last_activity_at_ms != entry.last_activity_at_ms
        {
            return Ok(Self::skip(
                SkipReason::RevisionChanged,
                "its record changed after the preview",
            ));
        }
        let path = PathBuf::from(&record.path);
        if !path.is_dir() {
            return Ok(Self::skip(
                SkipReason::NotFound,
                "the checkout is not on disk",
            ));
        }
        let live = self.observe_checkout(&path)?;
        if live.head != entry.head || live.head != record.head {
            return Ok(Self::skip(
                SkipReason::HeadMoved,
                "HEAD moved after the preview",
            ));
        }
        if live.fingerprint != entry.fingerprint {
            return Ok(Self::skip(
                SkipReason::IndexChanged,
                "the index or the working tree changed after the preview",
            ));
        }
        if live.unmerged {
            return Ok(Self::skip(
                SkipReason::UnmergedIndex,
                "the index has unmerged entries; resolve them first",
            ));
        }
        if mode == RemovalMode::Auto && live.dirty {
            return Ok(Self::skip(
                SkipReason::NotClean,
                "the sweep removes only clean worktrees",
            ));
        }

        // The recovery snapshot (manual) or the head ref (automatic) comes first; removal never
        // starts before it is verified.
        let mut record = record;
        let recovery = match self.take_recovery(&path, mode, id) {
            Ok(recovery) => recovery,
            Err(error) => return Ok(Self::skip(SkipReason::RecoveryFailed, error.to_string())),
        };
        self.at(Stage::RefsWritten, id);
        record.recovery = Some(recovery);
        self.save(&mut record)?;
        let saved_revision = record.revision;
        self.at(Stage::RecoveryRecorded, id);

        // The last look before removal: the revision just written is ours, so compare what the
        // preview promised that we did not write ourselves.
        let Some(again) = self.load(id)? else {
            return Ok(Self::skip(SkipReason::NotFound, "the record vanished"));
        };
        if again.state != WorktreeState::Idle || again.activity.holders > 0 {
            return Ok(Self::skip(
                SkipReason::Active,
                "a Run took the worktree during cleanup",
            ));
        }
        if again.revision != saved_revision {
            return Ok(Self::skip(
                SkipReason::RevisionChanged,
                "its record changed during cleanup",
            ));
        }
        let last = self.observe_checkout(&path)?;
        if last.head != entry.head {
            return Ok(Self::skip(
                SkipReason::HeadMoved,
                "HEAD moved during cleanup",
            ));
        }
        if last.fingerprint != entry.fingerprint {
            return Ok(Self::skip(
                SkipReason::IndexChanged,
                "the index or the working tree changed during cleanup",
            ));
        }

        let mut record = again;
        record.state = WorktreeState::Removing;
        record.removal = Some(Removal {
            mode,
            started_at_ms: self.now(),
            fingerprint: last.fingerprint.clone(),
            finished_at_ms: None,
        });
        self.save(&mut record)?;
        self.at(Stage::RemovingRecorded, id);

        if let Err(error) = self.remove_checkout(&record) {
            // Whatever is left is either intact (return it to IDLE) or damaged (recover finishes it).
            if self.intact(&path) {
                record.state = WorktreeState::Idle;
                record.removal = None;
                self.save(&mut record)?;
            }
            return Ok(Self::skip(SkipReason::RemovalFailed, error.to_string()));
        }
        self.at(Stage::CheckoutRemoved, id);
        self.finish_removal(&mut record)?;
        Ok(Outcome::Removed)
    }

    fn finish_removal(&self, record: &mut WorktreeRecord) -> Result<(), RetentionError> {
        record.state = WorktreeState::Removed;
        if let Some(removal) = record.removal.as_mut() {
            removal.finished_at_ms = Some(self.now());
        }
        self.save(record)
    }

    fn remove_checkout(&self, record: &WorktreeRecord) -> Result<(), RetentionError> {
        let repo = Path::new(&record.repo);
        let path = Path::new(&record.path);
        if path.exists() {
            let removed = self.git(repo, &["worktree", "remove", "--force", &record.path], &[]);
            if let Err(error) = removed {
                // A checkout Git no longer recognizes (its `.git` file is gone) is removed directly:
                // it is inside the managed root and its state is in the recovery refs.
                if path.exists() && !self.intact(path) {
                    std::fs::remove_dir_all(path).map_err(|e| io_error(path, &e))?;
                } else {
                    return Err(error);
                }
            }
        }
        // Forget the administrative entry of a checkout that is gone.
        let _ = self.git(repo, &["worktree", "prune"], &[]);
        if path.exists() {
            return Err(RetentionError::PathOccupied {
                path: record.path.clone(),
            });
        }
        Ok(())
    }

    /// Whether the checkout is a working Git worktree.
    fn intact(&self, path: &Path) -> bool {
        path.join(".git").exists() && self.git(path, &["rev-parse", "--git-dir"], &[]).is_ok()
    }

    // ------------------------------------------------------------- recovery

    /// The recovery refs for the worktree at `path` under `mode`, each verified to name the commit that
    /// was written before `complete` is set.
    fn take_recovery(
        &self,
        path: &Path,
        mode: RemovalMode,
        id: &str,
    ) -> Result<Recovery, RetentionError> {
        let head = self.head_of(path)?;
        let head_ref = self.ref_of(id, "head");
        self.git(path, &["update-ref", &head_ref, &head], &[])?;
        let mut written = vec![(head_ref.clone(), head.clone())];
        let mut recovery = Recovery {
            head_ref,
            index_ref: None,
            snapshot_ref: None,
            taken_at_ms: self.now(),
            complete: false,
            ignored_files: 0,
        };
        if mode == RemovalMode::Manual {
            let identity = [
                ("GIT_AUTHOR_NAME", "Quansio"),
                ("GIT_AUTHOR_EMAIL", "recovery@quansio.invalid"),
                ("GIT_COMMITTER_NAME", "Quansio"),
                ("GIT_COMMITTER_EMAIL", "recovery@quansio.invalid"),
            ];
            // The index, as it is. `write-tree` on the live index would refresh its cache-tree extension
            // and rewrite the file, so the staged entries are read (`ls-files -s` writes nothing), put
            // into a temporary index file and written as a tree from there; the real index is never written.
            let staged = self.git_raw(path, &["ls-files", "-s", "-z"], &[])?;
            let staged_scratch = self.state.join("tmp").join(format!("{id}.staged.index"));
            let _ = std::fs::remove_file(&staged_scratch);
            let staged_text = staged_scratch.to_string_lossy().into_owned();
            let staged_env = [("GIT_INDEX_FILE", staged_text.as_str())];
            let index_tree = (|| -> Result<String, RetentionError> {
                self.git_with_input(
                    path,
                    &["update-index", "-z", "--index-info"],
                    &staged_env,
                    &staged,
                )?;
                self.git(path, &["write-tree"], &staged_env)
            })();
            let _ = std::fs::remove_file(&staged_scratch);
            let index_tree = index_tree?;
            let index_commit = self.git(
                path,
                &[
                    "commit-tree",
                    &index_tree,
                    "-p",
                    &head,
                    "-m",
                    &format!("quansio recovery: index of {id}"),
                ],
                &identity,
            )?;
            // The working tree, built in a temporary index file outside the repository and the
            // worktree, so the real index is never written.
            let scratch = self.state.join("tmp").join(format!("{id}.index"));
            let _ = std::fs::remove_file(&scratch);
            let scratch_text = scratch.to_string_lossy().into_owned();
            let built = (|| -> Result<String, RetentionError> {
                let env = [("GIT_INDEX_FILE", scratch_text.as_str())];
                self.git(path, &["read-tree", &head], &env)?;
                self.git(path, &["add", "-A", "--", "."], &env)?;
                self.git(path, &["write-tree"], &env)
            })();
            let _ = std::fs::remove_file(&scratch);
            let snapshot_tree = built?;
            let snapshot_commit = self.git(
                path,
                &[
                    "commit-tree",
                    &snapshot_tree,
                    "-p",
                    &head,
                    "-p",
                    &index_commit,
                    "-m",
                    &format!("quansio recovery: working tree of {id}"),
                ],
                &identity,
            )?;
            let index_ref = self.ref_of(id, "index");
            let snapshot_ref = self.ref_of(id, "snapshot");
            self.git(path, &["update-ref", &index_ref, &index_commit], &[])?;
            self.git(path, &["update-ref", &snapshot_ref, &snapshot_commit], &[])?;
            written.push((index_ref.clone(), index_commit));
            written.push((snapshot_ref.clone(), snapshot_commit.clone()));
            recovery.index_ref = Some(index_ref);
            recovery.snapshot_ref = Some(snapshot_ref);
            let ignored = self.git_raw(
                path,
                &[
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--directory",
                    "-z",
                ],
                &[],
            )?;
            recovery.ignored_files = ignored
                .split(|byte| *byte == 0)
                .filter(|part| !part.is_empty())
                .count() as u64;
            // The snapshot's parents are HEAD, then the index snapshot.
            let parents = self.git(
                path,
                &["rev-list", "--parents", "-n", "1", &snapshot_commit],
                &[],
            )?;
            if parents.split_whitespace().count() != 3 {
                return Err(RetentionError::Git {
                    op: "verify".to_string(),
                    detail: "the snapshot commit does not have two parents".to_string(),
                });
            }
        }
        // Verify: every ref names the commit written, so "complete" is a fact and not a hope.
        for (name, commit) in &written {
            let named = self.git(
                path,
                &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
                &[],
            )?;
            if &named != commit {
                return Err(RetentionError::Git {
                    op: "verify".to_string(),
                    detail: format!("{name} does not name the commit that was written"),
                });
            }
        }
        recovery.complete = true;
        Ok(recovery)
    }

    /// Resolve every record a crash left in `REMOVING` (DOMAIN.md §8.7 E). Nothing else changes.
    ///
    /// # Errors
    /// The read errors of [`list`](Self::list) and the file errors above.
    pub fn recover(&self) -> Result<RecoveryReport, RetentionError> {
        let _guard = self.guard();
        self.recover_locked()
    }

    fn recover_locked(&self) -> Result<RecoveryReport, RetentionError> {
        let mut report = RecoveryReport::default();
        for mut record in self.list()? {
            if record.state != WorktreeState::Removing {
                continue;
            }
            let path = PathBuf::from(&record.path);
            let complete = record
                .recovery
                .as_ref()
                .is_some_and(|recovery| recovery.complete);
            let id = record.id.clone();
            if !complete {
                // Removal never starts before the recovery is complete, so the checkout is untouched.
                if path.is_dir() {
                    record.state = WorktreeState::Idle;
                    record.removal = None;
                    self.save(&mut record)?;
                    report.rolled_back.push(id);
                }
                continue;
            }
            if !path.exists() {
                let _ = self.git(Path::new(&record.repo), &["worktree", "prune"], &[]);
                self.finish_removal(&mut record)?;
                report.completed.push(id);
                continue;
            }
            if self.intact(&path) {
                let unchanged = match (&record.removal, self.observe_checkout(&path)) {
                    (Some(removal), Ok(live)) => live.fingerprint == removal.fingerprint,
                    _ => false,
                };
                if !unchanged {
                    record.state = WorktreeState::Idle;
                    record.removal = None;
                    self.save(&mut record)?;
                    report.rolled_back.push(id);
                    continue;
                }
            }
            // Unchanged and intact, or already damaged: the state is in the recovery refs.
            match self.remove_checkout(&record) {
                Ok(()) => {
                    self.finish_removal(&mut record)?;
                    report.completed.push(id);
                }
                Err(_) => {
                    // Left in REMOVING: the next recover (or cleanup) tries again.
                }
            }
        }
        Ok(report)
    }

    /// Recreate a removed worktree at its original path from its recovery refs (DOMAIN.md §8.7 F).
    ///
    /// # Errors
    /// [`RetentionError::NotRestorable`] unless the worktree is `REMOVED` with a complete recovery,
    /// [`RetentionError::PathOccupied`] when something is at its path, and the Git errors above.
    pub fn restore(&self, id: &str) -> Result<WorktreeRecord, RetentionError> {
        let _guard = self.guard();
        let mut record = self.get(id)?;
        let not_restorable = |record: &WorktreeRecord| RetentionError::NotRestorable {
            id: record.id.clone(),
            state: record.state.as_str().to_string(),
        };
        let Some(recovery) = record.recovery.clone().filter(|recovery| recovery.complete) else {
            return Err(not_restorable(&record));
        };
        if record.state != WorktreeState::Removed {
            return Err(not_restorable(&record));
        }
        let path = PathBuf::from(&record.path);
        if path.exists() {
            return Err(RetentionError::PathOccupied {
                path: record.path.clone(),
            });
        }
        let repo = PathBuf::from(&record.repo);
        let _ = self.git(&repo, &["worktree", "prune"], &[]);
        self.git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                &record.path,
                &recovery.head_ref,
            ],
            &[],
        )?;
        if let (Some(snapshot), Some(index)) = (&recovery.snapshot_ref, &recovery.index_ref) {
            // The working tree becomes the snapshot (deleting what the snapshot lacks), then the index
            // becomes what was staged. Without `-u` the second step leaves the files alone.
            self.git(
                &path,
                &[
                    "read-tree",
                    "--reset",
                    "-u",
                    &format!("{snapshot}^{{tree}}"),
                ],
                &[],
            )?;
            self.git(&path, &["read-tree", &format!("{index}^{{tree}}")], &[])?;
        }
        record.head = self.head_of(&path)?;
        record.state = WorktreeState::Idle;
        record.activity.holders = 0;
        record.activity.last_activity_at_ms = self.now();
        record.removal = None;
        self.save(&mut record)?;
        Ok(record)
    }

    // ------------------------------------------------------------- the sweep

    /// The automatic sweep: while more than `max_resident` worktrees are resident, remove the oldest
    /// idle, fully clean ones (idle for at least `sweep.min_idle_ms`, at most `sweep.max_per_run`).
    /// It prepares and cleans up in one call, through the same rechecks as a manual cleanup.
    ///
    /// # Errors
    /// The errors of [`prepare`](Self::prepare) and [`cleanup`](Self::cleanup).
    pub fn sweep(&self) -> Result<Option<CleanupReport>, RetentionError> {
        let candidates = {
            let _guard = self.guard();
            let records = self.list()?;
            let resident: Vec<&WorktreeRecord> = records
                .iter()
                .filter(|record| {
                    matches!(record.state, WorktreeState::Idle | WorktreeState::Active)
                })
                .collect();
            let excess = resident.len().saturating_sub(self.config.max_resident);
            if excess == 0 {
                return Ok(None);
            }
            let now = self.now();
            let mut idle: Vec<&WorktreeRecord> = resident
                .into_iter()
                .filter(|record| {
                    record.state == WorktreeState::Idle
                        && record.activity.holders == 0
                        && now.saturating_sub(record.activity.last_activity_at_ms)
                            >= self.config.sweep_min_idle_ms
                })
                .collect();
            idle.sort_by(|a, b| {
                a.activity
                    .last_activity_at_ms
                    .cmp(&b.activity.last_activity_at_ms)
                    .then_with(|| a.id.cmp(&b.id))
            });
            let mut chosen = Vec::new();
            for record in idle {
                if chosen.len() >= excess.min(self.config.sweep_max_per_run) {
                    break;
                }
                // Only a fully clean checkout is a candidate; anything else is left alone.
                let path = PathBuf::from(&record.path);
                if path.is_dir() {
                    let live = self.observe_checkout(&path)?;
                    if !live.dirty && !live.unmerged {
                        chosen.push(record.id.clone());
                    }
                }
            }
            chosen
        };
        if candidates.is_empty() {
            return Ok(None);
        }
        let set = self.prepare(&candidates)?;
        self.cleanup(&set.token, &candidates, RemovalMode::Auto)
            .map(Some)
    }

    // ----------------------------------------------------------------- git

    fn head_of(&self, path: &Path) -> Result<String, RetentionError> {
        Ok(self
            .git(path, &["rev-parse", "--verify", "HEAD^{commit}"], &[])?
            .trim()
            .to_string())
    }

    fn git(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<String, RetentionError> {
        let raw = self.git_raw(cwd, args, env)?;
        Ok(String::from_utf8_lossy(&raw)
            .trim_end_matches('\n')
            .to_string())
    }

    fn git_raw(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Vec<u8>, RetentionError> {
        self.git_run(cwd, args, env, None)
    }

    fn git_with_input(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        input: &[u8],
    ) -> Result<Vec<u8>, RetentionError> {
        self.git_run(cwd, args, env, Some(input))
    }

    #[allow(clippy::unused_self)]
    fn git_run(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>, RetentionError> {
        use std::process::Stdio;
        let failed = |detail: String| RetentionError::Git {
            op: args.first().copied().unwrap_or("").to_string(),
            detail,
        };
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(cwd)
            .args([
                "-c",
                "gc.auto=0",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        for (key, value) in env {
            command.env(key, value);
        }
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| failed(error.to_string()))?;
        if let Some(input) = input {
            // Written from another thread so a command that writes a lot before it reads cannot deadlock.
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| failed("no stdin".to_string()))?;
            let bytes = input.to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            });
        }
        let output = child
            .wait_with_output()
            .map_err(|error| failed(error.to_string()))?;
        if !output.status.success() {
            return Err(failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(output.stdout)
    }
}

struct LiveState {
    head: String,
    fingerprint: String,
    dirty: bool,
    unmerged: bool,
}

/// The paths a `git status --porcelain=v2 -z` output names (ordinary, unmerged and untracked entries).
fn changed_paths(status: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    for entry in status.split(|byte| *byte == 0) {
        let text = String::from_utf8_lossy(entry);
        let path = match text.chars().next() {
            Some('1') => text.splitn(9, ' ').nth(8),
            Some('u') => text.splitn(11, ' ').nth(10),
            Some('?') => text.strip_prefix("? "),
            _ => None,
        };
        if let Some(path) = path {
            paths.push(path.to_string());
        }
    }
    paths.sort();
    paths
}

/// What a changed path holds right now: a digest of a regular file's bytes, the target of a link,
/// a directory marker, or `absent` (a deleted file).
fn content_marker(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::read_link(path).map_or_else(
            |_| "link".to_string(),
            |target| format!("link:{}", target.display()),
        ),
        Ok(metadata) if metadata.is_file() => match std::fs::read(path) {
            Ok(bytes) => {
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt as _;
                    metadata.permissions().mode() & 0o111
                };
                #[cfg(not(unix))]
                let mode = 0;
                format!("file:{}:{mode}", Digest::of(&bytes))
            }
            Err(error) => format!("unreadable:{error}"),
        },
        Ok(_) => "directory".to_string(),
        Err(_) => "absent".to_string(),
    }
}

fn check_id(id: &str) -> Result<(), RetentionError> {
    if valid_worktree_id(id) {
        Ok(())
    } else {
        Err(RetentionError::InvalidId(id.to_string()))
    }
}

/// The ids, validated, de-duplicated and sorted; at least one and at most [`MAX_IDS`].
fn normalize_ids(ids: &[String]) -> Result<Vec<String>, RetentionError> {
    if ids.is_empty() || ids.len() > MAX_IDS {
        return Err(RetentionError::InvalidId(format!(
            "{} worktree ids",
            ids.len()
        )));
    }
    let mut out = BTreeSet::new();
    for id in ids {
        check_id(id)?;
        out.insert(id.clone());
    }
    Ok(out.into_iter().collect())
}

/// The token of a prepared set: SHA-256 of the canonical JSON of the sorted entries under a domain
/// separator, so it binds the exact set and each worktree's state.
fn token_of(entries: &[PreparedEntry]) -> String {
    let json = serde_json::to_string(entries).unwrap_or_default();
    Digest::of(format!("{TOKEN_DOMAIN}\0{json}").as_bytes())
        .as_str()
        .to_string()
}
