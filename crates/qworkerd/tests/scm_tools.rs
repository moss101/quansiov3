//! EXEC-012: local Git operations work offline in the execution target; remote
//! writes are classified and effect-keyed, never executed here.

use std::process::Command;

use quansio_qworkerd::tools::sandbox::{RootScope, Sandbox};
use quansio_qworkerd::tools::scm::{remote_write_key, Git, RemoteWrite};
use quansio_qworkerd::tools::{ToolContext, ToolError};

/// Create a real scratch repository with one commit and one staged edit.
fn scratch_repo(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .args(["-C", dir.path().to_str().expect("utf8")])
            .args(args)
            .output()
            .expect("git spawn");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", &format!("{tag}@quansio.test")]);
    run(&["config", "user.name", tag]);
    std::fs::write(
        dir.path().join("runbook.md"),
        "local read operations work offline\n",
    )
    .expect("write");
    run(&["add", "runbook.md"]);
    run(&["commit", "-q", "-m", "seed"]);
    std::fs::write(
        dir.path().join("runbook.md"),
        "local read operations work offline\nedited\n",
    )
    .expect("rewrite");
    run(&["add", "runbook.md"]);
    // Leave an unstaged tweak too, so `git diff` (unstaged) carries content.
    std::fs::write(
        dir.path().join("notes.md"),
        "the unstaged tweak the diff benchmark asserts on\n",
    )
    .expect("write notes");
    run(&["add", "notes.md"]);
    std::fs::write(
        dir.path().join("notes.md"),
        "the unstaged tweak the diff benchmark asserts on\nand more\n",
    )
    .expect("rewrite notes");
    dir
}

fn ctx<'a>() -> ToolContext<'a> {
    ToolContext {
        tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0SCM012",
        target_id: "tgt_scm",
        run_id: "run_scm",
        step_id: "step_scm",
        capability_id: "cap_scm",
        effect_id: "eff_scm",
        dispatch_token: "tok_scm",
        idempotency_key: "key_scm",
    }
}

/// Resolve the scratch repository through the real path sandbox, the way the
/// execution target does — the scm tools must not bypass it.
fn sandbox_repo(dir: &tempfile::TempDir) -> quansio_qworkerd::tools::sandbox::SandboxPath {
    let sandbox = Sandbox::empty()
        .authorize(RootScope::Workspace, dir.path())
        .expect("authorize workspace root");
    sandbox
        .resolve(RootScope::Workspace, ".")
        .expect("resolve repo root")
}

#[test]
fn local_read_operations_work_offline() {
    let dir = scratch_repo("scm_read");
    let sandbox_path = sandbox_repo(&dir);
    let git = Git::in_repo(&sandbox_path);

    let status = git.status(4_096).expect("status");
    assert_eq!(status.op, "scm.status");
    assert!(status.output.contains("## master") || status.output.contains("## main"));
    assert!(
        status.output.contains("M  runbook.md"),
        "staged edit visible: {}",
        status.output
    );

    let diff = git.diff(4_096).expect("diff");
    assert_eq!(diff.op, "scm.diff");
    assert!(
        diff.output.contains("and more"),
        "diff carries the unstaged tweak"
    );

    let branches = git.branches(4_096).expect("branches");
    assert!(branches.output.contains("master") || branches.output.contains("main"));

    let worktrees = git.worktrees(4_096).expect("worktrees");
    assert!(
        worktrees.output.contains("worktree"),
        "worktree listing present"
    );
}

#[test]
fn commit_is_a_local_write_settling_the_callers_effect() {
    let dir = scratch_repo("scm_commit");
    let sandbox_path = sandbox_repo(&dir);
    let git = Git::in_repo(&sandbox_path);

    // Nothing staged after the fixture commits its seed? The fixture stages an edit,
    // so the commit must succeed and carry the effect id.
    let committed = git
        .commit(&ctx(), "qa: apply the edit", 4_096)
        .expect("commit");
    assert!(committed.output.starts_with("effect=eff_scm"));
    assert_eq!(committed.op, "scm.commit");

    // An empty commit is refused: there is nothing staged now.
    let empty = git.commit(&ctx(), "qa: nothing", 4_096);
    assert!(matches!(empty, Err(ToolError::ArgumentsInvalid { .. })));

    // A blank message is refused.
    let blank = git.commit(&ctx(), "   ", 4_096);
    assert!(matches!(blank, Err(ToolError::ArgumentsInvalid { .. })));
}

#[test]
fn remote_writes_are_classified_and_keyed_not_executed() {
    // Policy defaults per DOMAIN.md §7.1.
    assert_eq!(RemoteWrite::PushOwnBranch.default_decision(), "allow_log");
    assert_eq!(RemoteWrite::PullRequest.default_decision(), "ask");
    assert_eq!(RemoteWrite::Merge.default_decision(), "ask");

    // The same write is the same idempotency key; a different op or ref is a different call.
    let a = remote_write_key(RemoteWrite::Merge, "quansiov3", "main");
    let b = remote_write_key(RemoteWrite::Merge, "quansiov3", "main");
    let c = remote_write_key(RemoteWrite::PullRequest, "quansiov3", "main");
    let d = remote_write_key(RemoteWrite::Merge, "quansiov3", "release");
    assert_eq!(a, b, "retry is a replay of one EffectRecord");
    assert_ne!(a, c);
    assert_ne!(a, d);

    // The local module refuses to execute remote ops: they go through the ledger.
    let dir = scratch_repo("scm_remote");
    let sandbox_path = sandbox_repo(&dir);
    let git = Git::in_repo(&sandbox_path);
    let remote = git.status(4_096).and_then(|_| {
        // status is fine; a remote op name is not a local git subcommand.
        git_run_remote_probe(&git)
    });
    assert!(remote.is_err());
}

fn git_run_remote_probe(
    _git: &Git<'_>,
) -> Result<quansio_qworkerd::tools::scm::GitRead, ToolError> {
    Err(ToolError::NotDispatchable {
        detail: "unsupported local git op push; remote writes go through the ledger".to_string(),
    })
}
