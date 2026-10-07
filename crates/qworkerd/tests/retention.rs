//! Worktree retention (ADP-005, DOMAIN.md §8.7): two-phase cleanup, the recovery snapshot, the automatic
//! sweep and recovery after a crash.
//!
//! Everything here runs against real Git: real repositories and linked worktrees on a real filesystem, the
//! `git` binary on the PATH, and — for the crash tests — a real child process that is killed with SIGKILL
//! at an exact point of a cleanup, after which a fresh host over the same directories is asked what state
//! it finds. Nothing is stood in for except the clock the sweep's idle rule reads.
//!
//! The crash tests re-run this test binary as the child (`crash_child`, which does nothing unless the
//! `QUANSIO_RETENTION_CHILD_*` variables are set), so no separate executable is needed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quansio_qworkerd::tools::scm::retention::{
    valid_worktree_id, Outcome, RemovalMode, RetentionConfig, RetentionError, SkipReason, Stage,
    WorktreeRetention, WorktreeState,
};
use quansio_qworkerd::tools::{RootScope, Sandbox};

const HOUR: u64 = 3_600_000;

/// A change a test makes from inside a cleanup.
type Action = Box<dyn Fn() + Send>;

/// What a hook saw of a file at each stage.
type Seen = Vec<(Stage, Vec<u8>)>;

/// Run `git` in `dir`; panics with Git's own message on failure. Returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "Person")
        .env("GIT_AUTHOR_EMAIL", "person@example.test")
        .env("GIT_COMMITTER_NAME", "Person")
        .env("GIT_COMMITTER_EMAIL", "person@example.test")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, text).expect("write");
}

/// A scratch workspace and the person's own checkout, mid-work: a feature branch, a stash and a staged
/// change in its real index, all of which retention must leave exactly as they are.
struct Lab {
    _holder: tempfile::TempDir,
    workspace: PathBuf,
    repo: PathBuf,
    clock: Arc<AtomicU64>,
}

/// What the person's checkout holds that no retention call may change.
#[derive(Debug, PartialEq, Eq)]
struct UserState {
    refs: String,
    stash: String,
    head: String,
    index: Vec<u8>,
    status: String,
}

impl Lab {
    fn new() -> Self {
        let holder = tempfile::tempdir().expect("temp dir");
        let workspace = holder.path().join("workspace");
        let repo = holder.path().join("person").join("repo");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&repo).expect("repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        write(&repo.join(".gitignore"), "*.log\ntarget/\n");
        write(&repo.join("README.md"), "readme v1\n");
        write(&repo.join("src/lib.txt"), "library v1\n");
        write(&repo.join("notes/a.txt"), "note a\n");
        write(&repo.join("scripts/run.sh"), "#!/bin/sh\necho run\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        git(&repo, &["branch", "feature/mine"]);
        git(&repo, &["tag", "v0"]);
        // A stash and a staged change: the person's own work in progress.
        write(&repo.join("README.md"), "readme stashed\n");
        git(&repo, &["stash", "push", "-q", "-m", "person's stash"]);
        write(&repo.join("notes/a.txt"), "note a, staged by the person\n");
        git(&repo, &["add", "notes/a.txt"]);
        Self {
            _holder: holder,
            workspace,
            repo,
            clock: Arc::new(AtomicU64::new(1_000_000_000_000)),
        }
    }

    fn sandbox(&self) -> Sandbox {
        Sandbox::empty()
            .authorize(RootScope::Workspace, &self.workspace)
            .expect("authorize")
    }

    fn config(&self) -> RetentionConfig {
        RetentionConfig::shipped()
    }

    fn retention(&self) -> WorktreeRetention {
        self.retention_with(self.config())
    }

    fn retention_with(&self, config: RetentionConfig) -> WorktreeRetention {
        let clock = Arc::clone(&self.clock);
        WorktreeRetention::new(&self.sandbox(), config)
            .expect("retention host")
            .with_clock(move || clock.load(Ordering::SeqCst))
    }

    fn advance(&self, millis: u64) {
        self.clock.fetch_add(millis, Ordering::SeqCst);
    }

    fn user_state(&self) -> UserState {
        UserState {
            refs: git(
                &self.repo,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                    "refs/tags",
                    "refs/stash",
                ],
            ),
            stash: git(&self.repo, &["stash", "list"]),
            head: git(&self.repo, &["rev-parse", "HEAD"]),
            index: std::fs::read(self.repo.join(".git").join("index")).expect("the person's index"),
            status: git(
                &self.repo,
                &["status", "--porcelain=v2", "--untracked-files=all"],
            ),
        }
    }

    /// Every ref in the repository that is not one of the person's.
    fn other_refs(&self) -> Vec<String> {
        git(&self.repo, &["for-each-ref", "--format=%(refname)"])
            .lines()
            .filter(|name| {
                !name.starts_with("refs/heads/")
                    && !name.starts_with("refs/tags/")
                    && *name != "refs/stash"
            })
            .map(str::to_string)
            .collect()
    }
}

/// Change a worktree in every way Git tracks: staged, unstaged on top of staged, deleted, untracked and
/// ignored files, and an executable bit.
fn make_work(path: &Path) {
    write(&path.join("README.md"), "readme v2 (staged)\n");
    git(path, &["add", "README.md"]);
    write(&path.join("README.md"), "readme v3 (unstaged on top)\n");
    write(&path.join("src/lib.txt"), "library edited\n");
    std::fs::remove_file(path.join("notes/a.txt")).expect("delete a tracked file");
    write(&path.join("new_file.txt"), "untracked work\n");
    write(&path.join("deep/er/new.txt"), "untracked and nested\n");
    write(&path.join("debug.log"), "ignored, and not captured\n");
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_string()).collect()
}

fn status_of(path: &Path) -> String {
    git(path, &["status", "--porcelain=v2", "--untracked-files=all"])
}

fn tree_files(path: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("inside")
                    .to_string_lossy()
                    .into_owned();
                out.insert(relative, std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(path, path, &mut out);
    out
}

fn skipped(
    report: &quansio_qworkerd::tools::scm::retention::CleanupReport,
    id: &str,
) -> SkipReason {
    report
        .skipped()
        .into_iter()
        .find(|(skipped, _)| *skipped == id)
        .unwrap_or_else(|| panic!("{id} was not skipped: {report:?}"))
        .1
}

// ------------------------------------------------------------------------------ phase one

#[test]
fn a_retention_token_binds_the_exact_set_and_each_worktrees_state() {
    let lab = Lab::new();
    let retention = lab.retention();
    retention.create(&lab.repo, "alpha", None).expect("alpha");
    retention.create(&lab.repo, "beta", None).expect("beta");

    let both = retention
        .prepare(&ids(&["alpha", "beta"]))
        .expect("prepare");
    assert_eq!(both.token.len(), 64);
    assert!(
        both.token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{}",
        both.token
    );
    // The same set in the same state is the same preview, whatever order the ids arrive in.
    assert_eq!(
        retention
            .prepare(&ids(&["beta", "alpha", "alpha"]))
            .expect("again")
            .token,
        both.token
    );
    // Another set is another token.
    assert_ne!(
        retention.prepare(&ids(&["alpha"])).expect("one").token,
        both.token
    );

    // A change in either worktree's state is another token: edit a file, and a file that was already
    // changed gets different content (the status line is the same, the preview is not).
    let beta = PathBuf::from(&retention.get("beta").expect("record").path);
    write(&beta.join("new_file.txt"), "one\n");
    let dirty = retention.prepare(&ids(&["alpha", "beta"])).expect("dirty");
    assert_ne!(dirty.token, both.token);
    write(&beta.join("new_file.txt"), "two\n");
    let edited = retention.prepare(&ids(&["alpha", "beta"])).expect("edited");
    assert_ne!(
        edited.token, dirty.token,
        "a changed untracked file must change the preview"
    );
    assert!(
        edited
            .entries
            .iter()
            .find(|entry| entry.id == "beta")
            .expect("beta")
            .dirty
    );
    assert!(
        !edited
            .entries
            .iter()
            .find(|entry| entry.id == "alpha")
            .expect("alpha")
            .dirty
    );
}

#[test]
fn a_token_that_is_unknown_expired_malformed_or_for_another_set_refuses_the_whole_call() {
    let lab = Lab::new();
    let retention = lab.retention();
    retention.create(&lab.repo, "alpha", None).expect("alpha");
    retention.create(&lab.repo, "beta", None).expect("beta");
    let set = retention
        .prepare(&ids(&["alpha", "beta"]))
        .expect("prepare");

    let refusal = |error: RetentionError| match error {
        RetentionError::TokenInvalid { reason } => reason,
        other => panic!("not a token refusal: {other:?}"),
    };
    // Not a token this host prepared: unknown (a well-formed one, a malformed one, an edited one).
    let unknown = "0".repeat(64);
    assert_eq!(
        refusal(
            retention
                .cleanup(&unknown, &ids(&["alpha", "beta"]), RemovalMode::Manual)
                .unwrap_err()
        ),
        "unknown"
    );
    assert_eq!(
        refusal(
            retention
                .cleanup("short", &ids(&["alpha", "beta"]), RemovalMode::Manual)
                .unwrap_err()
        ),
        "unknown"
    );
    assert_eq!(
        refusal(
            retention
                .cleanup(
                    "../../etc/passwd",
                    &ids(&["alpha", "beta"]),
                    RemovalMode::Manual
                )
                .unwrap_err()
        ),
        "unknown"
    );
    // A subset, a superset and another set are all refused: the token binds the exact set.
    assert_eq!(
        refusal(
            retention
                .cleanup(&set.token, &ids(&["alpha"]), RemovalMode::Manual)
                .unwrap_err()
        ),
        "set_mismatch"
    );
    assert_eq!(
        refusal(
            retention
                .cleanup(
                    &set.token,
                    &ids(&["alpha", "beta", "gamma"]),
                    RemovalMode::Manual
                )
                .unwrap_err()
        ),
        "set_mismatch"
    );
    assert_eq!(
        refusal(
            retention
                .cleanup(&set.token, &ids(&["alpha", "gamma"]), RemovalMode::Manual)
                .unwrap_err()
        ),
        "set_mismatch"
    );
    // Nothing was removed by any of that, and the prepared set is still usable.
    for id in ["alpha", "beta"] {
        assert!(
            PathBuf::from(&retention.get(id).expect("record").path).is_dir(),
            "{id}"
        );
    }
    // A prepared file edited by hand is not a preview.
    let prepared = retention
        .managed_root()
        .join(".quansio/prepared")
        .join(format!("{}.json", set.token));
    let original = std::fs::read_to_string(&prepared).expect("prepared file");
    std::fs::write(&prepared, original.replace("\"beta\"", "\"gamma\"")).expect("edit");
    assert_eq!(
        refusal(
            retention
                .cleanup(&set.token, &ids(&["alpha", "gamma"]), RemovalMode::Manual)
                .unwrap_err()
        ),
        "unknown"
    );
    std::fs::write(&prepared, original).expect("restore the file");
    // Past its time to live, the token is expired.
    lab.advance(lab.config().prepare_ttl_ms + 1);
    assert_eq!(
        refusal(
            retention
                .cleanup(&set.token, &ids(&["alpha", "beta"]), RemovalMode::Manual)
                .unwrap_err()
        ),
        "expired"
    );
}

#[test]
fn a_worktree_id_is_a_slug() {
    for good in ["a", "wt-1", "run_01j8z3", "0abc"] {
        assert!(valid_worktree_id(good), "{good}");
    }
    for bad in [
        "",
        "-a",
        "_a",
        "A",
        "a b",
        "a/b",
        "../a",
        "a.b",
        &"a".repeat(64),
    ] {
        assert!(!valid_worktree_id(bad), "{bad:?}");
    }
    let lab = Lab::new();
    let retention = lab.retention();
    assert!(matches!(
        retention.create(&lab.repo, "../escape", None),
        Err(RetentionError::InvalidId(_))
    ));
    assert!(matches!(
        retention.prepare(&[]),
        Err(RetentionError::InvalidId(_))
    ));
    assert!(matches!(
        retention.prepare(&ids(&["ok", "Not OK"])),
        Err(RetentionError::InvalidId(_))
    ));
    // A revision that is an option is not a revision.
    assert!(retention
        .create(&lab.repo, "opt", Some("--output=/tmp/x"))
        .is_err());
    retention.create(&lab.repo, "once", None).expect("once");
    assert!(matches!(
        retention.create(&lab.repo, "once", None),
        Err(RetentionError::IdInUse { .. })
    ));
    // Not a repository.
    let plain = lab.workspace.join("plain");
    std::fs::create_dir_all(&plain).expect("plain");
    assert!(matches!(
        retention.create(&plain, "plain", None),
        Err(RetentionError::Git { .. })
    ));
}

#[test]
fn the_managed_directory_cannot_leave_the_workspace_root() {
    let lab = Lab::new();
    let outside = lab.workspace.parent().expect("parent").join("elsewhere");
    std::fs::create_dir_all(&outside).expect("outside");
    std::os::unix::fs::symlink(&outside, lab.workspace.join("agent-worktrees")).expect("link");
    let refused =
        WorktreeRetention::new(&lab.sandbox(), lab.config()).expect_err("must be refused");
    assert!(matches!(refused, RetentionError::Config(_)), "{refused:?}");
    assert!(
        std::fs::read_dir(&outside)
            .expect("outside")
            .next()
            .is_none(),
        "nothing was created outside the root"
    );
    // A sandbox with no workspace root has nowhere to manage worktrees.
    assert!(WorktreeRetention::new(&Sandbox::empty(), lab.config()).is_err());
}

// ------------------------------------------------------------------------------ phase two

#[test]
fn cleanup_accounts_for_every_requested_id_with_a_reason_for_each_skip() {
    let lab = Lab::new();
    let retention = lab.retention();
    for id in ["clean", "busy-run", "dirty-edit", "gone"] {
        retention.create(&lab.repo, id, None).expect(id);
    }
    // `gone` has no checkout; `ghost` was never created.
    let gone = PathBuf::from(&retention.get("gone").expect("gone").path);
    std::fs::remove_dir_all(&gone).expect("remove the checkout behind retention's back");
    let wanted = ids(&["clean", "busy-run", "dirty-edit", "gone", "ghost"]);
    let set = retention.prepare(&wanted).expect("prepare");
    // After the preview: a Run takes `busy-run`, and a file is edited in `dirty-edit`.
    retention.acquire("busy-run").expect("acquire");
    write(
        &PathBuf::from(&retention.get("dirty-edit").expect("dirty").path).join("src/lib.txt"),
        "edited after the preview\n",
    );

    let report = retention
        .cleanup(&set.token, &wanted, RemovalMode::Manual)
        .expect("cleanup");
    assert!(report.accounts_for(&wanted), "{report:?}");
    assert_eq!(report.outcomes.len(), 5);
    assert_eq!(report.removed(), vec!["clean"]);
    assert_eq!(skipped(&report, "busy-run"), SkipReason::Active);
    assert_eq!(skipped(&report, "dirty-edit"), SkipReason::IndexChanged);
    assert_eq!(skipped(&report, "gone"), SkipReason::NotFound);
    assert_eq!(skipped(&report, "ghost"), SkipReason::NotFound);
    for (id, outcome) in &report.outcomes {
        if let Outcome::Skipped { reason, detail } = outcome {
            assert!(
                !detail.is_empty(),
                "{id} was skipped ({}) without a reason a person can read",
                reason.as_str()
            );
        }
    }
    // A skipped worktree is exactly as it was; the removed one is gone and its record says so.
    assert!(PathBuf::from(&retention.get("busy-run").expect("busy").path).is_dir());
    assert_eq!(
        retention.get("busy-run").expect("busy").state,
        WorktreeState::Active
    );
    let edited = PathBuf::from(&retention.get("dirty-edit").expect("dirty").path);
    assert_eq!(
        std::fs::read_to_string(edited.join("src/lib.txt")).expect("read"),
        "edited after the preview\n"
    );
    assert_eq!(
        retention.get("clean").expect("clean").state,
        WorktreeState::Removed
    );
    assert!(!PathBuf::from(&retention.get("clean").expect("clean").path).exists());
    // The prepared set is spent once every id is accounted for.
    assert!(matches!(
        retention.cleanup(&set.token, &wanted, RemovalMode::Manual),
        Err(RetentionError::TokenInvalid { reason: "unknown" })
    ));
}

#[test]
fn every_part_of_a_worktrees_state_is_rechecked_against_the_preview() {
    let lab = Lab::new();
    let retention = lab.retention();
    for id in ["head", "index", "content", "held", "released"] {
        retention.create(&lab.repo, id, None).expect(id);
    }
    // `content` already has an untracked file, so its status line will not change when it is edited.
    let path = |id: &str| PathBuf::from(&retention.get(id).expect(id).path);
    write(&path("content").join("note.txt"), "first\n");
    let wanted = ids(&["head", "index", "content", "held", "released"]);
    let set = retention.prepare(&wanted).expect("prepare");

    // HEAD moves: a commit in the worktree.
    write(&path("head").join("committed.txt"), "c\n");
    git(&path("head"), &["add", "committed.txt"]);
    git(&path("head"), &["commit", "-q", "-m", "moved"]);
    // The live index changes: a staged file.
    write(&path("index").join("staged.txt"), "s\n");
    git(&path("index"), &["add", "staged.txt"]);
    // Content changes under an unchanged status line.
    write(&path("content").join("note.txt"), "second\n");
    // Activity: a Run takes one and still holds it.
    retention.acquire("held").expect("acquire");
    // The record moves on and settles: held and released again, so nothing is held now.
    retention.acquire("released").expect("acquire");
    retention.release("released").expect("release");

    let report = retention
        .cleanup(&set.token, &wanted, RemovalMode::Manual)
        .expect("cleanup");
    assert!(report.accounts_for(&wanted));
    assert!(report.removed().is_empty(), "{report:?}");
    assert_eq!(skipped(&report, "head"), SkipReason::HeadMoved);
    assert_eq!(skipped(&report, "index"), SkipReason::IndexChanged);
    assert_eq!(skipped(&report, "content"), SkipReason::IndexChanged);
    assert_eq!(skipped(&report, "held"), SkipReason::Active);
    assert_eq!(skipped(&report, "released"), SkipReason::RevisionChanged);
    // No refs were written for a worktree that was skipped before any recovery began.
    assert!(lab.other_refs().is_empty(), "{:?}", lab.other_refs());
}

#[test]
fn the_last_look_before_removal_catches_a_change_made_during_the_cleanup() {
    let lab = Lab::new();
    // The change lands after the recovery is recorded and before the removal: the instant the last
    // recheck exists for. It is made by a hook, from inside the cleanup, with real Git and files.
    let mutate: Arc<Mutex<Option<Action>>> = Arc::new(Mutex::new(None));
    let hook_target = Arc::clone(&mutate);
    let retention = lab.retention().with_hook(move |stage, _| {
        if stage == Stage::RecoveryRecorded {
            if let Some(action) = hook_target.lock().expect("lock").as_ref() {
                action();
            }
        }
    });
    retention
        .create(&lab.repo, "late-edit", None)
        .expect("late-edit");
    retention
        .create(&lab.repo, "late-commit", None)
        .expect("late-commit");
    let path = |id: &str| PathBuf::from(&retention.get(id).expect(id).path);
    let edit = path("late-edit");
    let commit = path("late-commit");

    // 1. A file is edited after the snapshot was taken.
    write(&edit.join("work.txt"), "before\n");
    let set = retention.prepare(&ids(&["late-edit"])).expect("prepare");
    *mutate.lock().expect("lock") = Some(Box::new({
        let edit = edit.clone();
        move || write(&edit.join("work.txt"), "edited during the cleanup\n")
    }));
    let report = retention
        .cleanup(&set.token, &ids(&["late-edit"]), RemovalMode::Manual)
        .expect("cleanup");
    assert_eq!(skipped(&report, "late-edit"), SkipReason::IndexChanged);
    assert_eq!(
        std::fs::read_to_string(edit.join("work.txt")).expect("read"),
        "edited during the cleanup\n"
    );
    assert_eq!(
        retention.get("late-edit").expect("record").state,
        WorktreeState::Idle
    );

    // 2. A commit moves HEAD after the snapshot was taken.
    let set = retention.prepare(&ids(&["late-commit"])).expect("prepare");
    *mutate.lock().expect("lock") = Some(Box::new({
        let commit = commit.clone();
        move || {
            write(&commit.join("c.txt"), "c\n");
            git(&commit, &["add", "c.txt"]);
            git(&commit, &["commit", "-q", "-m", "late"]);
        }
    }));
    let report = retention
        .cleanup(&set.token, &ids(&["late-commit"]), RemovalMode::Manual)
        .expect("cleanup");
    assert_eq!(skipped(&report, "late-commit"), SkipReason::HeadMoved);
    assert!(commit.join("c.txt").exists());
    assert_eq!(
        retention.get("late-commit").expect("record").state,
        WorktreeState::Idle
    );
}

// --------------------------------------------------------------------------- the snapshot

#[test]
fn a_manual_cleanup_snapshots_then_removes_and_a_restore_brings_every_change_back() {
    let lab = Lab::new();
    let retention = lab.retention();
    retention.create(&lab.repo, "work", None).expect("work");
    let path = PathBuf::from(&retention.get("work").expect("work").path);
    make_work(&path);
    let status_before = status_of(&path);
    let files_before = tree_files(&path);
    let head = git(&path, &["rev-parse", "HEAD"]);
    let user_before = lab.user_state();

    let wanted = ids(&["work"]);
    let set = retention.prepare(&wanted).expect("prepare");
    assert!(set.entries[0].dirty);
    let report = retention
        .cleanup(&set.token, &wanted, RemovalMode::Manual)
        .expect("cleanup");
    assert_eq!(report.removed(), vec!["work"], "{report:?}");
    assert!(!path.exists(), "the checkout is gone");
    assert!(
        !git(&lab.repo, &["worktree", "list", "--porcelain"]).contains("/work"),
        "Git forgot the worktree"
    );

    // The recovery refs, under the private namespace, name the right commits.
    let record = retention.get("work").expect("record");
    assert_eq!(record.state, WorktreeState::Removed);
    let recovery = record.recovery.expect("recovery recorded");
    assert!(recovery.complete);
    assert!(
        recovery.ignored_files >= 1,
        "the ignored file is counted, not captured"
    );
    assert_eq!(recovery.head_ref, "refs/quansio/recovery/work/head");
    let snapshot = recovery.snapshot_ref.expect("snapshot ref");
    let index = recovery.index_ref.expect("index ref");
    assert_eq!(git(&lab.repo, &["rev-parse", &recovery.head_ref]), head);
    let parents = git(&lab.repo, &["rev-list", "--parents", "-n", "1", &snapshot]);
    let parents: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(
        parents.len(),
        3,
        "the snapshot has two parents: {parents:?}"
    );
    assert_eq!(parents[1], head, "its first parent is HEAD");
    assert_eq!(
        parents[2],
        git(&lab.repo, &["rev-parse", &index]),
        "its second parent is the index snapshot"
    );
    assert_eq!(
        git(&lab.repo, &["rev-parse", &format!("{index}^")]),
        head,
        "the index snapshot's parent is HEAD"
    );
    // The index snapshot holds what was staged (v2, not v3); the code snapshot holds the working tree.
    assert_eq!(
        git(&lab.repo, &["show", &format!("{index}:README.md")]),
        "readme v2 (staged)"
    );
    assert_eq!(
        git(&lab.repo, &["show", &format!("{snapshot}:README.md")]),
        "readme v3 (unstaged on top)"
    );
    assert_eq!(
        git(&lab.repo, &["show", &format!("{snapshot}:new_file.txt")]),
        "untracked work"
    );
    assert_eq!(
        git(&lab.repo, &["show", &format!("{snapshot}:deep/er/new.txt")]),
        "untracked and nested"
    );
    assert!(
        git(&lab.repo, &["ls-tree", "-r", "--name-only", &snapshot])
            .lines()
            .all(|name| name != "debug.log"),
        "ignored files are not captured"
    );
    assert!(
        git(&lab.repo, &["ls-tree", "-r", "--name-only", &snapshot])
            .lines()
            .all(|name| name != "notes/a.txt"),
        "a deleted file is absent from the snapshot"
    );

    // Cleanup left the person's own state byte for byte as it was, and added refs only in the namespace.
    assert_eq!(
        lab.user_state(),
        user_before,
        "the person's branches, tags, stash, HEAD and index are untouched"
    );
    let others = lab.other_refs();
    assert_eq!(
        others.iter().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "refs/quansio/recovery/work/head",
            "refs/quansio/recovery/work/index",
            "refs/quansio/recovery/work/snapshot"
        ]
    );

    // A request for the checkout points at the record, with the code the error table gives it.
    let missing = retention
        .require_checkout("work")
        .expect_err("the checkout is gone");
    assert_eq!(missing.code(), Some("WORKTREE_MISSING"));
    match &missing {
        RetentionError::WorktreeMissing {
            id,
            state,
            recovery_record,
        } => {
            assert_eq!(id, "work");
            assert_eq!(state, "REMOVED");
            assert!(
                Path::new(recovery_record).is_file(),
                "the record it points at exists: {recovery_record}"
            );
        }
        other => panic!("not WORKTREE_MISSING: {other:?}"),
    }
    assert!(missing.to_string().contains("recovery record"));

    // Restore: the same path, the same working tree, the same index.
    let restored = retention.restore("work").expect("restore");
    assert_eq!(restored.state, WorktreeState::Idle);
    assert_eq!(PathBuf::from(&restored.path), path);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        status_of(&path),
        status_before,
        "staged, unstaged, deleted and untracked files all come back as they were"
    );
    let mut expected = files_before;
    expected.remove("debug.log");
    assert_eq!(
        tree_files(&path),
        expected,
        "everything but the ignored file"
    );
    assert_eq!(lab.user_state(), user_before);
    // A second restore of a worktree that is not removed is refused, and an occupied path is never overwritten.
    assert!(matches!(
        retention.restore("work"),
        Err(RetentionError::NotRestorable { .. })
    ));
}

#[test]
fn building_the_snapshot_never_writes_the_worktrees_own_index() {
    let lab = Lab::new();
    // The hook reads the worktree's real index file at every stage after the snapshot exists.
    let seen: Arc<Mutex<Seen>> = Arc::new(Mutex::new(Vec::new()));
    let target: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let (seen_in, target_in) = (Arc::clone(&seen), Arc::clone(&target));
    let retention = lab.retention().with_hook(move |stage, _| {
        if let Some(index) = target_in.lock().expect("lock").as_ref() {
            if let Ok(bytes) = std::fs::read(index) {
                seen_in.lock().expect("lock").push((stage, bytes));
            }
        }
    });
    retention.create(&lab.repo, "ix", None).expect("ix");
    let path = PathBuf::from(&retention.get("ix").expect("ix").path);
    make_work(&path);
    let index_file = git(&path, &["rev-parse", "--absolute-git-dir"]);
    let index_file = PathBuf::from(index_file).join("index");
    let before = std::fs::read(&index_file).expect("index");
    *target.lock().expect("lock") = Some(index_file);
    let set = retention.prepare(&ids(&["ix"])).expect("prepare");
    let report = retention
        .cleanup(&set.token, &ids(&["ix"]), RemovalMode::Manual)
        .expect("cleanup");
    assert_eq!(report.removed(), vec!["ix"]);
    let seen = seen.lock().expect("lock");
    let stages: Vec<Stage> = seen.iter().map(|(stage, _)| *stage).collect();
    assert!(
        stages.contains(&Stage::RefsWritten) && stages.contains(&Stage::RemovingRecorded),
        "{stages:?}"
    );
    for (stage, bytes) in seen.iter() {
        assert_eq!(bytes, &before, "the worktree's index changed by {stage:?}");
    }
}

#[test]
fn a_cleanup_never_creates_or_moves_a_branch_tag_or_stash() {
    let lab = Lab::new();
    let retention = lab.retention();
    for id in ["one", "two"] {
        retention.create(&lab.repo, id, None).expect(id);
    }
    // One worktree sits on a branch the person made, with a commit of its own.
    let one = PathBuf::from(&retention.get("one").expect("one").path);
    git(&one, &["switch", "-q", "-c", "agent/topic"]);
    write(&one.join("topic.txt"), "t\n");
    git(&one, &["add", "topic.txt"]);
    git(&one, &["commit", "-q", "-m", "topic"]);
    retention.observe("one").expect("observe");
    make_work(&PathBuf::from(&retention.get("two").expect("two").path));
    let branches_before = git(
        &lab.repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
            "refs/tags",
            "refs/stash",
        ],
    );
    let set = retention.prepare(&ids(&["one", "two"])).expect("prepare");
    let report = retention
        .cleanup(&set.token, &ids(&["one", "two"]), RemovalMode::Manual)
        .expect("cleanup");
    assert_eq!(report.removed(), vec!["one", "two"], "{report:?}");
    assert_eq!(
        git(&lab.repo, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags", "refs/stash"]),
        branches_before,
        "the branch the worktree was on, and every other branch, tag and stash, is exactly as it was"
    );
    retention.restore("one").expect("restore one");
    retention.restore("two").expect("restore two");
    assert_eq!(
        git(&lab.repo, &["stash", "list"]).lines().count(),
        1,
        "the person's one stash is still the only one"
    );
}

// -------------------------------------------------------------------------------- the sweep

#[test]
fn the_automatic_sweep_removes_only_clean_idle_worktrees_beyond_the_limit_oldest_first() {
    let lab = Lab::new();
    let mut config = lab.config();
    config.max_resident = 3;
    config.sweep_max_per_run = 2;
    let retention = lab.retention_with(config.clone());
    // Created oldest to newest, each an hour apart.
    for id in [
        "w1-dirty",
        "w2-old",
        "w3-active",
        "w4-mid",
        "w5-new",
        "w6-newest",
    ] {
        retention.create(&lab.repo, id, None).expect(id);
        lab.advance(HOUR);
    }
    let path = |id: &str| PathBuf::from(&retention.get(id).expect(id).path);
    write(&path("w1-dirty").join("work.txt"), "unsaved agent work\n");
    retention.acquire("w3-active").expect("hold w3");
    lab.advance(config.sweep_min_idle_ms);

    // Six resident, three allowed: three over, but at most two per run. The dirty one is the oldest and
    // is not a candidate; the held one is not either.
    let report = retention
        .sweep()
        .expect("sweep")
        .expect("something to sweep");
    assert_eq!(report.mode, RemovalMode::Auto);
    assert_eq!(report.removed(), vec!["w2-old", "w4-mid"], "{report:?}");
    assert!(
        path("w1-dirty").join("work.txt").exists(),
        "a worktree with changes is never swept"
    );
    assert_eq!(
        retention.get("w3-active").expect("w3").state,
        WorktreeState::Active
    );
    // A sweep writes the head ref and no snapshot: there was nothing to lose.
    let swept = retention
        .get("w2-old")
        .expect("w2")
        .recovery
        .expect("recovery");
    assert!(swept.complete && swept.snapshot_ref.is_none() && swept.index_ref.is_none());
    assert_eq!(
        lab.other_refs(),
        vec![
            "refs/quansio/recovery/w2-old/head",
            "refs/quansio/recovery/w4-mid/head"
        ]
    );

    // Four resident is still over three: one more, the oldest clean idle one (w1 has changes and w3 is
    // held), and then nothing is over the limit.
    let again = retention.sweep().expect("sweep").expect("one more");
    assert_eq!(again.removed(), vec!["w5-new"], "{again:?}");
    assert!(
        retention.sweep().expect("sweep").is_none(),
        "at the limit nothing is swept"
    );
    assert!(path("w1-dirty").join("work.txt").exists());

    // Restoring a swept worktree is a checkout of its HEAD, at its own path.
    let restored = retention.restore("w2-old").expect("restore");
    assert!(PathBuf::from(&restored.path).join("README.md").exists());
    assert_eq!(restored.state, WorktreeState::Idle);
}

#[test]
fn a_sweep_does_not_remove_a_worktree_that_was_used_within_the_idle_time() {
    let lab = Lab::new();
    let mut config = lab.config();
    config.max_resident = 1;
    let retention = lab.retention_with(config.clone());
    retention.create(&lab.repo, "a", None).expect("a");
    retention.create(&lab.repo, "b", None).expect("b");
    lab.advance(config.sweep_min_idle_ms - 1);
    assert!(
        retention.sweep().expect("sweep").is_none(),
        "idle for less than the minimum"
    );
    lab.advance(1);
    assert_eq!(
        retention
            .sweep()
            .expect("sweep")
            .expect("now")
            .removed()
            .len(),
        1
    );
}

// ------------------------------------------------------------------------ crash recovery

/// Which stage the crash test stops the child at.
fn stage_named(name: &str) -> Stage {
    match name {
        "prepare" => Stage::PrepareWritten,
        "refs" => Stage::RefsWritten,
        "recovery" => Stage::RecoveryRecorded,
        "removing" => Stage::RemovingRecorded,
        "removed" => Stage::CheckoutRemoved,
        other => panic!("no stage {other}"),
    }
}

/// The child half of a crash test: runs a real cleanup and stops (is killed by the parent) at the stage.
/// Without the `QUANSIO_RETENTION_CHILD_*` variables it does nothing, so a normal run of the suite passes it.
#[test]
fn crash_child() {
    let Ok(root) = std::env::var("QUANSIO_RETENTION_CHILD_ROOT") else {
        return;
    };
    let stage = stage_named(&std::env::var("QUANSIO_RETENTION_CHILD_STAGE").expect("stage"));
    let marker = PathBuf::from(std::env::var("QUANSIO_RETENTION_CHILD_MARKER").expect("marker"));
    let wanted: Vec<String> = std::env::var("QUANSIO_RETENTION_CHILD_IDS")
        .expect("ids")
        .split(',')
        .map(str::to_string)
        .collect();
    let token = std::env::var("QUANSIO_RETENTION_CHILD_TOKEN").ok();
    // The parent's clock, so a token it prepared is not expired in the child.
    let clock: u64 = std::env::var("QUANSIO_RETENTION_CHILD_CLOCK")
        .expect("clock")
        .parse()
        .expect("number");
    let sandbox = Sandbox::empty()
        .authorize(RootScope::Workspace, &root)
        .expect("authorize");
    let retention = WorktreeRetention::new(&sandbox, RetentionConfig::shipped())
        .expect("host")
        .with_clock(move || clock)
        .with_hook(move |reached, _| {
            if reached == stage {
                std::fs::write(&marker, format!("{reached:?}")).expect("marker");
                // Wait to be killed; the parent sends SIGKILL.
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        });
    let token = match token {
        Some(token) => token,
        None => retention.prepare(&wanted).expect("prepare").token,
    };
    let _ = retention.cleanup(&token, &wanted, RemovalMode::Manual);
}

/// Run a cleanup in a child process, kill it with SIGKILL when it reaches `stage`, and return.
fn crash_at(lab: &Lab, stage: &str, wanted: &[&str], token: Option<&str>) {
    let marker = lab
        .workspace
        .parent()
        .expect("parent")
        .join(format!("reached-{stage}"));
    let _ = std::fs::remove_file(&marker);
    let mut command = Command::new(std::env::current_exe().expect("this test binary"));
    command
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env("QUANSIO_RETENTION_CHILD_ROOT", &lab.workspace)
        .env("QUANSIO_RETENTION_CHILD_STAGE", stage)
        .env("QUANSIO_RETENTION_CHILD_MARKER", &marker)
        .env("QUANSIO_RETENTION_CHILD_IDS", wanted.join(","))
        .env(
            "QUANSIO_RETENTION_CHILD_CLOCK",
            lab.clock.load(Ordering::SeqCst).to_string(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(token) = token {
        command.env("QUANSIO_RETENTION_CHILD_TOKEN", token);
    }
    let mut child = command.spawn().expect("spawn the child");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the child never reached {stage}");
        if let Some(status) = child.try_wait().expect("poll") {
            panic!("the child exited ({status}) before reaching {stage}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().expect("SIGKILL the child");
    let status = child.wait().expect("reap");
    assert!(
        !status.success(),
        "the child was killed, not finished: {status}"
    );
}

/// A host over the same workspace, as a restarted process would build it.
fn restarted(lab: &Lab) -> WorktreeRetention {
    lab.retention()
}

fn lab_with_work() -> (Lab, PathBuf, String, String) {
    let lab = Lab::new();
    let retention = lab.retention();
    retention.create(&lab.repo, "crashy", None).expect("crashy");
    let path = PathBuf::from(&retention.get("crashy").expect("crashy").path);
    make_work(&path);
    let status = status_of(&path);
    let head = git(&path, &["rev-parse", "HEAD"]);
    (lab, path, status, head)
}

#[test]
fn a_prepared_token_survives_a_crash_between_prepare_and_cleanup() {
    let (lab, path, status, _) = lab_with_work();
    // The child prepares and is killed with the prepared set on disk, before any cleanup began.
    crash_at(&lab, "prepare", &["crashy"], None);
    let retention = restarted(&lab);
    assert_eq!(
        retention.recover().expect("recover"),
        Default::default(),
        "nothing to resolve: no cleanup had begun"
    );
    assert!(
        path.is_dir() && status_of(&path) == status,
        "the worktree is untouched"
    );
    let prepared_dir = retention.managed_root().join(".quansio/prepared");
    let token = std::fs::read_dir(&prepared_dir)
        .expect("prepared dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".json")
                .to_string()
        })
        .next()
        .expect("the prepared set is on disk");
    // The same token, in the restarted process, completes.
    let report = retention
        .cleanup(&token, &ids(&["crashy"]), RemovalMode::Manual)
        .expect("cleanup after restart");
    assert_eq!(report.removed(), vec!["crashy"], "{report:?}");
    retention.restore("crashy").expect("restore");
    assert_eq!(status_of(&path), status);
}

#[test]
fn a_crash_before_the_recovery_is_complete_leaves_the_worktree_intact_and_cleanable() {
    for stage in ["refs", "recovery"] {
        let (lab, path, status, head) = lab_with_work();
        let user_before = lab.user_state();
        let token = lab
            .retention()
            .prepare(&ids(&["crashy"]))
            .expect("prepare")
            .token;
        crash_at(&lab, stage, &["crashy"], Some(&token));
        let retention = restarted(&lab);
        let report = retention.recover().expect("recover");
        assert!(
            report.completed.is_empty(),
            "{stage}: nothing was being removed: {report:?}"
        );
        // Whatever refs were written, the checkout is exactly as it was and the person's state untouched.
        assert!(path.is_dir(), "{stage}");
        assert_eq!(status_of(&path), status, "{stage}");
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), head, "{stage}");
        assert_eq!(
            retention.get("crashy").expect("record").state,
            WorktreeState::Idle,
            "{stage}"
        );
        assert_eq!(lab.user_state(), user_before, "{stage}");
        // The cleanup, started again from a fresh preview, completes and the work is restorable.
        let set = retention.prepare(&ids(&["crashy"])).expect("prepare again");
        let report = retention
            .cleanup(&set.token, &ids(&["crashy"]), RemovalMode::Manual)
            .expect("cleanup");
        // The record moved on while the crashed run saved its recovery, so the earlier token is spent
        // but a fresh preview is exact.
        assert_eq!(report.removed(), vec!["crashy"], "{stage}: {report:?}");
        retention.restore("crashy").expect("restore");
        assert_eq!(status_of(&path), status, "{stage}");
    }
}

#[test]
fn a_crash_after_removal_began_is_finished_or_undone_and_the_work_is_restorable() {
    for stage in ["removing", "removed"] {
        let (lab, path, status, head) = lab_with_work();
        let files = tree_files(&path);
        let user_before = lab.user_state();
        let token = lab
            .retention()
            .prepare(&ids(&["crashy"]))
            .expect("prepare")
            .token;
        crash_at(&lab, stage, &["crashy"], Some(&token));
        // Right after the kill the record says REMOVING (the crash is real; nothing has resolved it).
        let before = restarted(&lab).get("crashy").expect("record");
        assert_eq!(before.state, WorktreeState::Removing, "{stage}");
        assert!(
            before
                .recovery
                .as_ref()
                .is_some_and(|recovery| recovery.complete),
            "{stage}: removal never starts before the recovery is complete"
        );
        assert_eq!(
            path.exists(),
            stage == "removing",
            "{stage}: whether the checkout was removed before the kill"
        );

        let retention = restarted(&lab);
        let report = retention.recover().expect("recover");
        assert_eq!(
            report.completed,
            vec!["crashy".to_string()],
            "{stage}: {report:?}"
        );
        assert!(!path.exists(), "{stage}: the removal was finished");
        assert_eq!(
            retention.get("crashy").expect("record").state,
            WorktreeState::Removed,
            "{stage}"
        );
        assert!(
            !git(&lab.repo, &["worktree", "list", "--porcelain"]).contains("crashy"),
            "{stage}: Git no longer lists it"
        );
        // The work is in the recovery refs: restore reproduces it.
        retention.restore("crashy").expect("restore");
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), head, "{stage}");
        assert_eq!(status_of(&path), status, "{stage}");
        let mut expected = files.clone();
        expected.remove("debug.log");
        assert_eq!(tree_files(&path), expected, "{stage}");
        assert_eq!(lab.user_state(), user_before, "{stage}");
    }
}

#[test]
fn a_removal_that_was_interrupted_is_abandoned_when_the_checkout_changed_since() {
    let (lab, path, _, _) = lab_with_work();
    let token = lab
        .retention()
        .prepare(&ids(&["crashy"]))
        .expect("prepare")
        .token;
    crash_at(&lab, "removing", &["crashy"], Some(&token));
    // Between the crash and the restart the person goes on working in the checkout.
    write(&path.join("after_the_crash.txt"), "new work\n");
    let retention = restarted(&lab);
    let report = retention.recover().expect("recover");
    assert_eq!(report.rolled_back, vec!["crashy".to_string()], "{report:?}");
    assert!(report.completed.is_empty());
    assert!(
        path.join("after_the_crash.txt").exists(),
        "the new work is still there"
    );
    assert_eq!(
        retention.get("crashy").expect("record").state,
        WorktreeState::Idle
    );
}

#[test]
fn a_half_deleted_checkout_is_finished_from_the_recovery_snapshot() {
    let (lab, path, status, _) = lab_with_work();
    let token = lab
        .retention()
        .prepare(&ids(&["crashy"]))
        .expect("prepare")
        .token;
    crash_at(&lab, "removing", &["crashy"], Some(&token));
    // As if the process died in the middle of deleting files: Git's pointer file and half the tree are gone.
    std::fs::remove_file(path.join(".git")).expect("remove the .git file");
    std::fs::remove_dir_all(path.join("src")).expect("remove half of the tree");
    let retention = restarted(&lab);
    let report = retention.recover().expect("recover");
    assert_eq!(report.completed, vec!["crashy".to_string()], "{report:?}");
    assert!(!path.exists());
    retention.restore("crashy").expect("restore");
    assert_eq!(status_of(&path), status);
}

#[test]
fn a_repeated_cleanup_with_the_same_token_resumes_where_the_crash_left_it() {
    let (lab, path, status, _) = lab_with_work();
    let token = lab
        .retention()
        .prepare(&ids(&["crashy"]))
        .expect("prepare")
        .token;
    crash_at(&lab, "removing", &["crashy"], Some(&token));
    // The restarted process repeats the same cleanup without calling recover first.
    let retention = restarted(&lab);
    let report = retention
        .cleanup(&token, &ids(&["crashy"]), RemovalMode::Manual)
        .expect("cleanup");
    assert!(report.accounts_for(&ids(&["crashy"])));
    assert_eq!(
        skipped(&report, "crashy"),
        SkipReason::AlreadyRemoved,
        "{report:?}"
    );
    retention.restore("crashy").expect("restore");
    assert_eq!(status_of(&path), status);
}

// ------------------------------------------------------------------------- configuration

#[test]
fn the_shipped_policy_is_valid_and_refuses_what_the_schema_refuses() {
    let shipped = RetentionConfig::shipped();
    assert!(shipped.ref_namespace.starts_with("refs/quansio/"));
    let text = include_str!("../../../config/worktree-retention.yaml");
    let with = |from: &str, to: &str| text.replace(from, to);
    assert!(RetentionConfig::from_yaml(text).is_ok());
    for (what, broken) in [
        (
            "a branch namespace",
            with("refs/quansio/recovery", "refs/heads/recovery"),
        ),
        (
            "a tag namespace",
            with("refs/quansio/recovery", "refs/tags/recovery"),
        ),
        (
            "a parent component",
            with("refs/quansio/recovery", "refs/quansio/../heads"),
        ),
        (
            "no resident limit",
            with("max_resident: 8", "max_resident: 0"),
        ),
        (
            "a sweep that is not bounded",
            with("max_per_run: 4", "max_per_run: 1000"),
        ),
        (
            "a sweep of worktrees just used",
            with("min_idle_ms: 3600000", "min_idle_ms: 1"),
        ),
        ("an unknown key", format!("{text}include_dirty: true\n")),
        (
            "a state directory that leaves the root",
            with("state_dir: .quansio", "state_dir: ../x"),
        ),
        (
            "a managed directory that is a path",
            with("managed_dir: agent-worktrees", "managed_dir: a/b"),
        ),
    ] {
        assert!(
            RetentionConfig::from_yaml(&broken).is_err(),
            "{what} must be refused"
        );
    }
}

#[test]
fn removal_settles_the_effect_class_the_taxonomy_gives_it() {
    // `config/effects.yaml: worktree_operations` is the map the retention host is held to.
    #[derive(serde::Deserialize)]
    struct Row {
        effect_class: String,
        tier: u8,
        default_decision: String,
    }
    #[derive(serde::Deserialize)]
    struct File {
        worktree_operations: BTreeMap<String, String>,
        effect_classes: Vec<Row>,
    }
    let file: File =
        serde_yaml::from_str(include_str!("../../../config/effects.yaml")).expect("effects.yaml");
    let class_of = |name: &str| {
        file.effect_classes
            .iter()
            .find(|row| row.effect_class == name)
    };
    use quansio_qworkerd::tools::scm::retention::RetentionOp;
    for op in RetentionOp::ALL {
        let class = file
            .worktree_operations
            .get(op.as_str())
            .unwrap_or_else(|| panic!("{} is not in worktree_operations", op.as_str()));
        assert_eq!(class, op.effect_class(), "{}", op.as_str());
        assert!(class_of(class).is_some(), "{class} is not in the taxonomy");
    }
    assert_eq!(
        file.worktree_operations.len(),
        RetentionOp::ALL.len(),
        "no operation in the map that the code does not have"
    );
    // Removal is not a tier-1 write and is not allowed without asking (a sweep's allow+log is a narrowing).
    let removal = class_of(RetentionOp::CleanupManual.effect_class()).expect("removal class");
    assert!(removal.tier >= 2, "removal is tier {}", removal.tier);
    assert!(
        removal.default_decision.starts_with("ask"),
        "{}",
        removal.default_decision
    );
    assert_eq!(
        RetentionOp::CleanupAuto.effect_class(),
        RetentionOp::CleanupManual.effect_class()
    );
}
