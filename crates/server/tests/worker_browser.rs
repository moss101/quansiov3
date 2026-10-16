//! `browser.*` tools through the server's hub, a worker's own Chrome and the real database (EXEC-009 composition).
//!
//! Real PostgreSQL leases and browser sessions, a real worker session on a Unix socket, a real Google Chrome the
//! worker launches, and a local HTTP site whose counters are how many requests the page really made.
//!
//! * **a call acts on its own run's session**: the model cannot know a session id and may not name one; whatever
//!   it writes is replaced by the session of the run the call belongs to, opened on the run's first call;
//! * **a takeover fences the agent**: while a person holds the session nothing is sent to the worker, and the
//!   page sees no input; after the handback the same call goes through;
//! * **the approved tier comes from the taxonomy**: a click is dispatched under tier 3
//!   (`computer.input.privileged`), and a control that would settle above it is refused before any input;
//! * **fail closed**: a browser tool a worker cannot serve is refused, never answered by the conformance stub, and
//!   with no worker attached every `browser.*` call is `WORKER_UNAVAILABLE`.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, and a Chrome on this host (else `BLOCKED_EXTERNAL`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_machine::control::browser::BrowserSessionStore;
use quansio_machine::control::{
    MachineControl, NewTarget, Substrate, TargetClass, TargetFence, TargetStatus,
};
use quansio_qworkerd::browser_runtime::BrowserRuntime;
use quansio_qworkerd::daemon::{serve, UnattachedEvidence};
use quansio_qworkerd::executor::LocalToolExecutor;
use quansio_qworkerd::host::WorkerHost;
use quansio_qworkerd::tools::{RootScope, Sandbox};
use quansio_server::runtime::turn_loop::{HostDispatch, HostFailure, HostOutcome, ToolHostPort};
use quansio_server::worker::{HubConfig, WorkerHub, WorkerRoutedHost, WorkerToolHost};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YBRW002";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YBRW002";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YBRW002";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YBRW002";
const RUN_A: &str = "run_01J8Z3K6F1N8VQ2X5W9YBRWAAA";
const RUN_B: &str = "run_01J8Z3K6F1N8VQ2X5W9YBRWBBB";
const PAGE: &str = "<html><head><title>Shop</title></head><body>\
<button onclick=\"var x=new XMLHttpRequest();x.open('GET','/hit',false);x.send();\">Count</button>\
<button onclick=\"var x=new XMLHttpRequest();x.open('GET','/buy',false);x.send();\">Buy now</button>\
</body></html>";
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn chrome() -> Option<&'static str> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).exists())
}

/// A site on loopback that counts the requests it really receives.
struct Site {
    port: u16,
    hits: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
}

impl Site {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>> = Arc::default();
        let counted = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let counted = counted.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![0_u8; 4096];
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                    *counted.lock().unwrap().entry(path.clone()).or_default() += 1;
                    let (kind, body) = if path == "/" {
                        ("text/html", PAGE)
                    } else {
                        ("text/plain", "ok")
                    };
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
        });
        Self { port, hits }
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
    fn hits(&self, path: &str) -> u32 {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

struct World {
    name: String,
    pool: PgPool,
    hub: WorkerHub,
    target: String,
    _dir: tempfile::TempDir,
    _state: tempfile::TempDir,
    _root: tempfile::TempDir,
}

fn id(prefix: Prefix) -> String {
    CanonicalId::generate(prefix, &mut UlidGenerator::new()).to_string()
}

async fn world(label: &str, with_worker: bool) -> Option<World> {
    let name = admin_url().map(|_| scratch_name(label))?;
    let chrome = chrome()?;
    let pool = fresh_database(&name).await?;
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    let dir = tempfile::Builder::new()
        .prefix("w")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("w.sock");
    let hub = WorkerHub::new(pool.clone(), HubConfig::default());
    hub.listen(&socket).expect("listen");

    let target = id(Prefix::ExecutionTarget);
    let mut tx = pool.begin().await.unwrap();
    MachineControl::register(
        &mut tx,
        TENANT,
        &NewTarget {
            id: target.clone(),
            workspace_id: WORKSPACE.to_string(),
            class: TargetClass::PersistentWorkspaceComputer,
            substrate: Substrate::CustomerPrivateWorker,
            image_digest: None,
            desired_state: Some("READY".to_string()),
        },
    )
    .await
    .unwrap();
    for to in [TargetStatus::Provisioning, TargetStatus::Ready] {
        MachineControl::transition(&mut tx, TENANT, &target, &TargetFence { generation: 1 }, to)
            .await
            .unwrap();
    }
    // The two runs a session may belong to: a worker thread and a RUNNING run on the target for each, as the
    // runtime would have created them.
    for (n, run) in [(1, RUN_A), (2, RUN_B)] {
        let thread = format!("ath_01J8Z3K6F1N8VQ2X5W9YBRWT0{n}");
        sqlx::query(
            "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, work_node_id, status, execution_target_id) \
             VALUES ($1, $2, $3, 'worker', $4, 'ACTIVE', $5)",
        )
        .bind(&thread)
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(NODE)
        .bind(&target)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, status, \
                               trigger_kind, execution_target_id, started_at) \
             VALUES ($1, $2, $3, $4, $5, 1, 'RUNNING', 'manual', $6, now())",
        )
        .bind(run)
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(NODE)
        .bind(&thread)
        .bind(&target)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    if with_worker {
        let (socket, target, root_path, state_path) = (
            socket.clone(),
            target.clone(),
            root.path().to_path_buf(),
            state.path().to_path_buf(),
        );
        tokio::spawn(async move {
            let sandbox = Sandbox::empty()
                .authorize(RootScope::Workspace, &root_path)
                .unwrap();
            let executor = LocalToolExecutor::new(sandbox, TENANT).with_browser(
                BrowserRuntime::new(chrome, state_path, TENANT).allow_private_network(true),
            );
            let mut host = WorkerHost::new(executor, UnattachedEvidence);
            let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let (reader, writer) = stream.into_split();
            let _ = serve(
                &mut host,
                TENANT,
                &target,
                Duration::from_secs(3600),
                reader,
                writer,
            )
            .await;
        });
        for _ in 0..100 {
            if hub.links().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(hub.links().len(), 1, "the worker attached");
    }
    Some(World {
        name,
        pool,
        hub,
        target,
        _dir: dir,
        _state: state,
        _root: root,
    })
}

#[derive(Default)]
struct Stub;

#[async_trait]
impl ToolHostPort for Stub {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        Ok(HostOutcome::Success {
            output: json!({ "stub": dispatch.tool }),
            evidence_ids: vec![],
            remote_ref: None,
        })
    }
}

fn host(world: &World) -> WorkerRoutedHost {
    WorkerRoutedHost::new(
        Arc::new(WorkerToolHost::new(world.hub.clone())),
        Arc::new(Stub),
    )
}

fn dispatch(run: &str, tool: &str, args: Value) -> HostDispatch {
    let n = SEQUENCE.fetch_add(1, Ordering::SeqCst);
    HostDispatch {
        tool_call_id: id(Prefix::ToolCall),
        tool: tool.to_string(),
        version: 1,
        args,
        effect_id: format!("eff_{n:026}"),
        dispatch_token: format!("dsp_{n:026}"),
        run_id: run.to_string(),
        step_id: id(Prefix::Step),
        workspace_id: WORKSPACE.to_string(),
        tenant_id: TENANT.to_string(),
        acting_user_id: Some(USER.to_string()),
        max_output_bytes: 1 << 20,
        timeout_ms: 60_000,
    }
}

async fn ok(host: &WorkerRoutedHost, run: &str, tool: &str, args: Value) -> Value {
    match host.execute(dispatch(run, tool, args)).await {
        Ok(HostOutcome::Success { output, .. }) => output,
        other => panic!("{tool}: expected success, got {other:?}"),
    }
}

/// The page the run has open: its tab and the element ref named `name`.
async fn open_and_click_args(host: &WorkerRoutedHost, run: &str, url: &str, name: &str) -> Value {
    let navigated = ok(
        host,
        run,
        "browser.navigate",
        json!({"session_id": "bsn_the_model_made_this_up", "url": url}),
    )
    .await;
    let tab = navigated["tab_id"].as_str().unwrap().to_string();
    let observed = ok(
        host,
        run,
        "browser.observe",
        json!({"session_id": "x", "tab_id": tab}),
    )
    .await;
    let element = observed["elements"]
        .as_array()
        .and_then(|elements| elements.iter().find(|e| e["name"] == name))
        .unwrap_or_else(|| panic!("no element named {name:?} in {observed}"));
    json!({
        "session_id": "x",
        "tab_id": tab,
        "expected_document_revision": observed["document_revision"],
        "snapshot_id": observed["snapshot_id"],
        "ref": element["ref"],
    })
}

async fn sessions(pool: &PgPool, target: &str) -> Vec<(String, Option<String>)> {
    let mut tx = pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    sqlx::query("SELECT id, run_id FROM browser_sessions WHERE target_id = $1 ORDER BY id")
        .bind(target)
        .fetch_all(&mut *tx)
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get("id"), row.get("run_id")))
        .collect()
}

#[tokio::test]
async fn a_call_acts_on_its_own_runs_session_whatever_session_id_the_model_wrote() {
    let Some(world) = world("wb_bind", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    assert!(
        sessions(&world.pool, &world.target).await.is_empty(),
        "no session before the first call"
    );

    let navigated = ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "bsn_the_model_made_this_up", "url": site.url()}),
    )
    .await;
    let tab_a = navigated["tab_id"].as_str().unwrap().to_string();
    let first = sessions(&world.pool, &world.target).await;
    assert_eq!(first.len(), 1, "the first call opened the run's session");
    assert_eq!(first[0].1.as_deref(), Some(RUN_A));
    assert!(first[0].0.starts_with("bsn_") && first[0].0 != "bsn_the_model_made_this_up");

    // The same run, naming yet another session, uses the same one.
    ok(
        &host,
        RUN_A,
        "browser.observe",
        json!({"session_id": first[0].0.replace("bsn_", "bsn_x")}),
    )
    .await;
    assert_eq!(sessions(&world.pool, &world.target).await.len(), 1);

    // Another run gets its own, and cannot reach the first by naming it.
    ok(
        &host,
        RUN_B,
        "browser.navigate",
        json!({"session_id": first[0].0, "url": site.url()}),
    )
    .await;
    // Run B names run A's session *and* run A's tab. Forwarded as written the call would read A's page; acting on
    // B's own session, A's tab does not exist there.
    match host
        .execute(dispatch(
            RUN_B,
            "browser.observe",
            json!({"session_id": first[0].0, "tab_id": tab_a}),
        ))
        .await
    {
        Ok(HostOutcome::Failure { error, .. }) => {
            assert_eq!(error["code"], "BROWSER_TARGET_NOT_FOUND", "{error}");
        }
        other => panic!("run B reached run A's tab: {other:?}"),
    }
    let both = sessions(&world.pool, &world.target).await;
    assert_eq!(both.len(), 2);
    assert!(both.iter().any(|(_, run)| run.as_deref() == Some(RUN_B)));
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_takeover_fences_the_agent_before_anything_is_sent_and_a_handback_lifts_it() {
    let Some(world) = world("wb_fence", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let click = open_and_click_args(&host, RUN_A, &site.url(), "Count").await;
    let session = sessions(&world.pool, &world.target).await.remove(0).0;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut tx = world.pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    BrowserSessionStore::request_takeover(&mut tx, TENANT, &session, &now)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    match host
        .execute(dispatch(RUN_A, "browser.click", click.clone()))
        .await
    {
        Err(HostFailure::Refused { detail }) => {
            assert!(detail.contains("BROWSER_INPUT_FENCED"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        site.hits("/hit"),
        0,
        "the page saw no input during the takeover"
    );

    let mut tx = world.pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    BrowserSessionStore::handback(&mut tx, TENANT, &session, &now)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    ok(&host, RUN_A, "browser.click", click).await;
    assert_eq!(
        site.hits("/hit"),
        1,
        "after the handback the same click goes through, once"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn the_ceiling_is_the_taxonomy_s_tier_for_the_tool_and_a_higher_consequence_is_refused_before_input(
) {
    assert_eq!(
        quansio_server::worker::approved_tier("browser.click"),
        Some(3),
        "computer.input.privileged"
    );
    assert_eq!(
        quansio_server::worker::approved_tier("browser.navigate"),
        Some(2),
        "network.egress.new_destination"
    );
    assert_eq!(
        quansio_server::worker::approved_tier("browser.observe"),
        Some(0)
    );

    let Some(world) = world("wb_tier", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let buy = open_and_click_args(&host, RUN_A, &site.url(), "Buy now").await;
    match host.execute(dispatch(RUN_A, "browser.click", buy)).await {
        Ok(HostOutcome::Failure {
            retryable, error, ..
        }) => {
            assert_eq!(error["code"], "BROWSER_EFFECT_TIER_EXCEEDED", "{error}");
            assert!(!retryable || error["requires_new_observation"] == true);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(site.hits("/buy"), 0, "no input reached the page");
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_browser_tool_a_worker_cannot_serve_is_refused_and_never_answered_by_a_stub() {
    let Some(world) = world("wb_unrouted", true).await else {
        return blocked_marker();
    };
    let host = host(&world);
    for tool in [
        "browser.import_session",
        "browser.fill_payment_field",
        "browser.something_new",
    ] {
        match host
            .execute(dispatch(RUN_A, tool, json!({"session_id": "x"})))
            .await
        {
            Err(HostFailure::Refused { detail }) => assert!(
                detail.contains("BROWSER_TOOL_NOT_ROUTED"),
                "{tool}: {detail}"
            ),
            other => panic!("{tool}: {other:?}"),
        }
    }
    assert!(
        sessions(&world.pool, &world.target).await.is_empty(),
        "a refused tool opened no session"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn with_no_worker_attached_every_browser_call_is_worker_unavailable() {
    let Some(world) = world("wb_none", false).await else {
        return blocked_marker();
    };
    let host = host(&world);
    match host
        .execute(dispatch(
            RUN_A,
            "browser.observe",
            json!({"session_id": "x"}),
        ))
        .await
    {
        Err(HostFailure::Refused { detail }) => {
            assert!(detail.contains("WORKER_UNAVAILABLE"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    drop_pool(&world.pool, &world.name).await;
}
