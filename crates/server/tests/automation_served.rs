//! ADP-007 recovery: the real `quansio-server` process, killed with SIGKILL and relaunched over the same database.
//!
//! Nothing here is in-process. The deployable binary serves HTTP on a loopback port; the Python intelligence plane runs as its own
//! process; the model is a loopback OpenAI-compatible provider hosted by the test (the wire is real, the replies are the test's,
//! so this is not real-model evidence).
//!
//! * a hook delivery is accepted and its fire claimed while the background loop is off; the process is killed; a relaunched process
//!   executes the fire exactly once, and the replayed delivery is a duplicate after the restart too;
//! * a hook-fired workflow script is killed (SIGKILL) while one of its workers is waiting on the model, and a relaunched process
//!   finishes it from its journal: every call that had finished is not asked of the model again, no call is journalled twice, the
//!   fire and the run exist once, and replaying the delivery fires nothing.
//!
//! The call that was in flight when the process died had not finished, so the relaunch may ask the model for it once more: a model
//! call is a read, not an external effect. That count is asserted at most two and printed.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `uv` (for the plane), the built `quansio-server` (Cargo provides it to this test).
//! Without a database the tests print `BLOCKED_EXTERNAL` and return.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use quansio_server::control::schema;
use quansio_server::notify::webhooks::sign;
use quansio_server::scheduler::workflow::journal::{Journal, JournalStore};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::Notify;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YASV001";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YASV001";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YASV001";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YASV001";
/// Thirty-two zero bytes, base64: a development master key for the process's secret broker.
const KEY_FILE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// A loopback OpenAI-compatible provider. It tells the calls of the script apart by what the prompt says, counts how often each
/// reached it, and can hold one call open until told to answer (the process dies while that worker waits).
struct Provider {
    hits: Mutex<BTreeMap<String, u32>>,
    hold: AtomicBool,
    held: Notify,
    release: Notify,
}

fn texts(value: &Value, out: &mut String) {
    match value {
        Value::String(text) => {
            out.push_str(text);
            out.push('\n');
        }
        Value::Array(items) => items.iter().for_each(|item| texts(item, out)),
        Value::Object(map) => map.values().for_each(|item| texts(item, out)),
        _ => {}
    }
}

fn key_of(prompt: &str) -> String {
    if prompt.contains("Combine the reviews") {
        "summary".to_string()
    } else if prompt.contains("List what changed") {
        "collect".to_string()
    } else {
        ["alpha", "beta", "gamma"]
            .into_iter()
            .find(|word| prompt.contains(word))
            .map_or_else(|| "other".to_string(), |word| format!("each:{word}"))
    }
}

fn reply_for(key: &str) -> String {
    match key {
        "collect" => json!({ "items": ["alpha", "beta", "gamma"] }).to_string(),
        "summary" => json!({ "summary": "all three reviewed" }).to_string(),
        other => json!({ "ok": true, "note": format!("reviewed {other}") }).to_string(),
    }
}

async fn completions(State(provider): State<Arc<Provider>>, Json(body): Json<Value>) -> Response {
    let mut prompt = String::new();
    texts(&body["messages"], &mut prompt);
    let key = key_of(&prompt);
    let nth = {
        let mut hits = provider.hits.lock().expect("hits");
        let entry = hits.entry(key.clone()).or_insert(0);
        *entry += 1;
        *entry
    };
    if key == "each:beta" && nth == 1 && provider.hold.load(Ordering::SeqCst) {
        provider.held.notify_one();
        provider.release.notified().await;
    }
    let content = reply_for(&key);
    let model = body["model"].clone();
    let usage = json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 });
    if body["stream"] == json!(true) {
        let frame = |delta: Value, finish: Value| {
            json!({ "id": "chatcmpl-adp007", "object": "chat.completion.chunk", "model": model,
                    "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
        };
        let chunks = [
            frame(json!({ "role": "assistant", "content": "" }), Value::Null),
            frame(json!({ "content": content }), Value::Null),
            frame(json!({}), json!("stop")),
            json!({ "id": "chatcmpl-adp007", "object": "chat.completion.chunk", "model": model, "choices": [], "usage": usage }),
        ];
        let mut wire = String::new();
        for chunk in chunks {
            wire.push_str(&format!("data: {chunk}\n\n"));
        }
        wire.push_str("data: [DONE]\n\n");
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(wire))
            .expect("response");
    }
    let whole = json!({
        "id": "chatcmpl-adp007", "object": "chat.completion", "model": model,
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }],
        "usage": usage,
    });
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(whole.to_string()))
        .expect("response")
}

async fn start_provider(hold: bool) -> (Arc<Provider>, String) {
    let provider = Arc::new(Provider {
        hits: Mutex::new(BTreeMap::new()),
        hold: AtomicBool::new(hold),
        held: Notify::new(),
        release: Notify::new(),
    });
    let router = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(Arc::clone(&provider));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    (provider, url)
}

impl Provider {
    fn hits_of(&self, key: &str) -> u32 {
        self.hits
            .lock()
            .expect("hits")
            .get(key)
            .copied()
            .unwrap_or(0)
    }
}

/// One `quansio-server` process.
struct Server {
    child: Child,
    base: String,
    log: PathBuf,
}

impl Server {
    /// SIGKILL and reap: no shutdown hook runs, nothing is flushed.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Launch<'a> {
    database_url: &'a str,
    dir: &'a Path,
    tag: &'a str,
    intelligence: Option<&'a str>,
    background: bool,
}

async fn launch(options: &Launch<'_>) -> Server {
    let key_file = options.dir.join("master.key");
    std::fs::write(&key_file, KEY_FILE).expect("key file");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("port")
        .local_addr()
        .expect("addr")
        .port();
    let log = options.dir.join(format!("server-{}.log", options.tag));
    let mut command = Command::new(env!("CARGO_BIN_EXE_quansio-server"));
    command
        .env("QUANSIO_DATABASE_URL", options.database_url)
        .env("QUANSIO_BIND", format!("127.0.0.1:{port}"))
        .env("QUANSIO_TRUST_SCOPE_HEADERS", "1")
        .env("QUANSIO_SECRETS_KEY_FILE", &key_file)
        .env("QUANSIO_APPROVAL_SIGNING_KEY", "automation-served-key")
        .env("QUANSIO_SCHEDULER_INTERVAL_MS", "100")
        .env_remove("QUANSIO_INTELLIGENCE_ENDPOINT")
        .env_remove("QUANSIO_MIGRATION_DATABASE_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(&log).expect("log")));
    if !options.background {
        command.env("QUANSIO_SCHEDULER", "off");
    } else {
        command.env_remove("QUANSIO_SCHEDULER");
    }
    if let Some(endpoint) = options.intelligence {
        command.env("QUANSIO_INTELLIGENCE_ENDPOINT", endpoint);
    }
    let child = command.spawn().expect("spawn quansio-server");
    let mut server = Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
        log,
    };
    // A freshly built binary can take a while on its first launch (the platform scans it); allow it.
    let deadline = Instant::now() + Duration::from_secs(180);
    let http = reqwest::Client::new();
    loop {
        if let Ok(response) = http.get(format!("{}/v1/health", server.base)).send().await {
            if response.status().is_success() {
                return server;
            }
        }
        if let Some(status) = server.child.try_wait().expect("try_wait") {
            panic!("quansio-server exited with {status}: {}", server.stderr());
        }
        assert!(
            Instant::now() < deadline,
            "quansio-server did not become healthy: {}",
            server.stderr()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct Fixture {
    name: String,
    pool: PgPool,
    url: String,
    dir: tempfile::TempDir,
    http: reqwest::Client,
}

async fn fixture(prefix: &str) -> Option<Fixture> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YASV001', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([{ "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    Some(Fixture {
        url: common::scratch_url(&name),
        name,
        pool,
        dir: tempfile::tempdir().expect("dir"),
        http: reqwest::Client::new(),
    })
}

impl Fixture {
    async fn command(&self, server: &Server, name: &str, id: u32, params: Value) -> (u16, Value) {
        let response = self
            .http
            .post(format!("{}/v1/commands/{name}", server.base))
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", OWNER)
            .json(&json!({ "command_id": format!("cmd_01J8Z3K6F1N8VQ2X5W9YA{id:05}"), "params": params }))
            .send()
            .await
            .expect("post");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn deliver(
        &self,
        server: &Server,
        path: &str,
        secret: &str,
        timestamp: i64,
        body: &str,
        delivery_id: &str,
    ) -> (u16, Value) {
        let response = self
            .http
            .post(format!("{}{path}", server.base))
            .header("content-type", "application/json")
            .header(
                "quansio-signature",
                sign(secret.as_bytes(), timestamp, body.as_bytes()),
            )
            .header("quansio-delivery-id", delivery_id)
            .body(body.to_string())
            .send()
            .await
            .expect("deliver");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn count(&self, sql: &str, bind: &str) -> i64 {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let n: i64 = sqlx::query_scalar(sql)
            .bind(TENANT)
            .bind(bind)
            .fetch_one(&mut *tx)
            .await
            .expect("count");
        tx.commit().await.expect("commit");
        n
    }

    async fn fires(&self, routine: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM routine_fires WHERE tenant_id = $1 AND routine_id = $2",
            routine,
        )
        .await
    }

    async fn fired(&self, routine: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM routine_fires WHERE tenant_id = $1 AND routine_id = $2 AND status = 'fired'",
            routine,
        )
        .await
    }

    async fn runs(&self, routine: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM runs r JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
             WHERE r.tenant_id = $1 AND r.trigger_kind = 'routine' AND m.author_id = 'routine:' || $2",
            routine,
        )
        .await
    }

    async fn script_run(&self) -> Option<String> {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let run: Option<String> = sqlx::query_scalar(
            "SELECT run_id FROM evidence WHERE tenant_id = $1 AND captured_by_id = 'workflow' LIMIT 1",
        )
        .bind(TENANT)
        .fetch_optional(&mut *tx)
        .await
        .expect("run");
        tx.commit().await.expect("commit");
        run
    }

    async fn script_runs(&self) -> i64 {
        self.count(
            "SELECT count(DISTINCT run_id) FROM evidence WHERE tenant_id = $1 AND captured_by_id = 'workflow' AND $2 <> ''",
            "x",
        )
        .await
    }

    async fn journal(&self, run: &str) -> Journal {
        JournalStore::new(self.pool.clone(), TENANT)
            .load(run)
            .await
            .expect("journal")
    }

    /// What the database holds about the run, for a failure message.
    async fn dump(&self) -> String {
        let mut out = String::new();
        if let Some(run) = self.script_run().await {
            for entry in self.journal(&run).await.entries {
                out.push_str(&format!(
                    "journal {} {} {}\n",
                    entry["entry"], entry["key"], entry["status"]
                ));
            }
        }
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT r.id, r.status, coalesce(t.status, '-') FROM runs r LEFT JOIN turns t ON t.tenant_id = r.tenant_id AND t.run_id = r.id \
             WHERE r.tenant_id = $1 ORDER BY r.created_at",
        )
        .bind(TENANT)
        .fetch_all(&mut *tx)
        .await
        .expect("runs");
        for (id, run, turn) in rows {
            out.push_str(&format!("run {id} {run} turn {turn}\n"));
        }
        tx.commit().await.expect("commit");
        let sessions: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT coalesce(state, '-'), coalesce(wait_event, '-'), left(coalesce(query, '-'), 120) FROM pg_stat_activity \
             WHERE datname = current_database() AND pid <> pg_backend_pid() AND state <> 'idle'",
        )
        .fetch_all(&self.pool)
        .await
        .expect("sessions");
        for (state, wait, query) in sessions {
            out.push_str(&format!("session {state} {wait} {query}\n"));
        }
        out
    }

    async fn wait<F, Fut>(&self, what: &str, server: &Server, mut check: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !check().await {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; state:\n{}\nserver log:\n{}",
                self.dump().await,
                server.stderr()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn create_hook(
        &self,
        server: &Server,
        id: u32,
        extra: Value,
    ) -> (String, String, String) {
        let mut params = json!({
            "workspace_id": WORKSPACE, "title": "On pull request", "prompt": "Review {{payload}}",
            "trigger": { "kind": "webhook" },
        });
        if let (Value::Object(target), Value::Object(more)) = (&mut params, extra) {
            target.extend(more);
        }
        let (status, created) = self.command(server, "CreateRoutine", id, params).await;
        assert_eq!(status, 200, "{created}");
        let result = &created["result"];
        (
            result["id"].as_str().expect("id").to_string(),
            result["hook"]["path"].as_str().expect("path").to_string(),
            result["secret"].as_str().expect("secret").to_string(),
        )
    }

    async fn finish(self) {
        common::drop_pool(&self.pool, &self.name).await;
    }
}

#[tokio::test]
async fn a_hook_fire_claimed_before_a_kill_runs_once_after_the_relaunch_and_its_replay_is_a_duplicate(
) {
    let Some(f) = fixture("served_hook").await else {
        common::blocked_marker();
        return;
    };
    // The first process serves HTTP with its background loop off: it accepts the delivery and claims the fire, and never runs it.
    let mut first = launch(&Launch {
        database_url: &f.url,
        dir: f.dir.path(),
        tag: "1",
        intelligence: None,
        background: false,
    })
    .await;
    let (routine, path, secret) = f.create_hook(&first, 1, json!({})).await;
    let body = r#"{"action":"opened","number":7}"#;
    let ts = chrono::Utc::now().timestamp();
    let (status, accepted) = f.deliver(&first, &path, &secret, ts, body, "evt-1").await;
    assert_eq!(
        (status, accepted["status"].as_str()),
        (202, Some("accepted")),
        "{accepted}"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(f.fires(&routine).await, 1);
    assert_eq!(f.fired(&routine).await, 0, "nothing executed the fire");
    assert_eq!(f.runs(&routine).await, 0);

    // SIGKILL, then a process with the background loop on.
    first.kill();
    let second = launch(&Launch {
        database_url: &f.url,
        dir: f.dir.path(),
        tag: "2",
        intelligence: None,
        background: true,
    })
    .await;
    f.wait("the claimed fire to execute", &second, || async {
        f.fired(&routine).await == 1
    })
    .await;
    assert_eq!(f.runs(&routine).await, 1, "the fire ran once");
    assert_eq!(f.fires(&routine).await, 1);

    // The replay, after the restart: a duplicate, and another process pass finds nothing to run.
    let (status, again) = f.deliver(&second, &path, &secret, ts, body, "evt-1").await;
    assert_eq!(
        (status, again["status"].as_str()),
        (200, Some("duplicate")),
        "{again}"
    );
    let (status, relabelled) = f
        .deliver(&second, &path, &secret, ts, body, "evt-other")
        .await;
    assert_eq!(
        (status, relabelled["status"].as_str()),
        (200, Some("duplicate")),
        "{relabelled}"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(f.fires(&routine).await, 1);
    assert_eq!(f.runs(&routine).await, 1, "still one run");
    drop(second);
    f.finish().await;
}

fn served_script() -> Value {
    let item = json!({
        "type": "object", "additionalProperties": false, "required": ["ok", "note"],
        "properties": { "ok": { "type": "boolean" }, "note": { "type": "string", "maxLength": 200 } }
    });
    json!({
        "version": 1, "max_agent_calls": 8, "estimated_agent_calls": 5,
        "steps": [
            { "id": "collect", "kind": "agent", "prompt": "List what changed.\n{{payload}}", "effort": "low",
              "output_schema": { "type": "object", "additionalProperties": false, "required": ["items"],
                  "properties": { "items": { "type": "array", "maxItems": 5, "items": { "type": "string" } } } } },
            { "id": "each", "kind": "parallel", "over": { "step": "collect", "path": "/items" }, "max_items": 5,
              "agent": { "prompt": "Review item {{index}}: {{item}}", "output_schema": item, "effort": "medium" } },
            { "id": "summary", "kind": "reduce", "of": "each", "prompt": "Combine the reviews: {{results}}", "effort": "high",
              "output_schema": { "type": "object", "additionalProperties": false, "required": ["summary"],
                  "properties": { "summary": { "type": "string", "maxLength": 500 } } } }
        ]
    })
}

#[tokio::test]
async fn a_killed_process_resumes_a_hook_fired_script_from_its_journal_without_repeating_a_completed_call(
) {
    let Some(f) = fixture("served_script").await else {
        common::blocked_marker();
        return;
    };
    let (provider, provider_url) = start_provider(true).await;
    let Some(plane) = common::intelligence::start_with_provider(Some(&provider_url)).await else {
        f.finish().await;
        return;
    };
    let mut first = launch(&Launch {
        database_url: &f.url,
        dir: f.dir.path(),
        tag: "1",
        intelligence: Some(&plane.endpoint),
        background: true,
    })
    .await;
    let (routine, path, secret) = f
        .create_hook(&first, 1, json!({ "workflow": served_script() }))
        .await;
    let body = r#"{"action":"opened","number":7}"#;
    let ts = chrono::Utc::now().timestamp();
    let (status, accepted) = f.deliver(&first, &path, &secret, ts, body, "evt-1").await;
    assert_eq!(
        (status, accepted["status"].as_str()),
        (202, Some("accepted")),
        "{accepted}"
    );

    // The script runs: the collector, then the three item workers at once. The worker of `beta` waits on the model and never
    // gets its answer from this process. When the others have been recorded, the process is killed.
    tokio::time::timeout(Duration::from_secs(120), provider.held.notified())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the beta worker never reached the model; server log:\n{}",
                first.stderr()
            )
        });
    f.wait("the other items to be journalled", &first, || async {
        match f.script_run().await {
            Some(run) => {
                let journal = f.journal(&run).await;
                ["collect", "each#0", "each#2"]
                    .iter()
                    .all(|key| journal.finished(key).is_some())
            }
            None => false,
        }
    })
    .await;
    let run = f.script_run().await.expect("script run");
    let before = f.journal(&run).await;
    assert!(before.ended().is_none());
    assert!(
        before.finished("each#1").is_none(),
        "the held call had not finished"
    );
    assert!(
        before.started("each#1"),
        "and it was in flight: {:#?}",
        before.entries
    );
    let finished_before: Vec<String> = before
        .entries
        .iter()
        .filter(|e| e["entry"] == "call_finished")
        .filter_map(|e| e["key"].as_str().map(str::to_string))
        .collect();
    first.kill();

    // The relaunched process takes the script up where its journal left it. The provider answers the resumed call at once.
    provider.hold.store(false, Ordering::SeqCst);
    // The provider's request from the dead process is answered into the void, as a provider answers a client that went away.
    provider.release.notify_one();
    let second = launch(&Launch {
        database_url: &f.url,
        dir: f.dir.path(),
        tag: "2",
        intelligence: Some(&plane.endpoint),
        background: true,
    })
    .await;
    f.wait("the script to end", &second, || async {
        f.journal(&run).await.ended().is_some()
    })
    .await;
    let journal = f.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("succeeded")),
        "{:#?}",
        journal.entries
    );

    // No call that had finished was asked of the model again.
    assert_eq!(finished_before.len(), 3, "{finished_before:?}");
    for (journal_key, provider_key) in [
        ("collect", "collect"),
        ("each#0", "each:alpha"),
        ("each#2", "each:gamma"),
    ] {
        assert!(finished_before.iter().any(|key| key == journal_key));
        assert_eq!(
            provider.hits_of(provider_key),
            1,
            "{journal_key} finished before the kill and reached the model once: {:?}",
            provider.hits.lock().expect("hits")
        );
    }
    assert_eq!(provider.hits_of("summary"), 1, "the reducer ran once");
    let beta = provider.hits_of("each:beta");
    println!("the call in flight at the kill reached the model {beta} times (the killed process's request and the resumed one)");
    assert!(
        (1..=2).contains(&beta),
        "the unfinished call is asked at most once more: {beta}"
    );

    // The journal: every call finished once and was started once.
    for key in ["collect", "each#0", "each#1", "each#2", "summary"] {
        let finished = journal
            .entries
            .iter()
            .filter(|e| e["entry"] == "call_finished" && e["key"] == key)
            .count();
        let started = journal
            .entries
            .iter()
            .filter(|e| e["entry"] == "call_started" && e["key"] == key)
            .count();
        assert_eq!((started, finished), (1, 1), "{key}: {:#?}", journal.entries);
    }
    assert_eq!(journal.calls_used(), 5);

    // One fire, one run; the replay of the delivery fires nothing, before or after the work.
    assert_eq!(f.fires(&routine).await, 1);
    // The script run and one worker run for each of its five calls: six runs, authored by the routine, and one script run.
    assert_eq!(f.runs(&routine).await, 6);
    assert_eq!(f.script_runs().await, 1);
    let (status, again) = f.deliver(&second, &path, &secret, ts, body, "evt-1").await;
    assert_eq!(
        (status, again["status"].as_str()),
        (200, Some("duplicate")),
        "{again}"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(f.fires(&routine).await, 1);
    assert_eq!(f.runs(&routine).await, 6, "the replay started nothing");
    assert_eq!(f.script_runs().await, 1);
    assert_eq!(provider.hits_of("collect"), 1, "nothing was asked again");

    drop(second);
    drop(plane);
    f.finish().await;
}
