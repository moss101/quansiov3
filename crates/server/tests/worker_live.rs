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
const LIVE: &str = "<html><body><div id=\"c\">0</div><script>var n=0;function t(){document.getElementById('c').textContent=String(++n);setTimeout(t,100)}t()</script></body></html>";
const KEYS: &str = "<html><body style=\"margin:0;height:6000px\"><input id=\"i\" autofocus style=\"width:200px\" onkeydown=\"var x=new XMLHttpRequest();x.open('GET','/key-'+event.key,false);x.send();\"><script>window.addEventListener('scroll',function(){var x=new XMLHttpRequest();x.open('GET','/scrolled',false);x.send();});document.getElementById('i').focus();</script></body></html>";
/// A page that reports, by request, what the pointer and keyboard did: the button of a click, a double click, a drag, a
/// click with Shift held, and a shortcut.
const POINTER: &str = "<html><body style=\"margin:0\"><div id=\"d\" style=\"width:600px;height:400px;background:#ddd\"></div><input id=\"i\" style=\"position:absolute;top:420px\"><script>\
function ping(p){var x=new XMLHttpRequest();x.open('GET',p,false);x.send();}\
var d=document.getElementById('d'),down=null;\
d.addEventListener('contextmenu',function(e){e.preventDefault();ping('/ctx');});\
d.addEventListener('dblclick',function(){ping('/dbl');});\
d.addEventListener('mousedown',function(e){down=[e.clientX,e.clientY];});\
d.addEventListener('mouseup',function(e){if(down&&Math.abs(e.clientX-down[0])>50)ping('/drag');down=null;});\
d.addEventListener('click',function(e){if(e.shiftKey)ping('/shift-click');});\
var i=document.getElementById('i');i.focus();\
document.addEventListener('keydown',function(e){if((e.ctrlKey||e.metaKey)&&e.key==='a')ping('/select-all');});\
</script></body></html>";
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
                        "/live" => ("text/html", LIVE),
                        "/keys" => ("text/html", KEYS),
                        "/pointer" => ("text/html", POINTER),
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
    fn pointer(&self) -> String {
        format!("http://127.0.0.1:{}/pointer", self.port)
    }
    fn keys(&self) -> String {
        format!("http://127.0.0.1:{}/keys", self.port)
    }
    fn animated(&self) -> String {
        format!("http://127.0.0.1:{}/live", self.port)
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
    live: Arc<quansio_server::api::live::LiveHub>,
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
            let host = std::sync::Arc::new(WorkerHost::new(executor, UnattachedEvidence));
            let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let (reader, writer) = stream.into_split();
            let _ = serve(
                host,
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
        live: api.live().clone(),
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
    command_as(world, USER, name, id, params).await
}

async fn command_as(world: &World, user: &str, name: &str, id: u32, params: Value) -> (u16, Value) {
    command_on(world.router.clone(), user, name, id, params).await
}

async fn command_on(
    router: axum::Router,
    user: &str,
    name: &str,
    id: u32,
    params: Value,
) -> (u16, Value) {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/v1/commands/{name}"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", user)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({ "command_id": command_id(id), "params": params }).to_string(),
        ))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
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
        button: "left".to_string(),
        click_count: 1,
        modifiers: Vec::new(),
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
        live: state.live().clone(),
        router: quansio_server::api::router(state),
        target: world.target.clone(),
        _dir: tempfile::tempdir().unwrap(),
        _state: tempfile::tempdir().unwrap(),
        _root: tempfile::tempdir().unwrap(),
    }
}

/// Everything the live hub has for the tenant so far, as `(channel, frame)`.
fn drain(
    receiver: &mut tokio::sync::broadcast::Receiver<quansio_events::LiveFrame>,
) -> Vec<(String, Value)> {
    let mut frames = Vec::new();
    while let Ok(frame) = receiver.try_recv() {
        frames.push((frame.channel, frame.frame));
    }
    frames
}

#[tokio::test]
async fn frames_reach_the_live_hub_while_a_person_holds_the_session_and_a_handback_stops_the_stream(
) {
    let Some(world) = world("wl_frames", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let mut watching = world.live.subscribe(TENANT);
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.animated()}),
    )
    .await;
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let channel = format!("target:{}", world.target);
    assert!(
        drain(&mut watching)
            .iter()
            .all(|(_, frame)| frame["kind"] != "browser.frame"),
        "no one is holding the session, so no frames were asked for"
    );

    let (status, taken) = command(
        &world,
        "RequestTakeover",
        11,
        json!({"session_id": session}),
    )
    .await;
    assert_eq!(status, 200, "{taken}");
    // Chromium sends a first picture as soon as the screencast starts.
    let mut seen = Vec::new();
    for _ in 0..100 {
        seen.extend(
            drain(&mut watching)
                .into_iter()
                .filter(|(_, frame)| frame["kind"] == "browser.frame"),
        );
        if !seen.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!seen.is_empty(), "a frame reached the live hub");
    let (published_on, frame) = &seen[0];
    assert_eq!(published_on, &channel);
    assert_eq!(frame["session_id"], session.as_str());
    assert_eq!(frame["media_type"], "image/jpeg");
    assert!(frame["data"].as_str().is_some_and(|data| data.len() > 100
        && data
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))));
    assert_eq!(frame["url"], site.animated());
    assert!(
        frame["width"].as_u64().is_some_and(|w| w > 0)
            && frame["height"].as_u64().is_some_and(|h| h > 0),
        "the page's size travels with the picture"
    );
    assert!(frame["sequence"].as_u64().is_some_and(|n| n >= 1));

    // After the handback nobody is watching and the agent holds the session, so the stream stops. The page is animated, so a
    // stream that kept running would keep delivering pictures; the hub's own live receiver needs no socket to hear them.
    let (status, back) = command(&world, "Handback", 12, json!({"session_id": session})).await;
    assert_eq!(status, 200, "{back}");
    tokio::time::sleep(Duration::from_millis(600)).await;
    drain(&mut watching);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        drain(&mut watching)
            .iter()
            .all(|(_, frame)| frame["kind"] != "browser.frame"),
        "pictures kept coming after the handback"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_forged_frame_for_another_targets_session_is_never_published() {
    let Some(world) = world("wl_forgedframe", true).await else {
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
    let other = register_target(&world.pool).await;
    let mut watching = world.live.subscribe(TENANT);

    let stream = tokio::net::UnixStream::connect(world._dir.path().join("w.sock"))
        .await
        .unwrap();
    let (_reader, mut writer) = stream.into_split();
    let hello = quansio_machine::gateway::wire::encode(
        &quansio_machine::gateway::wire::FromWorker::Hello {
            tenant_id: TENANT.to_string(),
            target_id: other,
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
    let forged = quansio_machine::gateway::wire::encode(
        &quansio_machine::gateway::wire::FromWorker::BrowserFrame {
            session_id: session,
            tab_id: "forged".to_string(),
            media_type: "image/jpeg".to_string(),
            data: "Rk9SR0VE".to_string(),
            url: "https://evil.example/".to_string(),
            device_width: 0,
            device_height: 0,
            sequence: 1,
        },
    )
    .unwrap();
    writer.write_all(forged.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        drain(&mut watching)
            .iter()
            .all(|(_, frame)| frame["data"] != "Rk9SR0VE"),
        "a frame from a worker that does not hold the session's target was published"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_person_s_input_is_delivered_only_while_they_hold_the_session_and_every_malformed_one_is_refused(
) {
    let Some(world) = world("wl_input", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.big()}),
    )
    .await;
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let tab = guard(&world.pool, &session).await["tabs"][0]["tab_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    eventually("the guard names the tab", || async {
        !guard(&world.pool, &session).await["tabs"]
            .as_array()
            .is_none_or(Vec::is_empty)
    })
    .await;
    let tab = if tab.is_empty() {
        guard(&world.pool, &session).await["tabs"][0]["tab_id"]
            .as_str()
            .unwrap()
            .to_string()
    } else {
        tab
    };
    let click = |x: f64, y: f64| json!({"session_id": session, "tab_id": tab, "kind": "click", "x": x, "y": y});

    // The agent holds the session: a person's input has nowhere to go.
    let (status, refused) = command(&world, "SendBrowserInput", 21, click(50.0, 50.0)).await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(site.hits("/hit"), 0);

    let (status, taken) = command(
        &world,
        "RequestTakeover",
        22,
        json!({"session_id": session}),
    )
    .await;
    assert_eq!(status, 200, "{taken}");
    let (status, delivered) = command(&world, "SendBrowserInput", 23, click(50.0, 50.0)).await;
    assert_eq!(status, 200, "{delivered}");
    assert_eq!(delivered["result"]["delivered"], true);
    eventually("the person's click lands in the page", || async {
        site.hits("/hit") == 1
    })
    .await;

    // Malformed inputs are refused before they reach a worker.
    for (n, bad) in [
        (
            24,
            json!({"session_id": session, "tab_id": tab, "kind": "click", "x": -1, "y": 5}),
        ),
        (
            25,
            json!({"session_id": session, "tab_id": tab, "kind": "click", "x": 5, "y": 99999}),
        ),
        (
            26,
            json!({"session_id": session, "tab_id": tab, "kind": "drag"}),
        ),
        (
            27,
            json!({"session_id": session, "tab_id": tab, "kind": "text", "text": "x".repeat(5000)}),
        ),
        (
            28,
            json!({"session_id": session, "kind": "click", "x": 1, "y": 1}),
        ),
    ] {
        let (status, body) = command(&world, "SendBrowserInput", n, bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {body}");
    }
    assert_eq!(site.hits("/hit"), 1, "nothing malformed reached the page");
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn an_input_that_cannot_be_delivered_is_an_error_and_not_a_silent_drop() {
    let Some(world) = world("wl_inputgone", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.big()}),
    )
    .await;
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut tx = world.pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    quansio_machine::control::browser::BrowserSessionStore::request_takeover(
        &mut tx, TENANT, &session, &now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // The person holds the session, but this server has no worker hub.
    let bare = world_without_hub(&world).await;
    let (status, body) = command(
        &bare,
        "SendBrowserInput",
        31,
        json!({"session_id": session, "tab_id": "t", "kind": "click", "x": 1, "y": 1}),
    )
    .await;
    assert_eq!(status, 503, "{body}");
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_handback_the_worker_missed_is_repaired_before_the_next_agent_call() {
    let Some(world) = world("wl_repair", true).await else {
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

    // The person takes over through the command, so the worker's driver is user-held.
    let (status, taken) = command(
        &world,
        "RequestTakeover",
        41,
        json!({"session_id": session}),
    )
    .await;
    assert_eq!(status, 200, "{taken}");
    // The handback is recorded in the database but never reaches the worker (it was away, restarting, or the relay failed).
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut tx = world.pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    quansio_machine::control::browser::BrowserSessionStore::handback(
        &mut tx, TENANT, &session, &now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // The database says the agent holds the session. The next agent call states that to the worker first, so its driver
    // does not refuse the agent for a takeover that has ended.
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.big()}),
    )
    .await;
    drop_pool(&world.pool, &world.name).await;
}

// ------------------------------------------------------------------ watching the agent work

/// A WebSocket viewer of `/v1/stream`, subscribed to the target's channel.
struct Viewer {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

async fn serve_http(world: &World) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = world.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    addr
}

impl Viewer {
    async fn join(addr: std::net::SocketAddr, target: &str) -> Self {
        Self::join_as(addr, USER, target).await
    }

    async fn join_as(addr: std::net::SocketAddr, user: &str, target: &str) -> Self {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;
        let mut request = format!("ws://{addr}/v1/stream")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("x-quansio-tenant", TENANT.parse().unwrap());
        request
            .headers_mut()
            .insert("x-quansio-user", user.parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect");
        ws.send(Message::Text(
            json!({ "subscribe": [{ "channel": format!("target:{target}") }] }).to_string(),
        ))
        .await
        .expect("subscribe");
        Self { ws }
    }

    /// The next browser frame within `ms`.
    async fn frame(&mut self, ms: u64) -> Option<Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let value: Value = serde_json::from_str(&text).ok()?;
                    if value["kind"] == "live" && value["frame"]["kind"] == "browser.frame" {
                        return Some(value["frame"].clone());
                    }
                }
                Ok(Some(Ok(_))) => {}
                _ => return None,
            }
        }
    }

    /// The next text message within `ms`, parsed.
    async fn next_text(&mut self, ms: u64) -> Option<Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => return serde_json::from_str(&text).ok(),
                Ok(Some(Ok(_))) => {}
                _ => return None,
            }
        }
    }

    async fn leave(mut self) {
        let _ = self.ws.close(None).await;
    }
}

#[tokio::test]
async fn a_viewer_sees_the_agent_work_whether_it_joins_before_the_browser_exists_or_after() {
    let Some(world) = world("wl_watch", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let addr = serve_http(&world).await;

    // Joined first: nothing is running yet, so the first frame comes when the agent's call creates the browser.
    let mut early = Viewer::join(addr, &world.target).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.big()}),
    )
    .await;
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let seen = early
        .frame(15_000)
        .await
        .expect("the agent's browser was shown to a viewer who was already there");
    assert_eq!(seen["session_id"], session.as_str());
    assert_eq!(seen["url"], site.big());

    // Joined later: the stream is already there to be found.
    let mut late = Viewer::join(addr, &world.target).await;
    // The page is static, so the browser sends nothing new: the late viewer is given a current picture on arrival.
    let also = late
        .frame(15_000)
        .await
        .expect("a viewer who arrived late saw nothing");
    assert_eq!(also["session_id"], session.as_str());
    early.leave().await;
    late.leave().await;
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn with_no_viewer_the_agent_works_unwatched_and_nothing_is_streamed() {
    let Some(world) = world("wl_unwatched", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let mut watching = world.live.subscribe(TENANT);
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.animated()}),
    )
    .await;
    ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.animated()}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(
        drain(&mut watching)
            .iter()
            .all(|(_, frame)| frame["kind"] != "browser.frame"),
        "frames were produced for a session nobody is watching"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn the_stream_follows_a_newer_tab_and_stops_when_the_last_viewer_leaves() {
    let Some(world) = world("wl_follow", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let addr = serve_http(&world).await;
    let first = ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.animated()}),
    )
    .await;
    let first_tab = first["tab_id"].as_str().unwrap().to_string();

    let mut one = Viewer::join(addr, &world.target).await;
    let a_frame = one.frame(15_000).await.expect("a frame of the first tab");
    assert_eq!(a_frame["tab_id"], first_tab.as_str());

    // The agent opens another tab (a navigate with no tab names a new one): the viewer is moved to it.
    let second = ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.animated()}),
    )
    .await;
    let second_tab = second["tab_id"].as_str().unwrap().to_string();
    assert_ne!(first_tab, second_tab);
    let mut followed = false;
    for _ in 0..40 {
        match one.frame(500).await {
            Some(frame) if frame["tab_id"] == second_tab.as_str() => {
                followed = true;
                break;
            }
            Some(_) => {}
            None => {}
        }
    }
    assert!(followed, "the viewer was never shown the agent's newer tab");

    // Positive control for the silence below: while someone watches an animated page, pictures really do keep coming.
    let mut flowing = 0;
    for _ in 0..8 {
        if followed_viewer_frame(&mut one).await {
            flowing += 1;
        }
    }
    assert!(
        flowing >= 5,
        "only {flowing} of 8 short waits saw a picture while a viewer watched an animated page"
    );

    // Two viewers, one leaves: the stream stays. The last leaves: it stops.
    let other = Viewer::join(addr, &world.target).await;
    for _ in 0..50 {
        if world.hub.has_viewers(TENANT, &world.target) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    one.leave().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        world.hub.has_viewers(TENANT, &world.target),
        "one viewer is still watching"
    );
    other.leave().await;
    for _ in 0..100 {
        if !world.hub.has_viewers(TENANT, &world.target) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !world.hub.has_viewers(TENANT, &world.target),
        "the last viewer left"
    );

    // The page is animated, so a running stream would deliver a picture every moment. With nobody watching, none comes.
    let mut hub_side = world.live.subscribe(TENANT);
    tokio::time::sleep(Duration::from_millis(600)).await;
    drain(&mut hub_side);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        drain(&mut hub_side)
            .iter()
            .all(|(_, frame)| frame["kind"] != "browser.frame"),
        "pictures kept coming with no one watching"
    );

    // And a viewer who returns finds it running again, with numbers that never went backwards.
    let mut returning = Viewer::join(addr, &world.target).await;
    let back = returning
        .frame(15_000)
        .await
        .expect("a returning viewer was shown nothing");
    assert!(
        back["sequence"].as_u64().unwrap_or(0) > a_frame["sequence"].as_u64().unwrap_or(0),
        "frame numbers went backwards: {back} after {a_frame}"
    );
    returning.leave().await;
    drop_pool(&world.pool, &world.name).await;
}

/// Whether a picture arrives within a short wait.
async fn followed_viewer_frame(viewer: &mut Viewer) -> bool {
    viewer.frame(400).await.is_some()
}

#[tokio::test]
async fn a_person_s_special_keys_and_scroll_reach_the_page_and_anything_else_is_refused() {
    let Some(world) = world("wl_keys", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let navigated = ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.keys()}),
    )
    .await;
    let tab = navigated["tab_id"].as_str().unwrap().to_string();
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let (status, taken) = command(
        &world,
        "RequestTakeover",
        51,
        json!({"session_id": session}),
    )
    .await;
    assert_eq!(status, 200, "{taken}");

    // A named key reaches the focused field: the page reports it by name.
    for (n, key) in [(52, "Enter"), (53, "Backspace"), (54, "ArrowDown")] {
        let (status, body) = command(
            &world,
            "SendBrowserInput",
            n,
            json!({"session_id": session, "tab_id": tab, "kind": "key", "key": key}),
        )
        .await;
        assert_eq!(status, 200, "{key}: {body}");
        eventually(&format!("the page saw {key}"), || async {
            site.hits(&format!("/key-{key}")) == 1
        })
        .await;
    }

    // A scroll moves the page: the page reports its own scroll event.
    let (status, body) = command(
        &world,
        "SendBrowserInput",
        55,
        json!({"session_id": session, "tab_id": tab, "kind": "scroll", "x": 100, "y": 100, "delta_x": 0, "delta_y": 400}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    eventually("the page scrolled", || async {
        site.hits("/scrolled") >= 1
    })
    .await;

    // What a person may not send is refused before any worker: printable characters are text, chords are not keys, and a
    // scroll is bounded.
    let before = site.hits("/key-a");
    for (n, bad) in [
        (56, json!({"kind": "key", "key": "a"})),
        (57, json!({"kind": "key", "key": "Shift+a"})),
        (58, json!({"kind": "key", "key": "F12"})),
        (59, json!({"kind": "key"})),
        (
            60,
            json!({"kind": "scroll", "x": 1, "y": 1, "delta_x": 0, "delta_y": 10001}),
        ),
        (
            61,
            json!({"kind": "scroll", "x": -1, "y": 1, "delta_x": 0, "delta_y": 1}),
        ),
        (62, json!({"kind": "scroll", "x": 1, "y": 1, "delta_y": 1})),
    ] {
        let mut params = bad.clone();
        params["session_id"] = json!(session);
        params["tab_id"] = json!(tab);
        let (status, body) = command(&world, "SendBrowserInput", n, params).await;
        assert_eq!(status, 400, "{bad}: {body}");
    }
    assert_eq!(
        site.hits("/key-a"),
        before,
        "a refused key reached the page"
    );

    // The worker does not take the server's word for it: orders sent straight to it, bypassing the server's checks, are
    // refused by the driver itself and the page sees nothing.
    use quansio_machine::gateway::wire::BrowserOrder;
    let scrolled = site.hits("/scrolled");
    for key in ["F12", "a", "Shift+a", "Control+F12", "Shift"] {
        world
            .hub
            .send_order(
                TENANT,
                &world.target,
                BrowserOrder::UserKey {
                    session_id: session.clone(),
                    tab_id: tab.clone(),
                    key: key.to_string(),
                },
            )
            .await;
    }
    for (x, delta_x, delta_y) in [
        (10.0, 0.0, 50_000.0),
        (10.0, 50_000.0, 0.0),
        (10.0, 0.0, -50_000.0),
        (-5.0, 0.0, 10.0),
    ] {
        world
            .hub
            .send_order(
                TENANT,
                &world.target,
                BrowserOrder::UserScroll {
                    session_id: session.clone(),
                    tab_id: tab.clone(),
                    x,
                    y: 10.0,
                    delta_x,
                    delta_y,
                },
            )
            .await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    for key in ["F12", "a", "Shift"] {
        assert_eq!(
            site.hits(&format!("/key-{key}")),
            0,
            "the worker accepted {key}"
        );
    }
    assert_eq!(
        site.hits("/scrolled"),
        scrolled,
        "the worker accepted an out-of-bounds scroll"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn cancelling_a_run_stops_its_command_on_the_worker_and_leaves_another_runs_alone() {
    let Some(world) = world("live_cancel", true).await else {
        return blocked_marker();
    };
    let host = Arc::new(host(&world));
    let root = world._root.path().to_path_buf();
    let start = |run: &'static str, name: &'static str| {
        let host = Arc::clone(&host);
        tokio::spawn(async move {
            host.execute(dispatch(
                run,
                "terminal.exec",
                json!({ "command": format!("echo started > {name}.started; sleep 4; echo done > {name}.done") }),
            ))
            .await
        })
    };
    let cancelled = start(RUN_A, "a");
    let other = start(RUN_B, "b");
    eventually("both commands started", || {
        let root = root.clone();
        async move { root.join("a.started").exists() && root.join("b.started").exists() }
    })
    .await;

    // A person cancels run A: the command it is running on the worker is stopped, and run B's is not.
    let (status, body) = command(
        &world,
        "CancelRun",
        900,
        json!({ "run_id": RUN_A, "generation": 1 }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    match tokio::time::timeout(Duration::from_secs(2), cancelled)
        .await
        .expect("the cancelled call returns at once")
        .unwrap()
    {
        Ok(HostOutcome::Unknown { detail }) => assert!(detail.contains("cancel"), "{detail}"),
        other => panic!("a command stopped part-way settles as unknown, got {other:?}"),
    }
    match other.await.unwrap() {
        Ok(HostOutcome::Success { .. }) => {}
        other => panic!("the other run's command was disturbed: {other:?}"),
    }
    assert!(root.join("b.done").exists());
    assert!(
        !root.join("a.done").exists(),
        "the cancelled command finished its work"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn a_person_s_right_click_double_click_drag_and_shortcut_reach_the_page_and_malformed_ones_do_not(
) {
    let Some(world) = world("wl_pointer", true).await else {
        return blocked_marker();
    };
    let (site, host) = (Site::start().await, host(&world));
    let navigated = ok(
        &host,
        RUN_A,
        "browser.navigate",
        json!({"session_id": "x", "url": site.pointer()}),
    )
    .await;
    let tab = navigated["tab_id"].as_str().unwrap().to_string();
    let session = sessions(&world.pool, &world.target).await.remove(0).0;
    let (status, taken) = command(
        &world,
        "RequestTakeover",
        71,
        json!({"session_id": session}),
    )
    .await;
    assert_eq!(status, 200, "{taken}");

    let input = |n: u32, params: Value| {
        let (world, session, tab) = (&world, session.clone(), tab.clone());
        async move {
            let mut params = params;
            params["session_id"] = json!(session);
            params["tab_id"] = json!(tab);
            command(world, "SendBrowserInput", n, params).await
        }
    };
    for (n, params, what, path) in [
        (
            72,
            json!({"kind": "click", "x": 100, "y": 100, "button": "right"}),
            "a right click",
            "/ctx",
        ),
        (
            73,
            json!({"kind": "click", "x": 100, "y": 100, "click_count": 2}),
            "a double click",
            "/dbl",
        ),
        (
            74,
            json!({"kind": "drag", "from_x": 50, "from_y": 200, "to_x": 400, "to_y": 220}),
            "a drag",
            "/drag",
        ),
        (
            75,
            json!({"kind": "click", "x": 100, "y": 100, "modifiers": ["Shift"]}),
            "a click with Shift held",
            "/shift-click",
        ),
    ] {
        let (status, body) = input(n, params).await;
        assert_eq!(status, 200, "{what}: {body}");
        eventually(&format!("the page saw {what}"), || async {
            site.hits(path) >= 1
        })
        .await;
    }
    // A plain click does not look like a shift-click, and a drag is not a click: the page told them apart.
    assert_eq!(site.hits("/shift-click"), 1);

    // A shortcut reaches the focused field as a chord (Meta on macOS pages, Control elsewhere: either sets the flag the page
    // checks).
    let (status, body) = input(76, json!({"kind": "key", "key": "Control+a"})).await;
    assert_eq!(status, 200, "{body}");
    eventually("the page saw the shortcut", || async {
        site.hits("/select-all") >= 1
    })
    .await;

    // What a person may not send is refused before any worker.
    let before = (site.hits("/ctx"), site.hits("/drag"));
    for (n, bad) in [
        (
            77,
            json!({"kind": "click", "x": 1, "y": 1, "button": "back"}),
        ),
        (
            78,
            json!({"kind": "click", "x": 1, "y": 1, "click_count": 4}),
        ),
        (
            79,
            json!({"kind": "click", "x": 1, "y": 1, "modifiers": ["Hyper"]}),
        ),
        (
            80,
            json!({"kind": "click", "x": 1, "y": 1, "modifiers": ["Shift", "Shift"]}),
        ),
        (
            81,
            json!({"kind": "click", "x": 1, "y": 1, "modifiers": "Shift"}),
        ),
        (
            82,
            json!({"kind": "drag", "from_x": 1, "from_y": 1, "to_x": 30000, "to_y": 1}),
        ),
        (
            83,
            json!({"kind": "drag", "from_x": 1, "from_y": 1, "to_x": 5}),
        ),
        (84, json!({"kind": "key", "key": "Control+Control+a"})),
        (85, json!({"kind": "key", "key": "Shift+Control+a"})),
    ] {
        let (status, body) = input(n, bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {body}");
    }

    // The worker does not take the server's word for it either: orders sent straight to it are refused by the driver.
    use quansio_machine::gateway::wire::BrowserOrder;
    for order in [
        BrowserOrder::UserClick {
            session_id: session.clone(),
            tab_id: tab.clone(),
            x: 100.0,
            y: 100.0,
            button: "back".to_string(),
            click_count: 1,
            modifiers: Vec::new(),
        },
        BrowserOrder::UserClick {
            session_id: session.clone(),
            tab_id: tab.clone(),
            x: 100.0,
            y: 100.0,
            button: "right".to_string(),
            click_count: 9,
            modifiers: Vec::new(),
        },
        BrowserOrder::UserDrag {
            session_id: session.clone(),
            tab_id: tab.clone(),
            from_x: 1.0,
            from_y: 1.0,
            to_x: f64::NAN,
            to_y: 1.0,
            modifiers: Vec::new(),
        },
        BrowserOrder::UserDrag {
            session_id: session.clone(),
            tab_id: tab.clone(),
            from_x: 1.0,
            from_y: 1.0,
            to_x: 400.0,
            to_y: 1.0,
            modifiers: vec!["Fn".to_string()],
        },
    ] {
        world.hub.send_order(TENANT, &world.target, order).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        (site.hits("/ctx"), site.hits("/drag")),
        before,
        "the worker accepted an input it should have refused"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test]
async fn only_a_reader_of_the_targets_workspace_may_watch_its_channel() {
    let Some(world) = world("wl_channel_auth", true).await else {
        return blocked_marker();
    };
    // A member of the tenant who belongs to no workspace of it, and a stranger to the tenant.
    let outsider = "usr_01J8Z3K6F1N8VQ2X5W9YQZTSXD";
    let mut tx = world.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, primary_email, display_name) VALUES ($1, 'o@example.com', 'o')",
    )
    .bind(outsider)
    .execute(&mut *tx)
    .await
    .unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ('tm_outsider', $1, $2, 'member', 'active')")
        .bind(TENANT)
        .bind(outsider)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let addr = serve_http(&world).await;

    // The refusal is typed, and a refused socket starts nothing: no stream is asked for on the target's behalf.
    let mut refused = Viewer::join_as(addr, outsider, &world.target).await;
    let answer = refused.next_text(5_000).await.expect("an answer");
    assert_eq!(answer["code"], "SCOPE_FORBIDDEN", "{answer}");
    assert!(
        !world.hub.has_viewers(TENANT, &world.target),
        "a refused socket became a viewer"
    );
    // A target that does not exist is refused the same way, so the answer does not say which targets there are.
    let mut missing = Viewer::join_as(addr, USER, "tgt_01J8Z3K6F1N8VQ2X5W9YNZSVCH").await;
    let answer = missing.next_text(5_000).await.expect("an answer");
    assert_eq!(answer["code"], "SCOPE_FORBIDDEN", "{answer}");

    // A reader of the workspace is admitted and counted.
    let reader = Viewer::join(addr, &world.target).await;
    for _ in 0..50 {
        if world.hub.has_viewers(TENANT, &world.target) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        world.hub.has_viewers(TENANT, &world.target),
        "the workspace's reader was not admitted"
    );
    reader.leave().await;
    drop_pool(&world.pool, &world.name).await;
}

/// An executor that runs nothing and remembers the decisions it was handed.
struct DecisionLog(Arc<std::sync::Mutex<Vec<(String, String, String)>>>);

#[async_trait]
impl quansio_qworkerd::host::ToolExecutor for DecisionLog {
    async fn run(
        &self,
        _: &quansio_machine::gateway::ActionEnvelope,
        _: &mut quansio_qworkerd::host::OutputSink,
    ) -> Result<quansio_qworkerd::host::ToolOutput, quansio_qworkerd::host::HostFailure> {
        Ok(quansio_qworkerd::host::ToolOutput::new())
    }

    async fn browser_decision(
        &self,
        receipt: &quansio_machine::control::browser::DecisionReceipt,
    ) -> Result<(), String> {
        self.0.lock().unwrap().push((
            receipt.kind().as_str().to_string(),
            receipt.subject().to_string(),
            receipt.decision().as_str().to_string(),
        ));
        Ok(())
    }
}

#[tokio::test]
async fn a_decision_made_while_the_worker_was_away_is_told_to_it_when_it_attaches() {
    let Some(world) = world("wl_replay", false).await else {
        return blocked_marker();
    };
    // A session whose person decided a certificate and a permission while no worker was attached (and changed their mind on
    // the permission): the decisions are on the session.
    let session = "bsn_01J8Z3K6F1N8VQ2X5W9YRPLY01";
    let mut tx = world.pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    quansio_machine::control::browser::BrowserSessionStore::open(
        &mut tx,
        TENANT,
        session,
        &world.target,
        Some(RUN_A),
        None,
    )
    .await
    .unwrap();
    let decisions = json!({ "decisions": [
        { "kind": "certificate", "tab_id": "T1", "subject": "crt_1", "decision": "allow", "decided_by": USER, "decided_at": "2026-10-20T00:00:00Z" },
        { "kind": "permission", "tab_id": "T1", "subject": "http://127.0.0.1:1|camera", "decision": "allow", "decided_by": USER, "decided_at": "2026-10-20T00:00:01Z" },
        { "kind": "permission", "tab_id": "T1", "subject": "http://127.0.0.1:1|camera", "decision": "deny", "decided_by": USER, "decided_at": "2026-10-20T00:00:02Z" },
    ]});
    sqlx::query("UPDATE browser_sessions SET guard_state = $1 WHERE id = $2")
        .bind(decisions)
        .bind(session)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (socket, target, log) = (
        world._dir.path().join("w.sock"),
        world.target.clone(),
        seen.clone(),
    );
    tokio::spawn(async move {
        let host = Arc::new(WorkerHost::new(DecisionLog(log), UnattachedEvidence));
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (reader, writer) = stream.into_split();
        let _ = serve(
            host,
            TENANT,
            &target,
            Duration::from_secs(3600),
            reader,
            writer,
        )
        .await;
    });
    eventually(
        "the worker was told what was decided while it was away",
        || async { seen.lock().unwrap().len() == 3 },
    )
    .await;
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (
                "certificate".to_string(),
                "crt_1".to_string(),
                "allow".to_string()
            ),
            (
                "permission".to_string(),
                "http://127.0.0.1:1|camera".to_string(),
                "allow".to_string()
            ),
            (
                "permission".to_string(),
                "http://127.0.0.1:1|camera".to_string(),
                "deny".to_string()
            ),
        ],
        "oldest first, so the last word on a subject is the one the worker ends with"
    );
    drop_pool(&world.pool, &world.name).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_registers_a_worker_target_once_and_a_worker_can_then_attach_to_it() {
    let Some(world) = world("wl_provision", false).await else {
        return blocked_marker();
    };
    // A member of the tenant with no part in the workspace may not register machines for it.
    let outsider = "usr_01J8Z3K6F1N8VQ2X5W9YQZTSXE";
    let mut tx = world.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, primary_email, display_name) VALUES ($1, 'p@example.com', 'p')",
    )
    .bind(outsider)
    .execute(&mut *tx)
    .await
    .unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ('tm_prov', $1, $2, 'member', 'active')")
        .bind(TENANT)
        .bind(outsider)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let ask = json!({"workspace_id": WORKSPACE, "substrate": "customer_private_worker"});
    let (status, body) = command_as(&world, outsider, "ProvisionTarget", 600, ask.clone()).await;
    assert_eq!(status, 403, "{body}");
    // A person who can read the workspace and author work in it still may not register machines for it.
    for (role, user) in [
        ("viewer", "usr_01J8Z3K6F1N8VQ2X5W9YQZTSXF"),
        ("editor", "usr_01J8Z3K6F1N8VQ2X5W9YQZTSXG"),
    ] {
        let mut tx = world.pool.begin().await.unwrap();
        sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, $3)")
            .bind(user)
            .bind(format!("{role}@example.com"))
            .bind(role)
            .execute(&mut *tx)
            .await
            .unwrap();
        quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, 'member', 'active')")
            .bind(format!("tm_{role}"))
            .bind(TENANT)
            .bind(user)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ($1, $2, $3, $4, $5, 'active')")
            .bind(format!("wm_{role}"))
            .bind(TENANT)
            .bind(WORKSPACE)
            .bind(user)
            .bind(role)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let (status, body) = command_as(&world, user, "ProvisionTarget", 610, ask.clone()).await;
        assert_eq!(status, 403, "{role} registered a machine: {body}");
    }

    // Other substrates have flows of their own and are not answered here.
    for (n, bad) in [
        (
            601,
            json!({"workspace_id": WORKSPACE, "substrate": "cloud_microvm"}),
        ),
        (602, json!({"workspace_id": WORKSPACE})),
        (603, json!({"substrate": "customer_private_worker"})),
        (
            604,
            json!({"workspace_id": WORKSPACE, "substrate": "customer_private_worker", "target_class": "isolated_task_runtime"}),
        ),
    ] {
        let (status, body) = command(&world, "ProvisionTarget", n, bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {body}");
    }

    // The workspace's administrator registers one, ready to be leased; asking again returns it. (The harness's own target
    // is a worker target too, so it is the one that exists: no second is made.)
    let (status, first) = command(&world, "ProvisionTarget", 605, ask.clone()).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["result"]["created"], false, "{first}");
    assert_eq!(first["result"]["target_id"], world.target.as_str());

    // In a workspace with none, it makes exactly one, even when asked at once.
    let fresh = "ws_01J8Z3K6F1N8VQ2X5W9YPRVN01";
    let mut tx = world.pool.begin().await.unwrap();
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'fresh')")
        .bind(fresh)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let ask = json!({"workspace_id": fresh, "substrate": "customer_private_worker"});
    // Many asks at once, on a runtime with threads, in a few workspaces: each workspace ends with exactly one target.
    let mut spare = Vec::new();
    for n in 0..2u32 {
        let id = format!("ws_01J8Z3K6F1N8VQ2X5W9YPRVN0{}", n + 2);
        let mut tx = world.pool.begin().await.unwrap();
        quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
            .await
            .unwrap();
        sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'spare')")
            .bind(&id)
            .bind(TENANT)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        spare.push(id);
    }
    for (w, workspace) in spare.iter().enumerate() {
        let mut asks = Vec::new();
        for n in 0..8u32 {
            let router = world.router.clone();
            let params = json!({"workspace_id": workspace, "substrate": "customer_private_worker"});
            asks.push(tokio::spawn(async move {
                command_on(
                    router,
                    USER,
                    "ProvisionTarget",
                    700 + 10 * u32::try_from(w).unwrap() + n,
                    params,
                )
                .await
            }));
        }
        let mut targets = std::collections::BTreeSet::new();
        for ask in asks {
            let (status, body) = ask.await.unwrap();
            assert_eq!(status, 200, "{body}");
            targets.insert(body["result"]["target_id"].as_str().unwrap().to_string());
        }
        assert_eq!(targets.len(), 1, "{workspace}: eight asks made {targets:?}");
        let mut tx = world.pool.begin().await.unwrap();
        quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM execution_targets WHERE tenant_id = $1 AND workspace_id = $2",
        )
        .bind(TENANT)
        .bind(workspace)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(rows, 1, "{workspace}: {rows} target rows");
    }
    let (one, two) = tokio::join!(
        command(&world, "ProvisionTarget", 606, ask.clone()),
        command(&world, "ProvisionTarget", 607, ask.clone()),
    );
    assert_eq!((one.0, two.0), (200, 200), "{one:?} {two:?}");
    assert_eq!(
        one.1["result"]["target_id"], two.1["result"]["target_id"],
        "two asks made two targets"
    );
    assert_eq!(
        [&one.1["result"]["created"], &two.1["result"]["created"]]
            .iter()
            .filter(|created| created.as_bool() == Some(true))
            .count(),
        1
    );
    let target = one.1["result"]["target_id"].as_str().unwrap().to_string();
    assert_eq!(
        one.1["result"]["status"]
            .as_str()
            .map(str::to_string)
            .or(two.1["result"]["status"].as_str().map(str::to_string))
            .as_deref(),
        Some("READY")
    );
    assert_eq!(
        one.1["result"]["worker_socket"],
        world
            ._dir
            .path()
            .join("w.sock")
            .display()
            .to_string()
            .as_str()
    );
    assert_eq!(one.1["result"]["attached"], false);

    // A worker given what the answer names attaches to the target, and the hub holds its lease.
    let (socket, tenant, name) = (world._dir.path().join("w.sock"), TENANT, target.clone());
    tokio::spawn(async move {
        let host = Arc::new(WorkerHost::new(
            DecisionLog(Arc::default()),
            UnattachedEvidence,
        ));
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (reader, writer) = stream.into_split();
        let _ = serve(
            host,
            tenant,
            &name,
            Duration::from_secs(3600),
            reader,
            writer,
        )
        .await;
    });
    eventually("the worker attached to the registered target", || async {
        world
            .hub
            .links()
            .iter()
            .any(|link| link.target_id == target)
    })
    .await;
    let (status, again) = command(&world, "ProvisionTarget", 608, ask).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        (
            again["result"]["created"].as_bool(),
            again["result"]["attached"].as_bool()
        ),
        (Some(false), Some(true))
    );
    drop_pool(&world.pool, &world.name).await;
}
