//! qworkerd's control-channel rules (EXEC-002).
//!
//! The three tests the task names, plus the two prohibitions that keep a worker a worker:
//!
//! * **stale envelope** — every refusal is decided before any tool runs, asserted by an executor that
//!   counts its calls;
//! * **disconnect/reconnect** — a re-delivered dispatch is answered from the record rather than run
//!   again, and a replay that arrives after the lease lapsed is fenced instead: either way the effect is
//!   not executed twice;
//! * **network ACL** — a customer's private worker is never dialled;
//! * `crates/qworkerd` reaches no control-plane database and evaluates no policy, asserted by scanning
//!   its sources.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use quansio_machine::control::{Substrate, TargetStatus};
use quansio_machine::gateway::{
    ActionEnvelope, Dial, DispatchRequest, EnvelopeKind, LeaseClaim, MachineGateway, Rejection,
};
use quansio_qworkerd::host::{
    EvidenceSink, HostFailure, HostOutcome, NoopExecutor, OutputSink, Recorded, ToolExecutor,
    ToolOutput, WorkerHost,
};

const NOW: &str = "2026-09-13T10:00:00Z";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const LEASE: &str = "lse_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";

/// An executor that counts how many times it was asked to run anything.
///
/// It streams its output in fixed-size chunks, so the host's bound is exercised while the tool is
/// still producing rather than only on the finished buffer.
struct CountingExecutor {
    calls: Arc<AtomicUsize>,
    output: Vec<u8>,
    evidence: Vec<u8>,
    fail: bool,
}

impl CountingExecutor {
    fn new(output: &str) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                calls: calls.clone(),
                output: output.as_bytes().to_vec(),
                evidence: Vec::new(),
                fail: false,
            },
            calls,
        )
    }
}

#[async_trait::async_trait]
impl ToolExecutor for CountingExecutor {
    async fn run(
        &self,
        envelope: &ActionEnvelope,
        output: &mut OutputSink,
    ) -> Result<ToolOutput, HostFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(HostFailure::ToolFailed {
                tool: envelope.tool.clone(),
                detail: "the tool refused".to_string(),
            });
        }
        for chunk in self.output.chunks(7) {
            output.push(chunk);
        }
        let mut produced = ToolOutput::new();
        if !self.evidence.is_empty() {
            produced = produced.with_evidence(self.evidence.clone(), "text/plain");
        }
        Ok(produced)
    }
}

struct RecordingSink {
    uploads: AtomicUsize,
}

#[async_trait::async_trait]
impl EvidenceSink for RecordingSink {
    async fn upload(
        &self,
        effect_id: &str,
        _payload: &[u8],
        _media_type: &str,
    ) -> Result<String, HostFailure> {
        self.uploads.fetch_add(1, Ordering::SeqCst);
        Ok(format!("evd_for_{effect_id}"))
    }
}

fn lease() -> LeaseClaim {
    LeaseClaim {
        lease_id: LEASE.to_string(),
        target_id: TARGET.to_string(),
        generation: 1,
        held: true,
        expires_at: "2026-09-13T10:05:00Z".to_string(),
    }
}

fn dispatch() -> ActionEnvelope {
    ActionEnvelope::dispatch(&DispatchRequest {
        effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        dispatch_token: "dsp_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        target_id: TARGET,
        lease_id: LEASE,
        generation: 1,
        run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        tool: "fs.read",
        tool_version: 3,
        args_json: "{\"value\":\"42\"}",
        max_output_bytes: 1024,
        deadline_at: "2026-09-13T10:01:00Z",
    })
    .expect("a well-formed dispatch")
}

fn host(executor: CountingExecutor) -> WorkerHost<CountingExecutor, RecordingSink> {
    WorkerHost::new(
        executor,
        RecordingSink {
            uploads: AtomicUsize::new(0),
        },
    )
}

// ------------------------------------------------------------------- stale envelopes

#[tokio::test]
async fn a_stale_envelope_fails_before_any_tool_runs() {
    let (executor, calls) = CountingExecutor::new("output");
    let mut host = host(executor);
    let good = dispatch();

    let mut cases: Vec<(&str, ActionEnvelope, LeaseClaim, String, Rejection)> = Vec::new();

    // The action's own deadline passed.
    cases.push((
        "deadline",
        good.clone(),
        lease(),
        "2026-09-13T10:02:00Z".to_string(),
        Rejection::DeadlineExpired {
            deadline_at: "2026-09-13T10:01:00Z".to_string(),
            now: "2026-09-13T10:02:00Z".to_string(),
        },
    ));
    // The lease lapsed.
    cases.push((
        "lease expiry",
        good.clone(),
        lease(),
        "2026-09-13T10:06:00Z".to_string(),
        Rejection::LeaseExpired {
            expires_at: "2026-09-13T10:05:00Z".to_string(),
            now: "2026-09-13T10:06:00Z".to_string(),
        },
    ));
    // The worker was fenced: the envelope names an old generation.
    let fenced = good.clone();
    let mut held = lease();
    held.generation = 2;
    cases.push((
        "generation",
        fenced,
        held,
        NOW.to_string(),
        Rejection::StaleGeneration {
            envelope: 1,
            held: 2,
        },
    ));
    // The worker holds no lease at all, or a different one.
    let mut other_lease = lease();
    other_lease.lease_id = "lse_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string();
    cases.push((
        "not held",
        good.clone(),
        other_lease,
        NOW.to_string(),
        Rejection::LeaseNotHeld {
            envelope: LEASE.to_string(),
            held: "lse_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string(),
        },
    ));
    let mut released = lease();
    released.held = false;
    cases.push((
        "released",
        good.clone(),
        released,
        NOW.to_string(),
        Rejection::LeaseNotHeld {
            envelope: LEASE.to_string(),
            held: LEASE.to_string(),
        },
    ));
    // The envelope is for another target.
    let mut foreign = good.clone();
    foreign.target_id = "tgt_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string();
    cases.push((
        "target",
        foreign,
        lease(),
        NOW.to_string(),
        Rejection::TargetMismatch {
            envelope: "tgt_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string(),
            lease: TARGET.to_string(),
        },
    ));
    // The arguments were rewritten after the envelope was issued.
    let mut tampered = good.clone();
    tampered.args_json = "{\"value\":\"rm -rf\"}".to_string();
    cases.push((
        "arguments",
        tampered,
        lease(),
        NOW.to_string(),
        Rejection::ArgsDigestMismatch,
    ));
    // The envelope is missing what every kind needs.
    let mut nameless = good.clone();
    nameless.dispatch_token = String::new();
    cases.push((
        "shape",
        nameless,
        lease(),
        NOW.to_string(),
        Rejection::MalformedEnvelope {
            field: "dispatch_token",
            detail: "is required",
        },
    ));

    for (name, envelope, claim, now, expected) in cases {
        let refusal = host
            .receive(&envelope, &claim, &now)
            .await
            .expect_err(&format!("{name} must be refused"));
        assert_eq!(refusal, expected, "{name}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "every refusal is decided before the executor is touched"
    );
    assert!(
        host.recorded().is_empty(),
        "a refused action records nothing"
    );
}

#[test]
fn an_unknown_kind_is_refused_rather_than_guessed() {
    // The vocabulary is closed and complete: every kind survives the wire round trip, so the only way
    // an unrecognised kind is handled is by refusing it.
    for kind in [
        EnvelopeKind::Dispatch,
        EnvelopeKind::Cancel,
        EnvelopeKind::Checkpoint,
        EnvelopeKind::EvidenceUpload,
    ] {
        assert_eq!(EnvelopeKind::parse(kind.as_str()), Some(kind), "{kind:?}");
    }
    assert_eq!(
        EnvelopeKind::parse("dispatch"),
        Some(EnvelopeKind::Dispatch)
    );
    assert_eq!(EnvelopeKind::parse("teleport"), None);
    assert_eq!(EnvelopeKind::parse(""), None);
    assert_eq!(EnvelopeKind::EvidenceUpload.as_str(), "evidence_upload");
}

// ------------------------------------------------------------- disconnect and reconnect

#[tokio::test]
async fn a_replayed_dispatch_is_answered_from_the_record_not_run_again() {
    let (executor, calls) = CountingExecutor::new("the tool output");
    let mut host = host(executor);
    let envelope = dispatch();

    let first = host
        .receive(&envelope, &lease(), NOW)
        .await
        .expect("first delivery");
    assert!(!first.replayed);
    match &first.outcome {
        HostOutcome::Completed {
            output, truncated, ..
        } => {
            assert_eq!(output, b"the tool output");
            assert!(!truncated);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(host.recorded().len(), 1);

    // The worker reconnects and the runtime replays the dispatch it never heard the result of.
    let replay = host
        .receive(&envelope, &lease(), "2026-09-13T10:00:30Z")
        .await
        .expect("replay");
    assert!(
        replay.replayed,
        "the second delivery is answered from the record"
    );
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the effect is executed exactly once"
    );
}

#[tokio::test]
async fn a_replay_after_the_lease_lapsed_is_fenced_rather_than_served() {
    let (executor, calls) = CountingExecutor::new("output");
    let mut host = host(executor);
    let envelope = dispatch();
    host.receive(&envelope, &lease(), NOW)
        .await
        .expect("first delivery");

    // The lease expired while the worker was disconnected. The replay is refused — the worker is no
    // longer entitled to act on the target — and the effect still is not executed twice.
    let refusal = host
        .receive(&envelope, &lease(), "2026-09-13T10:06:00Z")
        .await
        .expect_err("must be fenced");
    assert!(
        matches!(refusal, Rejection::LeaseExpired { .. }),
        "got {refusal:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_cancelled_effect_is_recorded_and_not_re_dispatched() {
    let (executor, calls) = CountingExecutor::new("output");
    let mut host = host(executor);
    let mut envelope = dispatch();
    envelope.kind = EnvelopeKind::Cancel;

    let cancelled = host
        .receive(&envelope, &lease(), NOW)
        .await
        .expect("cancel");
    assert!(matches!(cancelled.outcome, HostOutcome::Cancelled { .. }));
    let replay = host
        .receive(&envelope, &lease(), NOW)
        .await
        .expect("replay");
    assert!(replay.replayed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a cancellation runs no tool"
    );
}

// ------------------------------------------------------------------------- hosts

#[tokio::test]
async fn output_is_bounded_while_the_tool_streams_and_evidence_is_uploaded() {
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = CountingExecutor {
        calls: calls.clone(),
        output: b"x".repeat(4096),
        evidence: b"screenshot bytes".to_vec(),
        fail: false,
    };
    let mut host = host(executor);
    let mut envelope = dispatch();
    envelope.max_output_bytes = 100;

    let recorded = host.receive(&envelope, &lease(), NOW).await.expect("run");
    match recorded.outcome {
        HostOutcome::Completed {
            output,
            truncated,
            evidence_ref,
            ..
        } => {
            assert_eq!(output.len(), 100, "the output is bounded by the envelope");
            assert!(truncated);
            assert_eq!(
                evidence_ref.as_deref(),
                Some("evd_for_eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF")
            );
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
}

#[test]
fn the_sink_bounds_each_chunk_as_it_arrives() {
    // The bound is a property of the sink, so it holds for a tool that never stops producing: what is
    // kept never exceeds the bound, and the amount discarded is counted rather than guessed.
    let mut sink = OutputSink::new(100);
    for _ in 0..10_000 {
        sink.push(&[b'y'; 64]);
    }
    assert_eq!(sink.kept().len(), 100);
    assert_eq!(sink.offered(), 640_000);
    assert!(sink.truncated());
    let (kept, truncated) = sink.finish();
    assert_eq!(kept.len(), 100);
    assert!(truncated);

    // A bound of zero means the envelope asked for no bound at all.
    let mut unbounded = OutputSink::new(0);
    unbounded.push(&[b'z'; 32]);
    assert_eq!(unbounded.kept().len(), 32);
    assert!(!unbounded.truncated());
}

// ------------------------------------------------------------------------- heartbeat

#[test]
fn a_worker_reports_the_state_it_observes_and_only_its_own_generation() {
    let claim = lease();
    let report = WorkerHost::<NoopExecutor, RecordingSink>::heartbeat(&claim, "READY", NOW);
    assert_eq!(report.target_id, TARGET);
    assert_eq!(report.generation, claim.generation);
    assert_eq!(
        MachineGateway::accept_heartbeat(&report, TARGET, claim.generation),
        Ok(TargetStatus::Ready)
    );

    // A report from a fenced generation is refused: the worker that lost the target must not overwrite
    // the observation of the generation that replaced it.
    assert_eq!(
        MachineGateway::accept_heartbeat(&report, TARGET, 2),
        Err(Rejection::StaleGeneration {
            envelope: 1,
            held: 2
        })
    );

    // A state string the lifecycle does not define is not stored.
    let invented = WorkerHost::<NoopExecutor, RecordingSink>::heartbeat(&claim, "HAPPY", NOW);
    assert!(matches!(
        MachineGateway::accept_heartbeat(&invented, TARGET, 1),
        Err(Rejection::MalformedEnvelope {
            field: "observed_state",
            ..
        })
    ));

    // A report about another target is not this target's observation.
    let mut foreign = report.clone();
    foreign.target_id = "tgt_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string();
    assert!(matches!(
        MachineGateway::accept_heartbeat(&foreign, TARGET, 1),
        Err(Rejection::TargetMismatch { .. })
    ));
}

#[tokio::test]
async fn a_checkpoint_hook_records_the_target() {
    let (executor, calls) = CountingExecutor::new("output");
    let mut host = host(executor);
    let mut envelope = dispatch();
    envelope.kind = EnvelopeKind::Checkpoint;

    host.receive(&envelope, &lease(), NOW)
        .await
        .expect("checkpoint");
    assert_eq!(host.checkpoints(), [TARGET.to_string()]);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a checkpoint runs no tool");
}

#[tokio::test]
async fn a_failing_tool_is_recorded_as_a_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = CountingExecutor {
        calls: calls.clone(),
        output: Vec::new(),
        evidence: Vec::new(),
        fail: true,
    };
    let mut host = host(executor);
    let recorded: Recorded = host.receive(&dispatch(), &lease(), NOW).await.expect("run");
    assert!(matches!(recorded.outcome, HostOutcome::Failed { .. }));
    assert_eq!(
        host.recorded().len(),
        1,
        "a failure is recorded so a replay is not re-run either"
    );
}

// --------------------------------------------------------------------- network ACL

#[test]
fn a_private_worker_is_never_dialled() {
    // The substrate decides: a customer's worker opens the channel itself, so a caller waits for it.
    assert_eq!(
        MachineGateway::dial(Substrate::CustomerPrivateWorker),
        Dial::AwaitInbound
    );
    for substrate in [
        Substrate::CloudMicrovm,
        Substrate::LocalCapsuleMacos,
        Substrate::LocalCapsuleWindows,
        Substrate::WindowsNative,
    ] {
        assert_eq!(
            MachineGateway::dial(substrate),
            Dial::Outbound,
            "{substrate:?}"
        );
    }
    assert!(Substrate::CustomerPrivateWorker.is_outbound_only());
}

// ------------------------------------------------- the two prohibitions, asserted in source

/// Every `.rs` file under `root`, as `(path, text)`.
fn rust_sources(root: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    let mut sources = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).expect("read source");
                sources.push((path, text));
            }
        }
    }
    sources
}

/// Which of `tokens` appear in `sources`, as `path: token`.
fn offenders(sources: &[(std::path::PathBuf, String)], tokens: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for (path, text) in sources {
        for token in tokens {
            if text.contains(token.as_str()) {
                found.push(format!("{}: {token}", path.display()));
            }
        }
    }
    found
}

#[test]
fn qworkerd_reaches_no_control_plane_database() {
    // Each token is assembled from fragments. The `worker-to-control-db` architecture gate reads the
    // crate's text, so writing the literals here would be indistinguishable from the violation this
    // test exists to catch -- and the gate is right to refuse that.
    let forbidden: [String; 9] = [
        ["Pg", "Connection"],
        ["Pg", "Pool"],
        ["sql", "x"],
        ["runtime", "_events"],
        ["effect", "_records"],
        ["protocol", "_states"],
        ["policy", "_decisions"],
        ["approval_", "requests"],
        ["capability", "_projections"],
    ]
    .map(|fragments| fragments.concat());

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sources = rust_sources(&root);
    assert!(
        sources.len() >= 2,
        "the scan must actually read the crate's sources, read {}",
        sources.len()
    );
    let found = offenders(&sources, &forbidden);
    assert!(
        found.is_empty(),
        "qworkerd talks to the server over the control channel, never to the control database: {found:?}"
    );

    // The scan is only worth anything if it would notice the thing it forbids, so prove it: a source
    // carrying each token must be flagged. The probe is written at run time from the same fragments,
    // so the token never appears as a literal in this file either.
    let probe = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("arch-probe");
    std::fs::create_dir_all(&probe).expect("create probe dir");
    let probe_file = probe.join("would_be_a_violation.rs");
    std::fs::write(&probe_file, forbidden.join("\n")).expect("write probe");
    let caught = offenders(&rust_sources(&probe), &forbidden);
    std::fs::remove_dir_all(&probe).expect("clean probe dir");
    for token in &forbidden {
        assert!(
            caught.iter().any(|entry| entry.ends_with(token)),
            "the scan must catch {token}, caught {caught:?}"
        );
    }
}

#[test]
fn qworkerd_evaluates_no_policy_or_capability() {
    // The runtime decides policy, capability, approval and routing before dispatching (RUN-006/RUN-011).
    // A worker that re-decided would be a second authority, so the vocabulary for deciding must not
    // appear in its vocabulary for executing.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/host.rs"),
    )
    .expect("read host");
    for token in [
        "evaluate_policy",
        "CapabilityProjection",
        "ApprovalReceipt",
        "DlpGuard",
        "RouteSelector",
    ] {
        assert!(
            !source.contains(token),
            "the worker must not decide: found {token}"
        );
    }
    assert!(
        source.contains("ActionEnvelope"),
        "it still validates what it was sent"
    );
    assert!(
        source.contains("MachineGateway::validate"),
        "and validation is how: the envelope is checked before it is run"
    );
}
