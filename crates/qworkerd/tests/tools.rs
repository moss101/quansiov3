//! The tool host's rules (EXEC-006).
//!
//! The three tests the task names — path traversal, PTY reconnect, command cancellation — plus the
//! properties the two acceptance statements rest on: a path that leaves its authorized root is refused
//! before any I/O, and a reconnecting terminal resumes from its durable cursor without running its
//! command a second time.
//!
//! These drive the shipped code over a real filesystem and real child processes; the only thing stood in
//! for is the terminal conduit, which is a trait because a target's terminal is the target's business
//! (see `terminal.rs`). A scratch directory is created per test, so nothing here touches a real tree.

use std::path::PathBuf;

use quansio_core::Digest;
use quansio_qworkerd::tools::{
    advance_cursor, decide, Attachment, CancelToken, FileHost, ProcessHost, RootScope, Sandbox,
    SpawnRequest, TerminalCommand, TerminalSession, TerminalStatus, ToolContext, ToolError,
};

fn context<'a>(effect: &'a str, key: &'a str) -> ToolContext<'a> {
    ToolContext {
        tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        target_id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        capability_id: "cap_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        effect_id: effect,
        dispatch_token: "dsp_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        idempotency_key: key,
    }
}

/// A scratch workspace root, plus a directory outside it that a request must never reach.
struct Tree {
    _holder: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let holder = tempfile::tempdir().expect("temp dir");
        let root = holder.path().join("workspace");
        let outside = holder.path().join("outside");
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::create_dir_all(&outside).expect("outside root");
        std::fs::write(outside.join("secret.txt"), b"not yours").expect("write outside");
        Self {
            _holder: holder,
            root,
            outside,
        }
    }

    fn host(&self) -> FileHost {
        let sandbox = Sandbox::empty()
            .authorize(RootScope::Workspace, &self.root)
            .expect("authorize workspace");
        FileHost::new(sandbox)
    }
}

// ------------------------------------------------------------------ path traversal

#[test]
fn a_path_that_leaves_its_root_is_refused_before_any_io() {
    let tree = Tree::new();
    let host = tree.host();
    std::fs::write(tree.root.join("inside.txt"), b"mine").expect("write inside");

    // A parent component is refused outright, whether or not it would still land inside the root.
    for request in [
        "../outside/secret.txt",
        "a/../../outside/secret.txt",
        "..",
        "./../outside",
    ] {
        let refusal = host
            .read(RootScope::Workspace, request, 4096)
            .expect_err("must be refused");
        assert!(
            matches!(refusal, ToolError::Path(_)),
            "{request} was not refused as a path: {refusal:?}"
        );
    }

    // An absolute path is not relative to a root, so it is refused rather than reinterpreted.
    let refusal = host
        .read(RootScope::Workspace, "/etc/passwd", 4096)
        .expect_err("absolute must be refused");
    assert!(
        matches!(
            refusal,
            ToolError::Path(quansio_qworkerd::tools::PathRefusal::Absolute { .. })
        ),
        "{refusal:?}"
    );

    // An unauthorized root is denied before any path is considered: the host root was never authorized.
    let refusal = host
        .read(RootScope::Host, "etc/passwd", 4096)
        .expect_err("unauthorized root must be refused");
    assert!(
        matches!(
            refusal,
            ToolError::Path(quansio_qworkerd::tools::PathRefusal::RootNotAuthorized {
                scope: "host"
            })
        ),
        "{refusal:?}"
    );
    assert!(!host.sandbox().has_root(RootScope::Host));
    assert!(host.sandbox().has_root(RootScope::Workspace));

    // A symbolic link inside the root that points outside it is refused: the path is textually inside,
    // so only resolution catches it.
    std::os::unix::fs::symlink(
        tree.outside.join("secret.txt"),
        tree.root.join("escape.txt"),
    )
    .expect("symlink");
    let refusal = host
        .read(RootScope::Workspace, "escape.txt", 4096)
        .expect_err("symlink escape must be refused");
    assert!(
        matches!(
            refusal,
            ToolError::Path(quansio_qworkerd::tools::PathRefusal::SymlinkEscape { .. })
        ),
        "{refusal:?}"
    );

    // Writing through the same link is refused too, so the rule is not read-only.
    let content = b"overwrite";
    let refusal = host
        .write(
            &context("eff_write", "idem_write"),
            RootScope::Workspace,
            "escape.txt",
            content,
            Digest::of(content).as_str(),
        )
        .expect_err("symlink escape must be refused for a write");
    assert!(matches!(refusal, ToolError::Path(_)), "{refusal:?}");
    assert_eq!(
        std::fs::read(tree.outside.join("secret.txt")).expect("read"),
        b"not yours",
        "the file outside the root was written through the link"
    );

    // The ordinary case works, so the refusals above are the path rules and not a broken host.
    assert_eq!(
        host.read(RootScope::Workspace, "inside.txt", 4096)
            .expect("read inside")
            .content,
        b"mine"
    );
    assert_eq!(
        host.read(RootScope::Workspace, "./inside.txt", 4096)
            .expect("a `.` component is not a traversal")
            .content,
        b"mine"
    );
}

// ------------------------------------------------------------------ files

#[test]
fn a_write_checks_its_declared_digest_and_reads_back_with_provenance() {
    let tree = Tree::new();
    let host = tree.host();
    let content = b"hello\nworld\n";

    // A declared digest that does not match the content is refused, and nothing is written.
    let refusal = host
        .write(
            &context("eff_write", "idem_write"),
            RootScope::Workspace,
            "note.txt",
            content,
            &"0".repeat(64),
        )
        .expect_err("a wrong digest must be refused");
    assert!(
        matches!(refusal, ToolError::DigestMismatch { .. }),
        "{refusal:?}"
    );
    assert!(!tree.root.join("note.txt").exists(), "nothing was written");

    let written = host
        .write(
            &context("eff_write", "idem_write"),
            RootScope::Workspace,
            "notes/note.txt",
            content,
            Digest::of(content).as_str(),
        )
        .expect("write");
    assert_eq!(written.bytes_written, content.len() as u64);
    assert_eq!(written.content_digest, Digest::of(content).as_str());
    assert!(!written.replaced);
    assert_eq!(written.effect_id, "eff_write");
    assert_eq!(
        written.idempotency_key, "idem_write",
        "the mutation carries what it settles"
    );

    // The parents a write needed were created inside the root, not beside it.
    assert!(tree.root.join("notes").is_dir());
    assert_eq!(
        std::fs::read_dir(&tree.root)
            .expect("root")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["notes".to_string()],
        "the write stayed inside its root"
    );

    let read = host
        .read(RootScope::Workspace, "notes/note.txt", 4096)
        .expect("read");
    assert_eq!(read.content, content);
    assert!(!read.truncated);
    assert_eq!(read.content_digest, written.content_digest);

    // A bound smaller than the file truncates and says so, and the digest is still the whole file's.
    let bounded = host
        .read(RootScope::Workspace, "notes/note.txt", 4)
        .expect("read");
    assert_eq!(bounded.content, b"hell");
    assert!(bounded.truncated);
    assert_eq!(bounded.size_bytes, content.len() as u64);
    assert_eq!(bounded.content_digest, Digest::of(content).as_str());

    // Rewriting is reported as a replacement, so a caller can tell an edit from a create.
    let again = host
        .write(
            &context("eff_write_2", "idem_write_2"),
            RootScope::Workspace,
            "notes/note.txt",
            b"replaced\n",
            Digest::of(b"replaced\n").as_str(),
        )
        .expect("rewrite");
    assert!(again.replaced);

    // A directory listing is sorted and bounded, and hiding nothing.
    let listing = host
        .list(RootScope::Workspace, ".", 100)
        .expect("list root");
    assert_eq!(listing.entries.len(), 1);
    assert!(listing.entries[0].is_dir);
    assert!(!listing.truncated);
    let bounded = host.list(RootScope::Workspace, ".", 0).expect("list");
    assert!(bounded.entries.is_empty());
    assert!(bounded.truncated);
}

#[test]
fn a_patch_applies_all_or_nothing() {
    let tree = Tree::new();
    let host = tree.host();
    let original = "one\ntwo\nthree\n";
    host.write(
        &context("eff_write", "idem_write"),
        RootScope::Workspace,
        "file.txt",
        original.as_bytes(),
        Digest::of(original.as_bytes()).as_str(),
    )
    .expect("write");

    // A hunk whose context does not match refuses the whole patch and leaves the file alone.
    let bad = "@@ -2,1 +2,1 @@\n-not the line\n+changed\n";
    let refusal = host
        .patch(
            &context("eff_patch", "idem_patch"),
            RootScope::Workspace,
            "file.txt",
            bad,
        )
        .expect_err("a mismatched hunk must be refused");
    assert!(
        matches!(refusal, ToolError::PatchRejected { .. }),
        "{refusal:?}"
    );
    assert_eq!(
        std::fs::read_to_string(tree.root.join("file.txt")).expect("read"),
        original,
        "a refused patch changed the file"
    );

    let good = "@@ -1,3 +1,4 @@\n one\n+one and a half\n two\n-three\n+three, changed\n";
    let applied = host
        .patch(
            &context("eff_patch", "idem_patch"),
            RootScope::Workspace,
            "file.txt",
            good,
        )
        .expect("patch");
    assert_eq!(applied.hunks, 1);
    assert_eq!(applied.added, 2);
    assert_eq!(applied.removed, 1);
    assert_eq!(
        std::fs::read_to_string(tree.root.join("file.txt")).expect("read"),
        "one\none and a half\ntwo\nthree, changed\n"
    );
    assert_eq!(
        applied.content_digest,
        Digest::of(b"one\none and a half\ntwo\nthree, changed\n").as_str()
    );

    // A patch with no hunks is refused rather than reported as a successful no-op.
    assert!(matches!(
        host.patch(
            &context("eff_patch", "idem_patch"),
            RootScope::Workspace,
            "file.txt",
            "--- a\n+++ b\n"
        ),
        Err(ToolError::PatchRejected { .. })
    ));
}

// ------------------------------------------------------------------ processes

#[tokio::test]
async fn output_is_bounded_while_a_command_runs() {
    let tree = Tree::new();
    let host = ProcessHost::new();
    let cwd = tree
        .host()
        .sandbox()
        .resolve(RootScope::Workspace, ".")
        .expect("root");
    let mut cancel = CancelToken::new();

    let outcome = host
        .exec(
            &context("eff_exec", "idem_exec"),
            &SpawnRequest {
                command: "printf 'a%.0s' $(seq 1 20000)",
                cwd: &cwd,
                max_output_bytes: 128,
                timeout_ms: 30_000,
            },
            &mut cancel,
        )
        .await
        .expect("exec");
    assert!(outcome.succeeded(), "{outcome:?}");
    assert_eq!(outcome.output.len(), 128, "the bound held");
    assert!(outcome.truncated);
    assert_eq!(
        outcome.output_bytes, 20_000,
        "what the command produced is counted, not guessed"
    );
    assert_eq!(outcome.effect_id, "eff_exec");

    // A command that fits is returned whole and not marked truncated.
    let outcome = host
        .exec(
            &context("eff_exec_2", "idem_exec_2"),
            &SpawnRequest {
                command: "printf 'hello'",
                cwd: &cwd,
                max_output_bytes: 4096,
                timeout_ms: 30_000,
            },
            &mut cancel,
        )
        .await
        .expect("exec");
    assert_eq!(outcome.output, b"hello");
    assert!(!outcome.truncated);
    assert_eq!(outcome.exit_code, Some(0));

    // A failing command's status comes back rather than being reported as success.
    let outcome = host
        .exec(
            &context("eff_exec_3", "idem_exec_3"),
            &SpawnRequest {
                command: "exit 3",
                cwd: &cwd,
                max_output_bytes: 4096,
                timeout_ms: 30_000,
            },
            &mut cancel,
        )
        .await
        .expect("exec");
    assert_eq!(outcome.exit_code, Some(3));
    assert!(!outcome.succeeded());
}

#[tokio::test]
async fn a_command_is_cancelled_while_it_runs_and_stopped_when_it_is() {
    let tree = Tree::new();
    let host = ProcessHost::new();
    let cwd = tree
        .host()
        .sandbox()
        .resolve(RootScope::Workspace, ".")
        .expect("root");
    let mut cancel = CancelToken::new();

    // The command writes a marker if it ever reaches its end. If cancellation stopped it, the marker is
    // absent — which is a statement about the process, not about this worker's bookkeeping.
    let marker = tree.root.join("finished");
    let command = format!("sleep 30; touch {}", marker.to_str().expect("marker path"));
    let cancellation = cancel.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cancellation.cancel();
    });

    let refusal = host
        .exec(
            &context("eff_exec", "idem_exec"),
            &SpawnRequest {
                command: &command,
                cwd: &cwd,
                max_output_bytes: 4096,
                timeout_ms: 60_000,
            },
            &mut cancel,
        )
        .await
        .expect_err("a cancelled command must not report success");
    assert_eq!(refusal, ToolError::Cancelled);
    canceller.await.expect("join");
    assert!(
        !marker.exists(),
        "the process ran to completion after being cancelled"
    );

    // A deadline stops the command the same way, and says which deadline it was.
    let mut fresh = CancelToken::new();
    let marker = tree.root.join("timed-out");
    let command = format!("sleep 30; touch {}", marker.to_str().expect("marker"));
    let refusal = host
        .exec(
            &context("eff_exec_2", "idem_exec_2"),
            &SpawnRequest {
                command: &command,
                cwd: &cwd,
                max_output_bytes: 4096,
                timeout_ms: 200,
            },
            &mut fresh,
        )
        .await
        .expect_err("a command past its deadline must not report success");
    assert_eq!(refusal, ToolError::Timeout { timeout_ms: 200 });
    assert!(
        !marker.exists(),
        "the process outlived its deadline and completed"
    );

    // A token cancelled before the call is observed without starting anything.
    let mut already = CancelToken::new();
    already.cancel();
    assert!(already.is_cancelled());
    assert_eq!(
        host.exec(
            &context("eff_exec_3", "idem_exec_3"),
            &SpawnRequest {
                command: "touch should-not-exist",
                cwd: &cwd,
                max_output_bytes: 4096,
                timeout_ms: 5_000,
            },
            &mut already,
        )
        .await
        .expect_err("an already-cancelled token must refuse"),
        ToolError::Cancelled
    );
    assert!(!tree.root.join("should-not-exist").exists());

    // A spawn returns a handle without waiting, and the command runs.
    let mut handle = host
        .spawn(
            &context("eff_spawn", "idem_spawn"),
            &SpawnRequest {
                command: "sleep 5",
                cwd: &cwd,
                max_output_bytes: 4096,
                timeout_ms: 5_000,
            },
        )
        .expect("spawn");
    let pid = handle.pid();
    assert!(pid > 0);
    assert!(alive(pid), "the spawned process did not start");
    assert_eq!(handle.effect_id, "eff_spawn");
    let mut host = host;
    host.cancel(&mut handle)
        .expect("cancel the spawned process");
    // And it is gone: the handle was signalled *and* reaped, so the kernel has released the pid rather
    // than leaving a zombie that still answers `kill -0`.
    assert!(
        !alive(pid),
        "the spawned process is still running after being cancelled"
    );
    // Cancelling an already-reaped process is a no-op, not an error.
    host.cancel(&mut handle).expect("a second cancel is safe");
}

/// Whether a pid is still alive, by asking the kernel rather than by inspecting `/proc`, which does not
/// exist on every platform this worker runs on.
fn alive(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

// ------------------------------------------------------------------ PTY / terminal reconnect

#[test]
fn a_reconnect_resumes_from_the_durable_cursor_without_running_the_command_again() {
    let mut session = TerminalSession::open(
        "tsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
    );
    let cwd = PathBuf::from("/workspace");
    let command = TerminalCommand {
        command_id: "tc_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        command: "echo hello",
        cwd: cwd.clone(),
        max_output_bytes: 4096,
        timeout_ms: 30_000,
    };

    // The first attach has nothing to resume, so it dispatches the command.
    assert_eq!(
        decide(&session, &command).expect("decide"),
        Attachment::Dispatch {
            command_id: command.command_id.to_string(),
            from: 0,
        }
    );

    // The command ran: the session records it and the client consumed up to an offset.
    session.last_command_id = Some(command.command_id.to_string());
    let cursor = advance_cursor(&session, 6).expect("advance");
    assert_eq!(cursor, 6);
    session.cursor = cursor;

    // The reconnect attaches the same command. It is *not* run again — that is the duplicate the
    // acceptance statement forbids — and the client is told where to resume reading.
    assert_eq!(
        decide(&session, &command).expect("decide"),
        Attachment::Replay {
            command_id: command.command_id.to_string(),
            from: 6,
        },
        "a reconnect re-ran a command that had already run"
    );

    // A different command on the same session is a dispatch, at the current offset, so a session can be
    // reused without its history confusing the next call.
    let next = TerminalCommand {
        command_id: "tc_01J8Z3K6F1N8VQ2X5W9Y0GGGGG",
        command: "echo again",
        cwd,
        max_output_bytes: 4096,
        timeout_ms: 30_000,
    };
    assert_eq!(
        decide(&session, &next).expect("decide"),
        Attachment::Dispatch {
            command_id: next.command_id.to_string(),
            from: 6,
        }
    );

    // The cursor is monotonic: a client cannot ask to re-read bytes it has already consumed.
    assert_eq!(
        advance_cursor(&session, 5).expect_err("backwards"),
        ToolError::CursorInvalid {
            cursor: "5".to_string()
        }
    );
    assert_eq!(advance_cursor(&session, 6).expect("same"), 6);

    // A session that is not open refuses the attach rather than replaying or dispatching.
    for status in [TerminalStatus::Closed, TerminalStatus::Lost] {
        let mut down = session.clone();
        down.status = status;
        assert_eq!(
            decide(&down, &command).expect_err("not open"),
            ToolError::SessionNotOpen {
                id: down.id.clone(),
                status: status.as_str(),
            }
        );
    }

    // An attach with no command id or line is refused, so a malformed retry cannot look like a replay.
    let mut open = session.clone();
    open.status = TerminalStatus::Open;
    let empty = TerminalCommand {
        command_id: "  ",
        command: "echo hello",
        cwd: PathBuf::from("/workspace"),
        max_output_bytes: 4096,
        timeout_ms: 30_000,
    };
    assert!(matches!(
        decide(&open, &empty).expect_err("empty id"),
        ToolError::ArgumentsInvalid { .. }
    ));
    assert_eq!(TerminalStatus::parse("lost"), Some(TerminalStatus::Lost));
    assert_eq!(TerminalStatus::parse("gone"), None);
    assert!(!TerminalStatus::Closed.is_open());
}

#[test]
fn a_root_must_be_absolute_existing_and_a_directory() {
    let tree = Tree::new();

    // A relative root is refused rather than resolved against the process's own directory, which nobody
    // chose.
    let refusal = Sandbox::empty()
        .authorize(RootScope::Workspace, "workspace")
        .expect_err("a relative root must be refused");
    assert!(
        matches!(
            refusal,
            quansio_qworkerd::tools::PathRefusal::RootUnusable { .. }
        ),
        "{refusal:?}"
    );

    // So is one that does not exist, rather than being created: making a directory at a mistyped path is
    // how a tool ends up writing where nobody intended.
    let missing = tree.root.join("does-not-exist");
    assert!(Sandbox::empty()
        .authorize(RootScope::Workspace, &missing)
        .is_err());

    // And one that is a file, not a directory.
    let file = tree.root.join("inside.txt");
    std::fs::write(&file, b"x").expect("write");
    assert!(Sandbox::empty()
        .authorize(RootScope::Workspace, &file)
        .is_err());

    // A directory is accepted, and an empty request is refused rather than resolving to the root.
    let host = tree.host();
    assert!(matches!(
        host.read(RootScope::Workspace, "   ", 16),
        Err(ToolError::Path(_))
    ));
}
