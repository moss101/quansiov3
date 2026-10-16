//! The `qworkerd` binary as a separate process, speaking the worker wire protocol over a real Unix socket
//! (EXEC-002 composition).
//!
//! The test plays the server's side of the protocol (the hub's own behaviour is proven against PostgreSQL in
//! `crates/server/tests/worker_host.rs`); the worker is the real executable. What this proves is the process
//! boundary: it dials in, introduces itself, obeys only the lease it was told, runs a dispatch exactly once even
//! when the same dispatch arrives again after a reconnect, and never listens.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use quansio_core::Digest;
use quansio_machine::gateway::wire::{
    self, EnvelopeWire, FromWorker, LeaseWire, OutcomeWire, ToWorker,
};
use quansio_machine::gateway::{ActionEnvelope, DispatchRequest};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{unix::OwnedWriteHalf, UnixListener, UnixStream};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YBIN001";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9YBIN001";
const LEASE: &str = "lse_01J8Z3K6F1N8VQ2X5W9YBIN001";

struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(socket: &std::path::Path, root: &std::path::Path) -> Reaped {
    Reaped(
        Command::new(env!("CARGO_BIN_EXE_qworkerd"))
            .args(["--socket"])
            .arg(socket)
            .args([
                "--tenant",
                TENANT,
                "--target",
                TARGET,
                "--heartbeat-secs",
                "1",
                "--workspace-root",
            ])
            .arg(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the binary starts"),
    )
}

fn dispatch(token: &str, generation: i64, tool: &str, args: &Value) -> ToWorker {
    let envelope = ActionEnvelope::dispatch(&DispatchRequest {
        effect_id: &format!("eff_{token}"),
        dispatch_token: token,
        target_id: TARGET,
        lease_id: LEASE,
        generation,
        run_id: "run_01J8Z3K6F1N8VQ2X5W9YBIN001",
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9YBIN001",
        tool,
        tool_version: 1,
        args_json: &args.to_string(),
        max_output_bytes: 65536,
        deadline_at: "2999-01-01T00:00:00Z",
    })
    .expect("a well-formed dispatch");
    ToWorker::Dispatch(Box::new(EnvelopeWire::from(&envelope)))
}

fn lease(generation: i64) -> ToWorker {
    ToWorker::Lease(LeaseWire {
        lease_id: LEASE.to_string(),
        target_id: TARGET.to_string(),
        generation,
        expires_at: "2999-01-01T00:00:00Z".to_string(),
    })
}

async fn send(writer: &mut OwnedWriteHalf, frame: &ToWorker) {
    writer
        .write_all(wire::encode(frame).unwrap().as_bytes())
        .await
        .unwrap();
}

async fn accept(
    listener: &UnixListener,
) -> (
    tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    OwnedWriteHalf,
) {
    let (stream, _): (UnixStream, _) =
        tokio::time::timeout(Duration::from_secs(20), listener.accept())
            .await
            .expect("the worker dials in")
            .unwrap();
    let (reader, writer) = stream.into_split();
    (BufReader::new(reader).lines(), writer)
}

/// The next result, skipping heartbeats.
async fn result(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> (String, bool, OutcomeWire) {
    loop {
        let line = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
            .await
            .expect("an answer arrives")
            .unwrap()
            .expect("the channel is open");
        if let FromWorker::Result {
            dispatch_token,
            replayed,
            outcome,
            ..
        } = wire::decode(&line).unwrap()
        {
            return (dispatch_token, replayed, outcome);
        }
    }
}

#[tokio::test]
async fn the_binary_dials_in_obeys_only_its_lease_and_runs_a_dispatch_once_across_a_reconnect() {
    let dir = tempfile::Builder::new()
        .prefix("q")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("s.sock");
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let _worker = start(&socket, root.path());

    let (mut lines, mut writer) = accept(&listener).await;
    let hello: FromWorker = wire::decode(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        hello,
        FromWorker::Hello {
            tenant_id: TENANT.to_string(),
            target_id: TARGET.to_string()
        }
    );

    // No lease yet: the worker holds nothing, so it refuses, and nothing runs.
    let content = "once\n";
    let write = json!({"path": "f.txt", "content": content, "content_digest": Digest::of(content.as_bytes()).as_str()});
    send(
        &mut writer,
        &dispatch("dsp_01J8Z3K6F1N8VQ2X5W9YBIN0A1", 1, "fs.write", &write),
    )
    .await;
    let (_, _, outcome) = result(&mut lines).await;
    assert!(
        matches!(outcome, OutcomeWire::Rejected { .. }),
        "{outcome:?}"
    );
    assert!(!root.path().join("f.txt").exists());

    // A lease at generation 2; an envelope at generation 1 is stale and is refused, nothing runs.
    send(&mut writer, &lease(2)).await;
    send(
        &mut writer,
        &dispatch("dsp_01J8Z3K6F1N8VQ2X5W9YBIN0A2", 1, "fs.write", &write),
    )
    .await;
    match result(&mut lines).await.2 {
        OutcomeWire::Rejected { reason } => assert!(reason.contains("generation"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(!root.path().join("f.txt").exists());

    // At the generation the worker holds it runs, for real, on the worker's disk.
    let token = "dsp_01J8Z3K6F1N8VQ2X5W9YBIN0A3";
    send(&mut writer, &dispatch(token, 2, "fs.write", &write)).await;
    let (answered, replayed, outcome) = result(&mut lines).await;
    assert_eq!((answered.as_str(), replayed), (token, false));
    assert!(
        matches!(outcome, OutcomeWire::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("f.txt")).unwrap(),
        content
    );

    // It heartbeats while it holds the lease.
    let mut beat = None;
    for _ in 0..40 {
        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let FromWorker::Heartbeat {
            generation,
            observed_state,
            ..
        } = wire::decode(&line).unwrap()
        {
            beat = Some((generation, observed_state));
            break;
        }
    }
    assert_eq!(beat, Some((2, "READY".to_string())));

    // The connection drops. The worker reconnects (it never listens) and the same dispatch is answered from its
    // record: the file is not written a second time.
    std::fs::write(root.path().join("f.txt"), "edited since").unwrap();
    drop(writer);
    drop(lines);
    let (mut lines, mut writer) = accept(&listener).await;
    let _hello = lines.next_line().await.unwrap().unwrap();
    send(&mut writer, &lease(2)).await;
    send(&mut writer, &dispatch(token, 2, "fs.write", &write)).await;
    let (answered, replayed, outcome) = result(&mut lines).await;
    assert_eq!(answered, token);
    assert!(replayed, "a re-delivered dispatch is a replay");
    assert!(matches!(outcome, OutcomeWire::Completed { .. }));
    assert_eq!(
        std::fs::read_to_string(root.path().join("f.txt")).unwrap(),
        "edited since",
        "the tool did not run again"
    );
}

#[tokio::test]
async fn the_binary_refuses_to_start_without_a_usable_workspace_root_or_with_an_unknown_flag() {
    let dir = tempfile::Builder::new()
        .prefix("q")
        .tempdir_in("/tmp")
        .unwrap();
    let missing = dir.path().join("does-not-exist");
    let status = Command::new(env!("CARGO_BIN_EXE_qworkerd"))
        .args([
            "--socket",
            "/tmp/none.sock",
            "--tenant",
            TENANT,
            "--target",
            TARGET,
            "--workspace-root",
        ])
        .arg(&missing)
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(2),
        "a root that does not exist is refused, never created"
    );
    assert!(!missing.exists());

    let status = Command::new(env!("CARGO_BIN_EXE_qworkerd"))
        .arg("--unlock-everything")
        .arg("1")
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[tokio::test]
async fn the_browser_flags_are_refused_without_a_browser_and_a_state_dir_inside_the_workspace_is_refused(
) {
    let dir = tempfile::Builder::new()
        .prefix("q")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("state")).unwrap();
    let base = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_qworkerd"))
            .args([
                "--socket",
                "/tmp/none.sock",
                "--tenant",
                TENANT,
                "--target",
                TARGET,
                "--workspace-root",
            ])
            .arg(&root)
            .args(extra)
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    };
    // The flags mean nothing without a browser: they are refused rather than ignored.
    for extra in [
        &["--allow-private-network"][..],
        &["--state-dir", "/tmp/x"][..],
    ] {
        let out = base(extra);
        assert_eq!(out.status.code(), Some(2), "{extra:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("only used with --chrome"),
            "{extra:?}"
        );
    }
    // A browser needs a state directory, and not one the file tools can read.
    let chrome = std::env::current_exe().unwrap(); // any existing file: the check that matters comes first
    let out = base(&["--chrome", chrome.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs --state-dir"));
    let inside = root.join("state");
    let out = base(&[
        "--chrome",
        chrome.to_str().unwrap(),
        "--state-dir",
        inside.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("must not be inside"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
