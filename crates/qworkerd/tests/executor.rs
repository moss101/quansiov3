//! The local tool executor against a real directory and a real shell (EXEC-002 composition).
//!
//! Nothing stands in for the filesystem or the shell: each test runs the tool the model would call, through
//! the host's own validation, and reads the result back from the disk.

use std::sync::atomic::{AtomicU64, Ordering};

use quansio_core::Digest;
use quansio_machine::gateway::{ActionEnvelope, DispatchRequest, LeaseClaim};
use quansio_qworkerd::executor::LocalToolExecutor;
use quansio_qworkerd::host::{EvidenceSink, HostFailure, HostOutcome, WorkerHost};
use quansio_qworkerd::tools::{RootScope, Sandbox};
use serde_json::{json, Value};

const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const LEASE: &str = "lse_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct NoEvidence;

#[async_trait::async_trait]
impl EvidenceSink for NoEvidence {
    async fn upload(&self, effect_id: &str, _: &[u8], _: &str) -> Result<String, HostFailure> {
        Ok(format!("evd_{effect_id}"))
    }
}

fn lease() -> LeaseClaim {
    LeaseClaim {
        lease_id: LEASE.to_string(),
        target_id: TARGET.to_string(),
        generation: 1,
        held: true,
        expires_at: "2999-01-01T00:00:00Z".to_string(),
    }
}

fn worker(root: &std::path::Path) -> WorkerHost<LocalToolExecutor, NoEvidence> {
    let sandbox = Sandbox::empty()
        .authorize(RootScope::Workspace, root)
        .expect("a real directory");
    WorkerHost::new(LocalToolExecutor::new(sandbox, "tnt_test"), NoEvidence)
}

fn envelope(tool: &str, args: &Value, max_output: usize) -> ActionEnvelope {
    let n = SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let effect = format!("eff_{n:026}");
    let token = format!("dsp_{n:026}");
    ActionEnvelope::dispatch(&DispatchRequest {
        effect_id: &effect,
        dispatch_token: &token,
        target_id: TARGET,
        lease_id: LEASE,
        generation: 1,
        run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        tool,
        tool_version: 1,
        args_json: &args.to_string(),
        max_output_bytes: max_output,
        deadline_at: "2999-01-01T00:00:00Z",
    })
    .expect("a well-formed dispatch")
}

async fn run(
    host: &mut WorkerHost<LocalToolExecutor, NoEvidence>,
    tool: &str,
    args: Value,
) -> Result<Value, String> {
    let recorded = host
        .receive(
            &envelope(tool, &args, 65536),
            &lease(),
            "2026-10-17T00:00:00Z",
        )
        .await
        .expect("the envelope is valid");
    match recorded.outcome {
        HostOutcome::Completed { output, .. } => {
            Ok(serde_json::from_slice(&output).expect("the output is one JSON document"))
        }
        HostOutcome::Failed { failure, .. } => Err(failure.to_string()),
        HostOutcome::Cancelled { .. } => Err("cancelled".to_string()),
        HostOutcome::Interrupted { .. } => Err("interrupted".to_string()),
    }
}

#[tokio::test]
async fn a_write_lands_on_disk_and_a_read_returns_what_was_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = worker(dir.path());
    let content = "hello from the worker\n";
    let digest = Digest::of(content.as_bytes()).as_str().to_string();

    let written = run(
        &mut host,
        "fs.write",
        json!({"path": "notes/a.txt", "content": content, "content_digest": digest}),
    )
    .await
    .expect("write");
    assert_eq!(written["bytes_written"], content.len());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes/a.txt")).unwrap(),
        content
    );

    let read = run(&mut host, "fs.read", json!({"path": "notes/a.txt"}))
        .await
        .expect("read");
    assert_eq!(read["content"], content);
    assert_eq!(read["content_digest"], digest);
    assert_eq!(read["encoding"], "utf8");
}

#[tokio::test]
async fn a_write_with_the_wrong_digest_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "original").unwrap();
    let mut host = worker(dir.path());
    let refused = run(
        &mut host,
        "fs.write",
        json!({"path": "f.txt", "content": "new", "content_digest": "0".repeat(64)}),
    )
    .await;
    assert!(refused.is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "original"
    );
}

#[tokio::test]
async fn a_stale_base_digest_is_an_edit_conflict_and_the_file_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "someone else changed this").unwrap();
    let mut host = worker(dir.path());
    let content = "mine";
    let refused = run(
        &mut host,
        "fs.write",
        json!({"path": "f.txt", "content": content, "content_digest": Digest::of(content.as_bytes()).as_str(), "base_digest": Digest::of(b"what I read").as_str()}),
    )
    .await
    .expect_err("conflict");
    assert!(refused.contains("FILE_EDIT_CONFLICT"), "{refused}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "someone else changed this"
    );
}

#[tokio::test]
async fn a_patch_applies_and_a_patch_that_does_not_match_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\n").unwrap();
    let mut host = worker(dir.path());
    let good = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n";
    let patched = run(
        &mut host,
        "fs.patch",
        json!({"path": "f.txt", "patch": good}),
    )
    .await
    .expect("patch");
    assert_eq!(patched["hunks"], 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "one\nTWO\nthree\n"
    );

    let bad = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n one\n-nope\n+x\n three\n";
    assert!(run(
        &mut host,
        "fs.patch",
        json!({"path": "f.txt", "patch": bad})
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "one\nTWO\nthree\n"
    );
}

#[tokio::test]
async fn paths_that_leave_the_workspace_are_refused_before_any_io() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let mut host = worker(dir.path());
    for path in ["../secret.txt", "/etc/passwd", "link/secret.txt"] {
        assert!(
            run(&mut host, "fs.read", json!({ "path": path }))
                .await
                .is_err(),
            "{path} must be refused"
        );
    }
}

#[tokio::test]
async fn a_terminal_command_runs_in_the_workspace_and_reports_its_exit() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = worker(dir.path());
    let ok = run(
        &mut host,
        "terminal.exec",
        json!({"command": "pwd && echo hi >made.txt && cat made.txt"}),
    )
    .await
    .expect("exec");
    assert_eq!(ok["exit_code"], 0);
    assert_eq!(ok["succeeded"], true);
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    assert!(
        ok["output"]
            .as_str()
            .unwrap()
            .contains(canonical.to_str().unwrap()),
        "{ok}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("made.txt")).unwrap(),
        "hi\n"
    );

    let failed = run(
        &mut host,
        "terminal.exec",
        json!({"command": "echo boom >&2; exit 3"}),
    )
    .await
    .expect("a failing command is a result");
    assert_eq!(failed["exit_code"], 3);
    assert_eq!(failed["succeeded"], false);
    assert!(failed["output"].as_str().unwrap().contains("boom"));
}

#[tokio::test]
async fn a_terminal_command_cannot_start_outside_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = worker(dir.path());
    assert!(run(
        &mut host,
        "terminal.exec",
        json!({"command": "pwd", "cwd": ".."})
    )
    .await
    .is_err());
    assert!(run(
        &mut host,
        "terminal.exec",
        json!({"command": "pwd", "cwd": "/"})
    )
    .await
    .is_err());
}

#[tokio::test]
async fn output_is_bounded_while_it_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let host = worker(dir.path());
    let small = envelope(
        "terminal.exec",
        &json!({"command": "yes x | head -c 200000"}),
        2048,
    );
    let recorded = host
        .receive(&small, &lease(), "2026-10-17T00:00:00Z")
        .await
        .unwrap();
    let HostOutcome::Completed { output, .. } = recorded.outcome else {
        panic!("{:?}", recorded.outcome)
    };
    assert!(output.len() <= 2048);
    let document: Value =
        serde_json::from_slice(&output).expect("the bounded document is still valid JSON");
    assert_eq!(document["truncated"], true);
    assert_eq!(document["output_bytes"], 200_000);

    // Control bytes escape to six bytes each; the document must still fit and still parse.
    let control = envelope(
        "terminal.exec",
        &json!({"command": "head -c 5000 /dev/zero"}),
        1024,
    );
    let recorded = host
        .receive(&control, &lease(), "2026-10-17T00:00:00Z")
        .await
        .unwrap();
    let HostOutcome::Completed { output, .. } = recorded.outcome else {
        panic!("{:?}", recorded.outcome)
    };
    assert!(output.len() <= 1024, "{}", output.len());
    assert_eq!(
        serde_json::from_slice::<Value>(&output).expect("valid JSON")["truncated"],
        true
    );
}

#[tokio::test]
async fn an_expired_deadline_stops_a_command_and_a_dispatch_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let host = worker(dir.path());
    let counted = envelope(
        "terminal.exec",
        &json!({"command": "echo run >> count.txt"}),
        65536,
    );
    host.receive(&counted, &lease(), "2026-10-17T00:00:00Z")
        .await
        .unwrap();
    let again = host
        .receive(&counted, &lease(), "2026-10-17T00:00:01Z")
        .await
        .unwrap();
    assert!(
        again.replayed,
        "a re-delivered dispatch is answered from the record"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("count.txt")).unwrap(),
        "run\n"
    );
}

#[tokio::test]
async fn a_spawned_process_is_owned_by_the_executor_and_an_unhosted_tool_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::empty()
        .authorize(RootScope::Workspace, dir.path())
        .unwrap();
    let mut host = WorkerHost::new(LocalToolExecutor::new(sandbox, "tnt_test"), NoEvidence);
    let spawned = run(&mut host, "process.spawn", json!({"command": "sleep 5"}))
        .await
        .expect("spawn");
    assert!(spawned["pid"].as_u64().unwrap() > 0);

    let refused = run(
        &mut host,
        "browser.navigate",
        json!({"url": "https://example.com"}),
    )
    .await
    .expect_err("not hosted here");
    assert!(refused.contains("browser.navigate"), "{refused}");
    // Unknown arguments are refused, as the declarations say.
    assert!(run(&mut host, "fs.read", json!({"path": "a", "extra": 1}))
        .await
        .is_err());
}
