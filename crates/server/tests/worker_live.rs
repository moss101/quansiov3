//! The control plane's side of a worker's browser: guard reports up, takeover, handback and decisions down (APP-007
//! over EXEC-009), through the real HTTP commands.
//!
//! Real PostgreSQL, a real worker session on a Unix socket, a real Google Chrome, and a local site whose counters are
//! how many requests the page really made.
//!
//! * **a guard report reaches the database**, and only for a session on the target the worker holds: a second worker
//!   cannot write the first one's session;
//! * **`RequestTakeover` and `Handback` move the worker's driver**: while a person holds the session their click lands
//!   in the page, and after the handback the driver refuses it — the relay is proved by what the page sees;
//! * **a decision is relayed** when a worker is attached, and the response says when it was not.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, and a Chrome on this host (else `BLOCKED_EXTERNAL`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
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
const BIG: &str = "<html><head><title>Big</title></head><body style=\"margin:0\">\
<button style=\"position:fixed;left:0;top:0;width:100vw;height:100vh\" onclick=\"var x=new XMLHttpRequest();x.open('GET','/hit',false);x.send();\">Count</button>\
</body></html>";
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
                    let (kind, body) = match path.as_str() {
                        "/" => ("text/html", PAGE),
                        "/big" => ("text/html", BIG),
                        _ => ("text/plain", "ok"),
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
    fn big(&self) -> String {
        format!("http://127.0.0.1:{}/big", self.port)
    }
    fn hits(&self, path: &str) -> u32 {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

struct World {
    name: String,
    pool: PgPool,
    hub: WorkerHub,
    router: axum::Router,
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
    let api = quansio_server::api::ApiState::new(pool.clone())
        .expect("api")
        .with_worker_hub(hub.clone());
    Some(World {
        name,
        pool,
        hub,
        router: quansio_server::api::router(api),
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

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0E{n:04}")
}

async fn command(world: &World, name: &str, id: u32, params: Value) -> (u16, Value) {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/v1/commands/{name}"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({ "command_id": command_id(id), "params": params }).to_string(),
        ))
        .unwrap();
    let response = world.router.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn guard(pool: &PgPool, session: &str) -> Value {
    let mut tx = pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    sqlx::query_scalar("SELECT guard_state FROM browser_sessions WHERE id = $1")
        .bind(session)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn a_guard_report_reaches_the_database_after_an_action() {
    let Some(world) = world("wl_guard", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.url()}),
    )
    .await;
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    eventually("the worker's guard report is stored", || async {
        let state = guard(&world.pool, &session).await;
        state["reported_at"].is_string()
            && state["tabs"]
                .as_array()
                .is_some_and(|tabs| tabs.iter().any(|tab| tab["url"] == site.url()))
    })
    .await;
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_worker_cannot_write_a_guard_report_for_a_session_on_another_target() {
    let Some(world) = world("wl_rogue", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.url()}),
    )
    .await;
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    eventually("the real report is stored", || async {
        guard(&world.pool, &session).await["reported_at"].is_string()
    })
    .await;
    let before = guard(&world.pool, &session).await;

    // A second target, READY and leased by a second worker that says things about the first target's session.
    let other = register_target(&world.pool).await;
    let socket = world._dir.path().join("w.sock");
    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let hello = quansio_machine::gateway::wire::encode(
        &quansio_machine::gateway::wire::FromWorker::Hello {
            tenant_id: TENANT.to_string(),
            target_id: other.clone(),
        },
    )
    .unwrap();
    writer.write_all(hello.as_bytes()).await.unwrap();
    for _ in 0..100 {
        if world.hub.links().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(world.hub.links().len(), 2, "the second worker attached");
    let forged = quansio_machine::gateway::wire::encode(&quansio_machine::gateway::wire::FromWorker::BrowserGuard {
        session_id: session.clone(),
        report: json!({"tabs": [{"tab_id": "forged", "url": "https://evil.example/", "document_revision": "rev_x",
                                  "certificates": [], "downloads": [], "popups_blocked": 0}], "in_doubt": []}),
    })
    .unwrap();
    writer.write_all(forged.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(reader);
    assert_eq!(
        guard(&world.pool, &session).await,
        before,
        "the other target's report changed nothing"
    );
    drop_pool(&world.pool, &world.name).await;
}

async fn register_target(pool: &PgPool) -> String {
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
    tx.commit().await.unwrap();
    target
}

#[tokio::test]
async fn takeover_and_handback_commands_move_the_workers_driver_and_the_page_sees_only_the_users_clicks(
) {
    use quansio_machine::gateway::wire::BrowserOrder;
    let Some(world) = world("wl_control", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let navigated = ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.big()}),
    )
    .await;
    let tab = navigated["tab_id"].as_str().unwrap().to_string();
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let click = |x: f64| BrowserOrder::UserClick {
        session_id: session.clone(),
        tab_id: tab.clone(),
        x,
        y: 50.0,
    };
    let hits = || site.hits("/hit");

    // The agent holds the session: the driver refuses a user's click.
    world
        .hub
        .send_order(TENANT, &world.target, click(50.0))
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        hits(),
        0,
        "a user click is refused while the agent holds the session"
    );

    // The person takes over through the real command; the worker's driver follows.
    let (status, taken) =
        command(&world, "RequestTakeover", 1, json!({"session_id": session})).await;
    assert_eq!(status, 200, "{taken}");
    assert_eq!(taken["result"]["worker_relayed"], true, "{taken}");
    world
        .hub
        .send_order(TENANT, &world.target, click(50.0))
        .await;
    eventually("the user's click lands in the page", || async {
        hits() == 1
    })
    .await;

    // The person hands back; the driver refuses the user again.
    let (status, back) = command(&world, "Handback", 2, json!({"session_id": session})).await;
    assert_eq!(status, 200, "{back}");
    assert_eq!(back["result"]["worker_relayed"], true, "{back}");
    world
        .hub
        .send_order(TENANT, &world.target, click(50.0))
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        hits(),
        1,
        "after the handback a user click is refused again"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_decision_is_relayed_to_an_attached_worker_and_the_response_says_when_none_was() {
    let Some(world) = world("wl_decision", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let navigated = ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.url()}),
    )
    .await;
    let tab = navigated["tab_id"].as_str().unwrap().to_string();
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let params = json!({"session_id": session, "tab_id": tab, "origin": "https://meet.example.com",
                        "permission": "microphone", "decision": "allow"});
    let (status, decided) = command(&world, "DecideBrowserPermission", 3, params.clone()).await;
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["result"]["worker_relayed"], true, "{decided}");

    // With no worker attached the decision is still recorded, and the response says the worker was not told.
    let bare = world_without_hub(&world).await;
    let (status, recorded) = command(&bare, "DecideBrowserPermission", 4, params).await;
    assert_eq!(status, 200, "{recorded}");
    assert_eq!(recorded["result"]["worker_relayed"], false, "{recorded}");
    drop_pool(&world.pool, &world.name).await;
}

/// The same database behind a server that composes no worker hub.
async fn world_without_hub(world: &World) -> World {
    let state = quansio_server::api::ApiState::new(world.pool.clone()).expect("api");
    World {
        name: world.name.clone(),
        pool: world.pool.clone(),
        hub: world.hub.clone(),
        router: quansio_server::api::router(state),
        target: world.target.clone(),
        _dir: tempfile::tempdir().unwrap(),
        _state: tempfile::tempdir().unwrap(),
        _root: tempfile::tempdir().unwrap(),
    }
}
