//! Worker tools through the server's real hub, lease and routed host (EXEC-002 composition over EXEC-006).
//!
//! Everything between the dispatch and the disk is real: PostgreSQL leases taken and renewed through
//! `MachineControl`, a Unix socket the worker dials, the worker's own `WorkerHost` fencing each envelope,
//! and the executor writing real files and running a real shell. The only thing played by the test is the
//! turn loop above the host.
//!
//! * **fails closed**: with no worker attached, a worker tool is refused with `WORKER_UNAVAILABLE`; it is
//!   never answered by a stub;
//! * **runs for real**: `fs.write`, `fs.read` and `terminal.exec` land on the worker's disk, and the lease
//!   the hub holds is live in the control plane while the worker is attached;
//! * **only a target that dials in**: a target on another substrate, or one not `READY`, is refused a worker;
//! * **one worker per target**: a second worker for a leased target is refused;
//! * **a lost worker releases its lease**, and the next dispatch is refused;
//! * **an unanswered dispatch is unknown, never retried**;
//! * **routing**: a tool the worker does not host goes to the host it wraps.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quansio_core::{CanonicalId, Digest, Prefix, UlidGenerator};
use quansio_machine::control::{
    MachineControl, NewTarget, Substrate, TargetClass, TargetFence, TargetStatus,
};
use quansio_machine::gateway::wire::{self, FromWorker};
use quansio_qworkerd::daemon::{serve, UnattachedEvidence};
use quansio_qworkerd::executor::LocalToolExecutor;
use quansio_qworkerd::host::WorkerHost;
use quansio_qworkerd::tools::{RootScope, Sandbox};
use quansio_server::runtime::turn_loop::{HostDispatch, HostFailure, HostOutcome, ToolHostPort};
use quansio_server::worker::{HubConfig, WorkerHub, WorkerRoutedHost, WorkerToolHost};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::io::AsyncWriteExt;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YEXC002";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YEXC002";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YEXC002";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YEXC002";

struct World {
    name: String,
    pool: PgPool,
    hub: WorkerHub,
    socket: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn id(prefix: Prefix) -> String {
    CanonicalId::generate(prefix, &mut UlidGenerator::new()).to_string()
}

async fn world(label: &str) -> Option<World> {
    world_with(label, HubConfig::default()).await
}

async fn world_with(label: &str, config: HubConfig) -> Option<World> {
    let name = admin_url().map(|_| scratch_name(label))?;
    let pool = fresh_database(&name).await?;
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    let dir = tempfile::Builder::new()
        .prefix("w")
        .tempdir_in("/tmp")
        .expect("tempdir");
    let socket = dir.path().join("w.sock");
    let hub = WorkerHub::new(pool.clone(), config);
    hub.listen(&socket).expect("listen");
    Some(World {
        name,
        pool,
        hub,
        socket,
        _dir: dir,
    })
}

async fn register(pool: &PgPool, substrate: Substrate, ready: bool) -> String {
    let target = id(Prefix::ExecutionTarget);
    let mut tx = pool.begin().await.unwrap();
    MachineControl::register(
        &mut tx,
        TENANT,
        &NewTarget {
            id: target.clone(),
            workspace_id: WORKSPACE.to_string(),
            class: TargetClass::PersistentWorkspaceComputer,
            substrate,
            image_digest: None,
            desired_state: Some("READY".to_string()),
        },
    )
    .await
    .unwrap();
    if ready {
        for to in [TargetStatus::Provisioning, TargetStatus::Ready] {
            MachineControl::transition(
                &mut tx,
                TENANT,
                &target,
                &TargetFence { generation: 1 },
                to,
            )
            .await
            .unwrap();
        }
    }
    tx.commit().await.unwrap();
    target
}

/// A real worker: the daemon's own session loop over a real connection to the hub.
fn attach_worker(
    world: &World,
    target: &str,
    root: &std::path::Path,
) -> tokio::task::JoinHandle<()> {
    attach_worker_beating(world, target, root, Duration::from_secs(3600))
}

fn attach_worker_beating(
    world: &World,
    target: &str,
    root: &std::path::Path,
    beat: Duration,
) -> tokio::task::JoinHandle<()> {
    let (socket, target, root) = (world.socket.clone(), target.to_string(), root.to_path_buf());
    tokio::spawn(async move {
        let sandbox = Sandbox::empty()
            .authorize(RootScope::Workspace, &root)
            .unwrap();
        let mut host = WorkerHost::new(LocalToolExecutor::new(sandbox, TENANT), UnattachedEvidence);
        let stream = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("connect");
        let (reader, writer) = stream.into_split();
        let _ = serve(&mut host, TENANT, &target, beat, reader, writer).await;
    })
}

async fn attached(world: &World, count: usize) {
    for _ in 0..100 {
        if world.hub.links().len() == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {count} attached workers, saw {:?}",
        world.hub.links()
    );
}

#[derive(Default)]
struct Recording(AtomicUsize);

#[async_trait]
impl ToolHostPort for Recording {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(HostOutcome::Success {
            output: json!({ "rest": dispatch.tool }),
            evidence_ids: vec![],
            remote_ref: None,
        })
    }
}

fn dispatch(tool: &str, args: Value, timeout_ms: u64) -> HostDispatch {
    let suffix = id(Prefix::Run);
    HostDispatch {
        tool_call_id: id(Prefix::ToolCall),
        tool: tool.to_string(),
        version: 1,
        args,
        effect_id: id(Prefix::EffectRecord),
        dispatch_token: format!("dsp_{}", suffix.trim_start_matches("run_")),
        run_id: id(Prefix::Run),
        step_id: id(Prefix::Step),
        workspace_id: WORKSPACE.to_string(),
        tenant_id: TENANT.to_string(),
        acting_user_id: Some(USER.to_string()),
        max_output_bytes: 65536,
        timeout_ms,
    }
}

fn routed(world: &World, rest: Arc<dyn ToolHostPort>) -> WorkerRoutedHost {
    WorkerRoutedHost::new(Arc::new(WorkerToolHost::new(world.hub.clone())), rest)
}

fn success(outcome: Result<HostOutcome, HostFailure>) -> Value {
    match outcome {
        Ok(HostOutcome::Success { output, .. }) => output,
        other => panic!("expected success, got {other:?}"),
    }
}

async fn lease_is_live(pool: &PgPool, target: &str) -> bool {
    let mut tx = pool.begin().await.unwrap();
    let held = MachineControl::load(&mut tx, TENANT, target)
        .await
        .unwrap()
        .lease_id;
    let Some(lease) = held else { return false };
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    MachineControl::validate_lease(&mut tx, TENANT, &lease, 1, &now)
        .await
        .is_ok()
}

#[tokio::test]
async fn with_no_worker_a_worker_tool_is_refused_and_never_answered_by_a_stub() {
    let Some(world) = world("wk_none").await else {
        return blocked_marker();
    };
    let rest = Arc::new(Recording::default());
    let host = routed(&world, rest.clone());
    let refused = host
        .execute(dispatch(
            "terminal.exec",
            json!({"command": "echo hi"}),
            5000,
        ))
        .await;
    match refused {
        Err(HostFailure::Refused { detail }) => {
            assert!(detail.contains("WORKER_UNAVAILABLE"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        rest.0.load(Ordering::SeqCst),
        0,
        "a worker tool never falls through to the wrapped host"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn tools_run_on_the_worker_under_a_live_lease_and_the_disk_shows_it() {
    let Some(world) = world("wk_run").await else {
        return blocked_marker();
    };
    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    let root = tempfile::tempdir().unwrap();
    let _worker = attach_worker(&world, &target, root.path());
    attached(&world, 1).await;
    assert!(
        lease_is_live(&world.pool, &target).await,
        "the hub holds a live lease in the control plane"
    );
    let snapshot = &world.hub.links()[0];
    assert_eq!(
        (snapshot.target_id.as_str(), snapshot.workspace_id.as_str()),
        (target.as_str(), WORKSPACE)
    );

    let rest = Arc::new(Recording::default());
    let host = routed(&world, rest.clone());
    let content = "written by the agent\n";
    let written = success(
        host.execute(dispatch(
            "fs.write",
            json!({"path": "out/a.txt", "content": content, "content_digest": Digest::of(content.as_bytes()).as_str()}),
            10_000,
        ))
        .await,
    );
    assert_eq!(written["bytes_written"], content.len());
    assert_eq!(
        std::fs::read_to_string(root.path().join("out/a.txt")).unwrap(),
        content
    );

    let read = success(
        host.execute(dispatch("fs.read", json!({"path": "out/a.txt"}), 10_000))
            .await,
    );
    assert_eq!(read["content"], content);

    let exec = success(
        host.execute(dispatch(
            "terminal.exec",
            json!({"command": "wc -c < out/a.txt"}),
            10_000,
        ))
        .await,
    );
    assert_eq!(exec["exit_code"], 0);
    assert_eq!(
        exec["output"].as_str().unwrap().trim(),
        content.len().to_string()
    );

    // A tool the worker does not host goes to the wrapped host.
    assert_eq!(
        success(host.execute(dispatch("web.fetch", json!({}), 1000)).await)["rest"],
        "web.fetch"
    );
    assert_eq!(rest.0.load(Ordering::SeqCst), 1);
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_path_outside_the_worker_root_is_a_failed_result_not_a_read() {
    let Some(world) = world("wk_esc").await else {
        return blocked_marker();
    };
    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    let root = tempfile::tempdir().unwrap();
    let _worker = attach_worker(&world, &target, root.path());
    attached(&world, 1).await;
    let host = routed(&world, Arc::new(Recording::default()));
    match host
        .execute(dispatch(
            "fs.read",
            json!({"path": "../../etc/passwd"}),
            5000,
        ))
        .await
    {
        Ok(HostOutcome::Failure {
            retryable, error, ..
        }) => {
            assert!(!retryable);
            assert_eq!(error["code"], "TOOL_FAILED");
        }
        other => panic!("{other:?}"),
    }
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn only_a_ready_target_that_dials_in_is_given_a_worker_and_only_one_worker_per_target() {
    let Some(world) = world("wk_gate").await else {
        return blocked_marker();
    };
    let root = tempfile::tempdir().unwrap();
    let microvm = register(&world.pool, Substrate::CloudMicrovm, true).await;
    let not_ready = register(&world.pool, Substrate::CustomerPrivateWorker, false).await;
    let _a = attach_worker(&world, &microvm, root.path());
    let _b = attach_worker(&world, &not_ready, root.path());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        world.hub.links().is_empty(),
        "neither target may be served by a worker: {:?}",
        world.hub.links()
    );

    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    let _first = attach_worker(&world, &target, root.path());
    attached(&world, 1).await;
    let _second = attach_worker(&world, &target, root.path());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        world.hub.links().len(),
        1,
        "a leased target takes no second worker"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_lost_worker_releases_its_lease_and_the_next_dispatch_is_refused() {
    let Some(world) = world("wk_lost").await else {
        return blocked_marker();
    };
    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    let root = tempfile::tempdir().unwrap();
    let worker = attach_worker(&world, &target, root.path());
    attached(&world, 1).await;
    assert!(lease_is_live(&world.pool, &target).await);

    worker.abort();
    for _ in 0..100 {
        if world.hub.links().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(world.hub.links().is_empty());
    assert!(
        !lease_is_live(&world.pool, &target).await,
        "the lease is released with the worker"
    );
    let host = routed(&world, Arc::new(Recording::default()));
    assert!(matches!(
        host.execute(dispatch("fs.read", json!({"path": "a"}), 2000))
            .await,
        Err(HostFailure::Refused { .. })
    ));
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_dispatch_the_worker_never_answers_is_unknown_and_is_not_sent_twice() {
    let Some(world) = world("wk_unk").await else {
        return blocked_marker();
    };
    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    // A worker that says hello and then reads without ever answering.
    let socket = world.socket.clone();
    let target_id = target.clone();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    let _silent = tokio::spawn(async move {
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let hello = wire::encode(&FromWorker::Hello {
            tenant_id: TENANT.to_string(),
            target_id,
        })
        .unwrap();
        writer.write_all(hello.as_bytes()).await.unwrap();
        let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(reader));
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("\"dispatch\"") {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    attached(&world, 1).await;
    let host = routed(&world, Arc::new(Recording::default()));
    let outcome = host
        .execute(dispatch(
            "terminal.exec",
            json!({"command": "touch x"}),
            300,
        ))
        .await;
    match outcome {
        Ok(HostOutcome::Unknown { detail }) => {
            assert!(detail.contains("did not answer"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "the hub sent the dispatch once and did not retry"
    );
    drop_pool(&world.pool, &world.name).await;
}

fn short_lease() -> HubConfig {
    HubConfig {
        lease_ttl_seconds: 2,
        ..HubConfig::default()
    }
}

#[tokio::test]
async fn heartbeats_keep_the_lease_alive_past_its_ttl_and_the_target_records_what_the_worker_observed(
) {
    let Some(world) = world_with("wk_beat", short_lease()).await else {
        return blocked_marker();
    };
    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    let root = tempfile::tempdir().unwrap();
    let _worker = attach_worker_beating(&world, &target, root.path(), Duration::from_millis(500));
    attached(&world, 1).await;
    // Three times the TTL: without renewal the lease would have lapsed twice over.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        lease_is_live(&world.pool, &target).await,
        "heartbeats renewed the lease"
    );
    let host = routed(&world, Arc::new(Recording::default()));
    let out = success(
        host.execute(dispatch(
            "terminal.exec",
            json!({"command": "echo alive"}),
            5000,
        ))
        .await,
    );
    assert_eq!(out["output"].as_str().unwrap().trim(), "alive");

    let mut tx = world.pool.begin().await.unwrap();
    let seen = MachineControl::load(&mut tx, TENANT, &target)
        .await
        .unwrap();
    assert_eq!(seen.observed_state.as_deref(), Some("READY"));
    assert!(
        seen.last_heartbeat_at.is_some(),
        "the heartbeat is recorded on the target"
    );
    drop(tx);
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_worker_that_goes_quiet_is_fenced_when_its_lease_lapses_and_its_work_is_refused() {
    let Some(world) = world_with("wk_quiet", short_lease()).await else {
        return blocked_marker();
    };
    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    let root = tempfile::tempdir().unwrap();
    // Connected, leased, and never heartbeating.
    let _worker = attach_worker(&world, &target, root.path());
    attached(&world, 1).await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !lease_is_live(&world.pool, &target).await,
        "the lease lapsed with no heartbeat"
    );
    let host = routed(&world, Arc::new(Recording::default()));
    match host
        .execute(dispatch(
            "terminal.exec",
            json!({"command": "touch should-not-exist"}),
            3000,
        ))
        .await
    {
        Err(HostFailure::Refused { detail }) => {
            assert!(detail.contains("WORKER_NOT_DISPATCHED"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !root.path().join("should-not-exist").exists(),
        "nothing ran under a lapsed lease"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_lease_revoked_in_the_control_plane_stops_forwarding_even_before_its_cached_expiry() {
    let Some(world) = world("wk_revoke").await else {
        return blocked_marker();
    };
    let target = register(&world.pool, Substrate::CustomerPrivateWorker, true).await;
    let root = tempfile::tempdir().unwrap();
    let _worker = attach_worker(&world, &target, root.path());
    attached(&world, 1).await;
    let host = routed(&world, Arc::new(Recording::default()));
    success(
        host.execute(dispatch(
            "terminal.exec",
            json!({"command": "touch before"}),
            5000,
        ))
        .await,
    );
    assert!(root.path().join("before").exists());

    // An operator fences the worker. Its lease has a minute left in the hub's own copy and in the worker's.
    let lease_id = world.hub.links()[0].lease_id.clone();
    let mut tx = world.pool.begin().await.unwrap();
    MachineControl::revoke_lease(&mut tx, TENANT, &lease_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    match host
        .execute(dispatch(
            "terminal.exec",
            json!({"command": "touch after"}),
            5000,
        ))
        .await
    {
        Err(HostFailure::Refused { detail }) => {
            assert!(detail.contains("WORKER_NOT_DISPATCHED"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !root.path().join("after").exists(),
        "nothing was sent to a worker whose lease was revoked"
    );
    drop_pool(&world.pool, &world.name).await;
}
