//! The `qworkerd` binary runs dispatches side by side and stops one on a cancel (EXEC-002).
//!
//! The worker is the real executable over a real Unix socket and the commands are real processes; the test plays the
//! server's side of the protocol. What it proves:
//!
//! * a long command does not hold the channel: a dispatch sent after it is answered first, and the worker keeps
//!   heartbeating (so its lease is renewed) while the command runs;
//! * `--max-concurrent` bounds how many run at once, and a cancel of one still waiting ends it `cancelled` — it never runs;
//! * a cancel of one that is running stops it (`interrupted`: part of it may have happened), and it does not go on to do the
//!   rest of its work;
//! * a dispatch delivered again while it is still running is answered with that run's outcome (`replayed`) and does not run
//!   twice.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use quansio_machine::gateway::wire::{
    self, EnvelopeWire, FromWorker, LeaseWire, OutcomeWire, ToWorker,
};
use quansio_machine::gateway::{ActionEnvelope, DispatchRequest};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YCNC001";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9YCNC001";
const LEASE: &str = "lse_01J8Z3K6F1N8VQ2X5W9YCNC001";

struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

type Lines = tokio::io::Lines<BufReader<OwnedReadHalf>>;

/// A worker with a lease, and the server's ends of its channel.
async fn attached(
    max_concurrent: &str,
    root: &std::path::Path,
) -> (Reaped, Lines, OwnedWriteHalf, tempfile::TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("q")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("s.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let worker = Reaped(
        Command::new(env!("CARGO_BIN_EXE_qworkerd"))
            .arg("--socket")
            .arg(&socket)
            .args([
                "--tenant",
                TENANT,
                "--target",
                TARGET,
                "--heartbeat-secs",
                "1",
                "--max-concurrent",
                max_concurrent,
                "--workspace-root",
            ])
            .arg(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the binary starts"),
    );
    let (stream, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
        .await
        .expect("the worker dials in")
        .unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let _hello = lines.next_line().await.unwrap().unwrap();
    send(
        &mut writer,
        &ToWorker::Lease(LeaseWire {
            lease_id: LEASE.to_string(),
            target_id: TARGET.to_string(),
            generation: 1,
            expires_at: "2999-01-01T00:00:00Z".to_string(),
        }),
    )
    .await;
    (worker, lines, writer, dir)
}

fn exec(token: &str, command: &str) -> ToWorker {
    let envelope = ActionEnvelope::dispatch(&DispatchRequest {
        effect_id: &format!("eff_{token}"),
        dispatch_token: token,
        target_id: TARGET,
        lease_id: LEASE,
        generation: 1,
        run_id: "run_01J8Z3K6F1N8VQ2X5W9YCNC001",
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9YCNC001",
        tool: "terminal.exec",
        tool_version: 1,
        args_json: &json!({ "command": command }).to_string(),
        max_output_bytes: 65536,
        deadline_at: "2999-01-01T00:00:00Z",
    })
    .expect("a well-formed dispatch");
    ToWorker::Dispatch(Box::new(EnvelopeWire::from(&envelope)))
}

fn cancel(token: &str) -> ToWorker {
    ToWorker::Cancel {
        dispatch_token: token.to_string(),
    }
}

async fn send(writer: &mut OwnedWriteHalf, frame: &ToWorker) {
    writer
        .write_all(wire::encode(frame).unwrap().as_bytes())
        .await
        .unwrap();
}

/// What the worker sends next: a result as `(token, replayed, outcome)`, or `None` for a heartbeat.
async fn next(lines: &mut Lines) -> Option<(String, bool, OutcomeWire)> {
    let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
        .await
        .expect("the worker says something")
        .unwrap()
        .expect("the channel is open");
    match wire::decode(&line).unwrap() {
        FromWorker::Result {
            dispatch_token,
            replayed,
            outcome,
            ..
        } => Some((dispatch_token, replayed, outcome)),
        _ => None,
    }
}

async fn result(lines: &mut Lines) -> (String, bool, OutcomeWire) {
    loop {
        if let Some(result) = next(lines).await {
            return result;
        }
    }
}

#[tokio::test]
async fn a_long_command_does_not_hold_the_channel_and_the_worker_keeps_its_lease_alive() {
    let root = tempfile::tempdir().unwrap();
    let (_worker, mut lines, mut writer, _dir) = attached("4", root.path()).await;

    let slow = "dsp_01J8Z3K6F1N8VQ2X5W9YCNCS01";
    let quick = "dsp_01J8Z3K6F1N8VQ2X5W9YCNCQ01";
    let started = Instant::now();
    send(&mut writer, &exec(slow, "sleep 4; echo slow > slow.txt")).await;
    send(&mut writer, &exec(quick, "echo quick")).await;

    // The quick one is answered while the slow one runs, and heartbeats keep coming meanwhile.
    let mut order = Vec::new();
    let mut beats_before_slow = 0;
    while order.len() < 2 {
        match next(&mut lines).await {
            Some((token, _, outcome)) => {
                assert!(
                    matches!(outcome, OutcomeWire::Completed { .. }),
                    "{token}: {outcome:?}"
                );
                order.push((token, started.elapsed()));
            }
            None if order.iter().all(|(token, _)| token != slow) => beats_before_slow += 1,
            None => {}
        }
    }
    assert_eq!(
        order[0].0, quick,
        "the dispatch sent second was answered first"
    );
    assert!(
        order[0].1 < Duration::from_secs(3),
        "the quick dispatch waited for the slow one: {:?}",
        order[0].1
    );
    assert_eq!(order[1].0, slow);
    assert!(
        beats_before_slow >= 2,
        "the worker stopped heartbeating while a command ran ({beats_before_slow} heartbeats in 4 s)"
    );
    assert!(root.path().join("slow.txt").exists());
}

#[tokio::test]
async fn the_concurrency_is_bounded_and_a_cancelled_dispatch_that_was_waiting_never_runs() {
    let root = tempfile::tempdir().unwrap();
    let (_worker, mut lines, mut writer, _dir) = attached("1", root.path()).await;

    let first = "dsp_01J8Z3K6F1N8VQ2X5W9YCNCF01";
    let waiting = "dsp_01J8Z3K6F1N8VQ2X5W9YCNCW01";
    send(&mut writer, &exec(first, "sleep 2; echo one > one.txt")).await;
    send(&mut writer, &exec(waiting, "echo two > two.txt")).await;
    // With one turn, the second has not started; cancelling it ends it before it runs.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !root.path().join("two.txt").exists(),
        "a second dispatch ran past --max-concurrent 1"
    );
    send(&mut writer, &cancel(waiting)).await;
    let (token, replayed, outcome) = result(&mut lines).await;
    assert_eq!((token.as_str(), replayed), (waiting, false));
    assert_eq!(
        outcome,
        OutcomeWire::Cancelled,
        "nothing ran, so it is cancelled"
    );
    let (token, _, outcome) = result(&mut lines).await;
    assert_eq!(token, first);
    assert!(
        matches!(outcome, OutcomeWire::Completed { .. }),
        "{outcome:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(root.path().join("one.txt").exists());
    assert!(
        !root.path().join("two.txt").exists(),
        "the cancelled dispatch ran anyway"
    );

    // A cancel of a dispatch that has settled changes nothing: it is answered from the record.
    send(&mut writer, &cancel(first)).await;
    send(&mut writer, &exec(first, "sleep 2; echo one > one.txt")).await;
    let (token, replayed, outcome) = result(&mut lines).await;
    assert_eq!((token.as_str(), replayed), (first, true));
    assert!(
        matches!(outcome, OutcomeWire::Completed { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_running_dispatch_is_stopped_by_a_cancel_and_does_not_finish_its_work() {
    let root = tempfile::tempdir().unwrap();
    let (_worker, mut lines, mut writer, _dir) = attached("4", root.path()).await;

    let running = "dsp_01J8Z3K6F1N8VQ2X5W9YCNCR01";
    send(
        &mut writer,
        &exec(
            running,
            "echo started > started.txt; sleep 3; echo done > done.txt",
        ),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.path().join("started.txt").exists() {
        assert!(Instant::now() < deadline, "the command never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let cancelled_at = Instant::now();
    send(&mut writer, &cancel(running)).await;
    let (token, _, outcome) = result(&mut lines).await;
    assert_eq!(token, running);
    assert!(
        matches!(outcome, OutcomeWire::Interrupted { .. }),
        "a command stopped part-way is interrupted, not completed or cancelled: {outcome:?}"
    );
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(2),
        "the answer waited for the command: {:?}",
        cancelled_at.elapsed()
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !root.path().join("done.txt").exists(),
        "the cancelled command went on to finish its work"
    );
}

#[tokio::test]
async fn a_dispatch_delivered_again_while_it_runs_is_answered_from_that_run() {
    let root = tempfile::tempdir().unwrap();
    let (_worker, mut lines, mut writer, _dir) = attached("4", root.path()).await;

    let token = "dsp_01J8Z3K6F1N8VQ2X5W9YCNCD01";
    let command = "echo ran >> count.txt; sleep 1";
    send(&mut writer, &exec(token, command)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    send(&mut writer, &exec(token, command)).await;
    let mut answers = [result(&mut lines).await, result(&mut lines).await];
    answers.sort_by_key(|(_, replayed, _)| *replayed);
    assert_eq!(answers[0].0, token);
    assert_eq!(answers[1].0, token);
    assert_eq!(
        (answers[0].1, answers[1].1),
        (false, true),
        "one run and one replay"
    );
    assert_eq!(
        answers[0].2, answers[1].2,
        "both are answered with the one outcome"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("count.txt")).unwrap(),
        "ran\n",
        "the command ran twice"
    );
}
