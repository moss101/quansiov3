//! ADP-007 D: journaled workflow scripts, against a scratch database (blocked, not skipped, without one).
//!
//! Everything below the model is the product: the real PostgreSQL, the real planner and GraphTransaction, the real approval path
//! and Effect Ledger, the engine that runs every worker's turn, the scheduler's background pass and the evidence layer that holds
//! the journal. The model is a scripted stand-in ([`Scripted`]): it answers each worker with the reply the test chose and counts
//! how often each call reached it, which is what "without repeating completed calls" is measured by.
//!
//! * journal replay and restart: a worker that finished but whose result never reached the journal (the process died between)
//!   is resumed by a second composition over the same database and its model is not called again; every call reaches the model
//!   once; the parent makes no model call and no item-level work; there is one worker per item and one reducer;
//! * item results are schema-validated: an invalid reply fails that item with `WORKFLOW_RESULT_INVALID`, the reducer is given the
//!   failure, a script's own single step failing fails the run;
//! * the budget: above the threshold, before the first call and again before a fan-out, the run holds an approval through the
//!   ordinary approval path and creates no worker until it is granted; a denial ends the run; reaching the cap pauses the run into
//!   an attention item instead of failing it, and an approval raises the cap;
//! * a cancelled script run stops calling.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`. Absent: `BLOCKED_EXTERNAL`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_machine::secrets::LocalMasterKeyProvider;
use quansio_server::api::workflow::ApiWorkflowPort;
use quansio_server::api::ApiState;
use quansio_server::composition::Background;
use quansio_server::control::routines::config::AutomationConfig;
use quansio_server::control::schema;
use quansio_server::notify::Notifier;
use quansio_server::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, RuntimeError,
};
use quansio_server::scheduler::workflow::journal::JournalStore;
use quansio_server::scheduler::workflow::{
    AgentCall, CallProgress, WorkflowError, WorkflowExecutor, WorkflowPort,
};
use serde_json::{json, Value};
use sqlx::Row;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YAWF001";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YAWF001";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YAWF001";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YAWF001";
const SIGNING_KEY: &str = "automation-workflow-signing-key";

type Reply = dyn Fn(&str, &str) -> Option<String> + Send + Sync;

/// The model: the reply a test chose for each call, the count of times each call reached it, and what it was asked.
struct Scripted {
    pool: sqlx::PgPool,
    hits: Arc<Mutex<BTreeMap<String, u32>>>,
    asked: Arc<Mutex<BTreeMap<String, String>>>,
    reply: Arc<Reply>,
}

#[async_trait]
impl ModelProposalSource for Scripted {
    async fn propose(&self, request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &request.tenant_id).await?;
        let blocks: Value = sqlx::query_scalar(
            "SELECT m.content_blocks FROM runs r JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
             WHERE r.tenant_id = $1 AND r.id = $2",
        )
        .bind(&request.tenant_id)
        .bind(&request.run_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let blocks = blocks.as_array().cloned().unwrap_or_default();
        let key = blocks
            .iter()
            .find_map(|block| block["notice"]["workflow_call"].as_str())
            .unwrap_or("(not a workflow call)")
            .to_string();
        let text = blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        *self
            .hits
            .lock()
            .expect("hits")
            .entry(key.clone())
            .or_insert(0) += 1;
        self.asked
            .lock()
            .expect("asked")
            .insert(key.clone(), text.clone());
        Ok(ModelProposal {
            assistant_text: (self.reply)(&key, &text),
            ..ModelProposal::default()
        })
    }
}

struct Harness {
    name: String,
    pool: sqlx::PgPool,
    state: ApiState,
    addr: SocketAddr,
    http: reqwest::Client,
    hits: Arc<Mutex<BTreeMap<String, u32>>>,
    asked: Arc<Mutex<BTreeMap<String, String>>>,
    reply: Arc<Reply>,
    config: AutomationConfig,
}

fn keys() -> Arc<dyn quansio_machine::secrets::KeyProvider> {
    Arc::new(LocalMasterKeyProvider::from_master_key(&[3u8; 32], "test:adp007-wf").expect("key"))
}

async fn serve(state: &ApiState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = quansio_server::api::router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    addr
}

/// The default replies: a list of three, an `ok` per item, a summary.
fn happy(key: &str, _text: &str) -> Option<String> {
    Some(match key {
        "collect" => json!({ "items": ["alpha", "beta", "gamma"] }).to_string(),
        "summary" => json!({ "summary": "all three reviewed" }).to_string(),
        _ => json!({ "ok": true, "note": "reviewed" }).to_string(),
    })
}

async fn prepare(
    prefix: &str,
    reply: Arc<Reply>,
    edit: impl FnOnce(&mut AutomationConfig),
) -> Option<Harness> {
    prepare_as(prefix, reply, edit, false).await
}

/// A pool whose every connection acts as `quansio_app`, the role a deployment serves as, so row-level security applies.
async fn app_role_pool(name: &str) -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET ROLE quansio_app").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&common::scratch_url(name))
        .await
        .expect("app-role pool")
}

async fn prepare_as(
    prefix: &str,
    reply: Arc<Reply>,
    edit: impl FnOnce(&mut AutomationConfig),
    as_app: bool,
) -> Option<Harness> {
    std::env::set_var("QUANSIO_APPROVAL_SIGNING_KEY", SIGNING_KEY);
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    // Reads are allowed; nothing a worker proposes here needs more.
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YAWF001', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([{ "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    let mut config = AutomationConfig::shipped();
    edit(&mut config);
    let hits = Arc::new(Mutex::new(BTreeMap::new()));
    let asked = Arc::new(Mutex::new(BTreeMap::new()));
    // The schema and the seed are the owner's; what runs is the deployable's, which may serve as the least-privileged role.
    let serving = if as_app {
        app_role_pool(&name).await
    } else {
        pool
    };
    let h = compose(&name, &serving, &config, &hits, &asked, &reply).await;
    Some(h)
}

/// One composition over the database: the model, the API state and a listener. Calling it again over the same database is a
/// restart of the process.
async fn compose(
    name: &str,
    pool: &sqlx::PgPool,
    config: &AutomationConfig,
    hits: &Arc<Mutex<BTreeMap<String, u32>>>,
    asked: &Arc<Mutex<BTreeMap<String, String>>>,
    reply: &Arc<Reply>,
) -> Harness {
    let model = Scripted {
        pool: pool.clone(),
        hits: Arc::clone(hits),
        asked: Arc::clone(asked),
        reply: Arc::clone(reply),
    };
    let state = ApiState::new(pool.clone())
        .expect("api")
        .with_key_provider(keys())
        .with_automation_config(config.clone())
        .with_model_source(Arc::new(model));
    let addr = serve(&state).await;
    Harness {
        name: name.to_string(),
        pool: pool.clone(),
        state,
        addr,
        http: reqwest::Client::new(),
        hits: Arc::clone(hits),
        asked: Arc::clone(asked),
        reply: Arc::clone(reply),
        config: config.clone(),
    }
}

impl Harness {
    async fn restarted(&self) -> Harness {
        compose(
            &self.name,
            &self.pool,
            &self.config,
            &self.hits,
            &self.asked,
            &self.reply,
        )
        .await
    }

    async fn command(&self, name: &str, id: u32, params: Value) -> (u16, Value) {
        let response = self
            .http
            .post(format!("http://{}/v1/commands/{name}", self.addr))
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", OWNER)
            .json(&json!({ "command_id": format!("cmd_01J8Z3K6F1N8VQ2X5W9YA{id:05}"), "params": params }))
            .send()
            .await
            .expect("post");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        self.http
            .get(format!("http://{}{path}", self.addr))
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", OWNER)
            .send()
            .await
            .expect("get")
            .json()
            .await
            .expect("json")
    }

    async fn rows(&self, sql: &str, bind: &str) -> Vec<sqlx::postgres::PgRow> {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows = sqlx::query(sql)
            .bind(TENANT)
            .bind(bind)
            .fetch_all(&mut *tx)
            .await
            .expect("rows");
        tx.commit().await.expect("commit");
        rows
    }

    async fn count(&self, sql: &str, bind: &str) -> i64 {
        self.rows(sql, bind).await[0].get(0)
    }

    fn hits_of(&self, key: &str) -> u32 {
        self.hits
            .lock()
            .expect("hits")
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    fn total_hits(&self) -> u32 {
        self.hits.lock().expect("hits").values().sum()
    }

    /// A routine with a script, fired now: the fire executes through the interactive path, which starts the script run.
    async fn start_script(&self, id: u32, script: Value) -> (String, String) {
        let (status, created) = self
            .command(
                "CreateRoutine",
                id,
                json!({ "workspace_id": WORKSPACE, "title": "Review the changes", "prompt": "Review what changed.",
                        "trigger": { "kind": "interval", "spec": "1h" }, "workflow": script }),
            )
            .await;
        assert_eq!(status, 200, "{created}");
        let routine = created["result"]["id"].as_str().expect("id").to_string();
        let (status, fired) = self
            .command(
                "TriggerRoutineNow",
                id + 1,
                json!({ "routine_id": routine }),
            )
            .await;
        assert_eq!(status, 200, "{fired}");
        let run = fired["result"]["run_id"]
            .as_str()
            .expect("the script run")
            .to_string();
        (routine, run)
    }

    async fn journal(&self, run: &str) -> quansio_server::scheduler::workflow::journal::Journal {
        JournalStore::new(self.pool.clone(), TENANT)
            .load(run)
            .await
            .expect("journal")
    }

    /// Drive the script run until it ends or waits, through the deployable's background pass.
    async fn pass(&self) -> Result<(), String> {
        // A pass over a stub model ends in seconds; one that does not is a loop, and a loop is a failure with a name.
        tokio::time::timeout(
            std::time::Duration::from_secs(90),
            Background::new(self.pool.clone(), self.state.clone()).workflow_pass(),
        )
        .await
        .unwrap_or_else(|_| panic!("the workflow pass did not end within 90 s"))
    }

    async fn run_status(&self, run: &str) -> String {
        self.get(&format!("/v1/runs/{run}")).await["status"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    async fn finish(self) {
        common::drop_pool(&self.pool, &self.name).await;
    }
}

fn item_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["ok", "note"],
        "properties": { "ok": { "type": "boolean" }, "note": { "type": "string", "maxLength": 200 } }
    })
}

fn items_schema(max: u64) -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["items"],
        "properties": { "items": { "type": "array", "maxItems": max, "items": { "type": "string" } } }
    })
}

fn summary_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["summary"],
        "properties": { "summary": { "type": "string", "maxLength": 500 } }
    })
}

fn script(max_calls: u64, estimated: u64, max_items: u64) -> Value {
    json!({
        "version": 1, "max_agent_calls": max_calls, "estimated_agent_calls": estimated,
        "steps": [
            { "id": "collect", "kind": "agent", "prompt": "List what changed.\n{{payload}}", "output_schema": items_schema(max_items), "effort": "low" },
            { "id": "note", "kind": "log", "message": "collected" },
            { "id": "each", "kind": "parallel", "over": { "step": "collect", "path": "/items" }, "max_items": max_items,
              "agent": { "prompt": "Review item {{index}}: {{item}}", "output_schema": item_schema(), "effort": "medium" } },
            { "id": "summary", "kind": "reduce", "of": "each", "prompt": "Combine the reviews: {{results}}", "output_schema": summary_schema(), "effort": "high" }
        ]
    })
}

/// A port that lets the worker finish and then loses its result, `after` calls in: the process died between the worker's last
/// write and the journal's.
struct DiesAfter {
    inner: ApiWorkflowPort,
    after: usize,
    completed: AtomicUsize,
}

#[async_trait]
impl WorkflowPort for DiesAfter {
    async fn run_call(&self, call: AgentCall) -> Result<CallProgress, WorkflowError> {
        let progress = self.inner.run_call(call).await?;
        if matches!(progress, CallProgress::Replied { .. })
            && self.completed.fetch_add(1, Ordering::SeqCst) + 1 == self.after
        {
            return Err(WorkflowError::Port("the process died".to_string()));
        }
        Ok(progress)
    }
}

#[tokio::test]
async fn a_restart_resumes_a_script_from_its_journal_without_repeating_a_completed_call() {
    let Some(h) = prepare("wf_restart", Arc::new(happy), |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let (routine, run) = h.start_script(1, script(12, 5, 5)).await;

    // The first process: it dies after its third worker finished, before that worker's result reached the journal.
    let dying = WorkflowExecutor::new(
        h.pool.clone(),
        Arc::clone(h.state.automation()),
        Arc::new(DiesAfter {
            inner: ApiWorkflowPort::new(h.state.clone()),
            after: 3,
            completed: AtomicUsize::new(0),
        }),
    );
    let died = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        dying.drive_run(TENANT, &run),
    )
    .await
    .unwrap_or_else(|_| panic!("the first process's pass did not end within 90 s"));
    assert!(matches!(died, Err(WorkflowError::Port(_))), "{died:?}");
    let journal = h.journal(&run).await;
    let finished = journal
        .entries
        .iter()
        .filter(|e| e["entry"] == "call_finished")
        .count();
    assert_eq!(
        journal.calls_used(),
        4,
        "the collector and three items were started"
    );
    assert_eq!(
        finished, 3,
        "the collector and two items were recorded; the third was lost with the process"
    );
    assert!(journal.ended().is_none());
    let hits_before = h.total_hits();
    assert_eq!(hits_before, 4, "four workers reached the model");

    // The second process: a new composition over the same database finishes the run.
    let second = h.restarted().await;
    second.pass().await.expect("pass");
    let journal = second.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("succeeded")),
        "{:#?}",
        journal.entries
    );
    // No completed call was repeated: every call reached the model exactly once, the resumed one included (its worker had
    // already answered; the resumed call read that answer instead of asking again).
    for key in ["collect", "each#0", "each#1", "each#2", "summary"] {
        assert_eq!(
            second.hits_of(key),
            1,
            "{key} reached the model once: {:?}",
            second.hits.lock().expect("hits")
        );
    }
    assert_eq!(second.total_hits(), 5);
    assert_eq!(
        journal.calls_used(),
        5,
        "a call is counted once however often it is resumed"
    );
    let started: Vec<&str> = journal
        .entries
        .iter()
        .filter(|e| e["entry"] == "call_started")
        .filter_map(|e| e["key"].as_str())
        .collect();
    let mut distinct = started.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let finished_keys: Vec<&str> = journal
        .entries
        .iter()
        .filter(|e| e["entry"] == "call_finished")
        .filter_map(|e| e["key"].as_str())
        .collect();
    let mut distinct_finished = finished_keys.clone();
    distinct_finished.sort_unstable();
    distinct_finished.dedup();
    assert_eq!(
        finished_keys.len(),
        distinct_finished.len(),
        "no call was recorded as finished twice: {finished_keys:?}"
    );
    assert_eq!(
        started.len(),
        distinct.len(),
        "no call was started twice: {started:?}"
    );

    // The shape of the run: the graph, one worker per item, one reducer, a parent that made no model call.
    let projection = second
        .get(&format!("/v1/routines?workspace_id={WORKSPACE}"))
        .await;
    let overview = &projection["items"][0]["script_runs"][0];
    assert_eq!(overview["status"], "succeeded", "{overview}");
    assert_eq!(overview["calls_used"], 5);
    let steps = overview["steps"].as_array().expect("steps");
    assert_eq!(
        steps
            .iter()
            .map(|s| s["status"].as_str().unwrap_or(""))
            .collect::<Vec<_>>(),
        ["done", "done", "done", "done"]
    );
    assert_eq!(
        steps[2]["items"],
        json!({ "total": 3, "finished": 3, "failed": 0 })
    );
    assert_eq!(second.run_status(&run).await, "SUCCEEDED");
    assert_eq!(
        second
            .count(
                "SELECT count(*) FROM turns WHERE tenant_id = $1 AND run_id = $2",
                &run
            )
            .await,
        0,
        "the parent makes no model turn and does no item-level work"
    );
    let workers = second
        .count(
            "SELECT count(*) FROM agent_threads WHERE tenant_id = $1 AND agent_kind = 'worker' AND $2 <> ''",
            "x",
        )
        .await;
    assert_eq!(
        workers, 5,
        "one worker per call: the collector, one per item, the reducer"
    );
    let retired = second
        .count(
            "SELECT count(*) FROM agent_threads WHERE tenant_id = $1 AND agent_kind = 'worker' \
             AND status = 'TERMINATED' AND $2 <> ''",
            "x",
        )
        .await;
    assert_eq!(retired, 5, "every worker joined its parent and ended");
    let edges = second
        .count(
            "SELECT count(*) FROM agent_graph_edges WHERE tenant_id = $1 AND joined_at IS NOT NULL AND $2 <> ''",
            "x",
        )
        .await;
    assert_eq!(
        edges, 5,
        "the delegation lineage is in the AgentGraph and every edge was joined"
    );
    // The graph is the execution state: every node of the script is done, so is the Objective.
    let objective = journal.head().expect("head")["script_ref"]["objective_id"]
        .as_str()
        .expect("objective")
        .to_string();
    let statuses: Vec<String> = second
        .rows(
            "WITH RECURSIVE tree AS (SELECT id, status FROM work_nodes WHERE tenant_id = $1 AND id = $2 \
             UNION ALL SELECT n.id, n.status FROM work_nodes n JOIN tree t ON n.parent_id = t.id WHERE n.tenant_id = $1) \
             SELECT status FROM tree",
            &objective,
        )
        .await
        .iter()
        .map(|row| row.get("status"))
        .collect();
    assert_eq!(
        statuses.len(),
        1 + 3 + 3,
        "the Objective, three steps, three items"
    );
    assert!(
        statuses.iter().all(|status| status == "done"),
        "{statuses:?}"
    );
    // The reducer was handed all three outcomes, as data.
    let asked = second
        .asked
        .lock()
        .expect("asked")
        .get("summary")
        .cloned()
        .expect("asked");
    assert!(
        asked.contains("\"index\":2") && asked.contains("\"status\":\"done\""),
        "{asked}"
    );

    // A finished run is not driven again: a later pass changes nothing and calls nothing.
    second.pass().await.expect("pass");
    assert_eq!(second.total_hits(), 5);
    assert_eq!(
        second.journal(&run).await.entries.len(),
        journal.entries.len()
    );
    let _ = routine;
    second.finish().await;
}

#[tokio::test]
async fn an_invalid_item_result_fails_that_item_and_the_reducer_sees_the_failure() {
    let reply: Arc<Reply> = Arc::new(|key: &str, _text: &str| match key {
        "each#1" => Some("I could not do this one, sorry".to_string()),
        "each#2" => Some(json!({ "ok": "yes", "note": "reviewed", "extra": 1 }).to_string()),
        other => happy(other, ""),
    });
    let Some(h) = prepare("wf_invalid", reply, |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let (_routine, run) = h.start_script(1, script(12, 5, 5)).await;
    h.pass().await.expect("pass");
    let journal = h.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("succeeded")),
        "{:#?}",
        journal.entries
    );

    // Item 0 is fine; item 1 is not JSON; item 2 breaks the schema twice over. Each failed item is a typed error with its
    // violations, not a silent drop.
    assert_eq!(journal.finished("each#0").expect("0")["status"], "done");
    let not_json = journal.finished("each#1").expect("1");
    assert_eq!(not_json["status"], "failed");
    assert_eq!(not_json["error"]["code"], "WORKFLOW_RESULT_INVALID");
    assert!(not_json["error"]["message"]
        .as_str()
        .expect("message")
        .contains("not JSON"));
    let broken = journal.finished("each#2").expect("2");
    assert_eq!(broken["error"]["code"], "WORKFLOW_RESULT_INVALID");
    let violations = broken["error"]["details"]["violations"]
        .as_array()
        .expect("violations");
    assert!(
        violations.iter().any(|v| v["path"] == ".ok"),
        "{violations:?}"
    );
    assert!(
        violations.iter().any(|v| v["path"] == ".extra"),
        "{violations:?}"
    );
    assert!(
        journal.result_of("each#2").is_none(),
        "an invalid reply leaves no result"
    );

    // The reducer was given all three outcomes, failures included, in item order.
    let asked = h
        .asked
        .lock()
        .expect("asked")
        .get("summary")
        .cloned()
        .expect("summary asked");
    assert!(asked.contains("\"status\":\"failed\""), "{asked}");
    assert!(asked.contains("WORKFLOW_RESULT_INVALID"), "{asked}");
    assert_eq!(
        asked.matches("\"index\":").count(),
        3,
        "every item is accounted for: {asked}"
    );
    // The failed items are failed nodes; the script itself went on and succeeded.
    let failed = h
        .count(
            "SELECT count(*) FROM work_nodes WHERE tenant_id = $1 AND status = 'failed' AND title LIKE 'each [%' AND $2 <> ''",
            "x",
        )
        .await;
    assert_eq!(failed, 2);
    let projection = h
        .get(&format!("/v1/routines?workspace_id={WORKSPACE}"))
        .await;
    assert_eq!(
        projection["items"][0]["script_runs"][0]["steps"][2]["items"],
        json!({ "total": 3, "finished": 3, "failed": 2 })
    );

    // An invalid reply from a script's own single step fails the run: nothing after it can run on a result that does not exist.
    let second = h.restarted().await;
    let (_r2, run2) = {
        let one_step = json!({ "version": 1, "max_agent_calls": 3, "estimated_agent_calls": 2, "steps": [
            { "id": "first", "kind": "agent", "prompt": "answer", "output_schema": summary_schema() },
            { "id": "second", "kind": "agent", "prompt": "continue from {{steps.first}}", "output_schema": summary_schema() } ] });
        second.start_script(10, one_step).await
    };
    // `first` is answered by the default reply for unknown keys (an item-shaped object), which breaks its schema.
    second.pass().await.expect("pass");
    let journal = second.journal(&run2).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("failed"))
    );
    assert_eq!(
        journal.ended().expect("end")["error"]["code"],
        "WORKFLOW_RESULT_INVALID"
    );
    assert!(
        journal.finished("second").is_none(),
        "the next step never ran"
    );
    assert_eq!(second.run_status(&run2).await, "FAILED");
    second.finish().await;
}

// ------------------------------------------------------------------ the budget and its approvals

async fn approve(h: &Harness, run: &str, id: u32) -> Value {
    let listed = h
        .get(&format!("/v1/approvals?run_id={run}&status=requested"))
        .await;
    let request = listed["approvals"][0].clone();
    assert!(
        request["id"].is_string(),
        "an approval is waiting: {listed}"
    );
    let (status, granted) = h
        .command(
            "ApproveEffect",
            id,
            json!({ "approval_request_id": request["id"], "params_digest": request["params_digest"] }),
        )
        .await;
    assert_eq!(status, 200, "{granted}");
    assert!(
        granted["result"].get("turn").is_none(),
        "a script run takes no model turn when its gate is granted: {granted}"
    );
    request
}

#[tokio::test]
async fn above_the_threshold_a_script_holds_an_approval_before_it_fans_out() {
    let reply: Arc<Reply> = Arc::new(|key: &str, _| match key {
        "collect" => Some(json!({ "items": ["a", "b", "c", "d", "e"] }).to_string()),
        other => happy(other, ""),
    });
    // A low threshold keeps the scripts small: more than 4 calls needs a person.
    let Some(h) = prepare("wf_gate", reply, |config| {
        config.script.approval_threshold_calls = 4
    })
    .await
    else {
        common::blocked_marker();
        return;
    };

    // The estimate itself is above the threshold: the gate comes before the first call.
    let (_routine, run) = h.start_script(1, script(10, 7, 5)).await;
    h.pass().await.expect("pass");
    assert_eq!(
        h.total_hits(),
        0,
        "no worker was called before the approval"
    );
    assert_eq!(h.count("SELECT count(*) FROM agent_threads WHERE tenant_id = $1 AND agent_kind = 'worker' AND $2 <> ''", "x").await, 0,
        "and no worker exists");
    assert_eq!(
        h.run_status(&run).await,
        "WAITING_APPROVAL",
        "the script run is parked on the approval"
    );
    let journal = h.journal(&run).await;
    assert_eq!(journal.calls_used(), 0);
    assert_eq!(journal.open_gate().expect("a gate")["gate"], "fan_out");
    let request = h
        .get(&format!("/v1/approvals?run_id={run}&status=requested"))
        .await["approvals"][0]
        .clone();
    assert!(
        request["summary"]
            .as_str()
            .expect("summary")
            .contains("up to 7 agent calls"),
        "{request}"
    );
    // The gate is an effect on the Effect Ledger, proposed and not yet authorized.
    let effect = h.rows("SELECT status, effect_class, resource ->> 'selector' AS selector FROM effect_records WHERE tenant_id = $1 AND run_id = $2", &run).await;
    assert_eq!(effect.len(), 1);
    assert_eq!(effect[0].get::<String, _>("status"), "PROPOSED");
    assert_eq!(effect[0].get::<String, _>("effect_class"), "record.create");
    assert_eq!(
        effect[0].get::<String, _>("selector"),
        format!("workflow:{run}")
    );
    // Another pass while it waits makes no second request and no call.
    h.pass().await.expect("pass");
    assert_eq!(
        h.count(
            "SELECT count(*) FROM approval_requests WHERE tenant_id = $1 AND run_id = $2",
            &run
        )
        .await,
        1
    );
    assert_eq!(h.total_hits(), 0);

    // The attention item: an approval request reaches the workspace's approvers, and the pause reaches the owner.
    Notifier::new(h.pool.clone(), TENANT)
        .scan(1000)
        .await
        .expect("notifier");
    assert!(h.count("SELECT count(*) FROM notifications WHERE tenant_id = $1 AND kind = 'needs_approval' AND $2 <> ''", "x").await >= 1);
    assert_eq!(h.count("SELECT count(*) FROM notifications WHERE tenant_id = $1 AND kind = 'attention' AND $2 <> ''", "x").await, 1,
        "the pause is an attention item for the owner");

    // A person grants it; the script goes on, and its gate is settled through the ledger.
    approve(&h, &run, 100).await;
    assert_eq!(h.run_status(&run).await, "RUNNING");
    h.pass().await.expect("pass");
    let journal = h.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("succeeded")),
        "{:#?}",
        journal.entries
    );
    assert_eq!(journal.approved_calls(), 7);
    assert_eq!(h.total_hits(), 7, "collect, five items, summary");
    let effect = h.rows("SELECT status, approval_receipt_id FROM effect_records WHERE tenant_id = $1 AND run_id = $2", &run).await;
    assert_eq!(effect[0].get::<String, _>("status"), "SETTLED_SUCCESS");
    assert!(
        effect[0]
            .get::<Option<String>, _>("approval_receipt_id")
            .is_some(),
        "the single-use receipt was consumed"
    );
    assert_eq!(h.run_status(&run).await, "SUCCEEDED");

    // The actual count, not only the estimate: a script that estimates 3 but finds 5 items needs a gate when it learns that.
    let (_r2, run2) = h.start_script(10, script(10, 3, 5)).await;
    h.pass().await.expect("pass");
    let journal = h.journal(&run2).await;
    assert_eq!(
        journal.calls_used(),
        1,
        "only the collector ran: the fan-out waits for its gate"
    );
    assert_eq!(
        journal.open_gate().expect("a gate")["calls"],
        7,
        "the projected total is what is asked for"
    );
    assert_eq!(h.run_status(&run2).await, "WAITING_APPROVAL");
    assert_eq!(h.count("SELECT count(*) FROM work_nodes WHERE tenant_id = $1 AND title LIKE 'each [%' AND $2 <> ''", "x").await, 5,
        "the five item nodes of the first script exist; the second script's do not");
    approve(&h, &run2, 110).await;
    h.pass().await.expect("pass");
    assert_eq!(
        h.journal(&run2)
            .await
            .ended()
            .map(|end| end["status"].clone()),
        Some(json!("succeeded"))
    );

    // A denial ends the script: no worker is created, the run is cancelled.
    let (_r3, run3) = h.start_script(20, script(10, 7, 5)).await;
    h.pass().await.expect("pass");
    let request = h
        .get(&format!("/v1/approvals?run_id={run3}&status=requested"))
        .await["approvals"][0]
        .clone();
    let (status, denied) = h
        .command(
            "DenyEffect",
            120,
            json!({ "approval_request_id": request["id"] }),
        )
        .await;
    assert_eq!(status, 200, "{denied}");
    h.pass().await.expect("pass");
    let journal = h.journal(&run3).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("cancelled")),
        "{:#?}",
        journal.entries
    );
    assert_eq!(journal.calls_used(), 0);
    assert_eq!(h.run_status(&run3).await, "CANCELLED");
    h.finish().await;
}

#[tokio::test]
async fn reaching_the_call_budget_pauses_the_run_into_an_attention_item_instead_of_failing_it() {
    let reply: Arc<Reply> = Arc::new(|key: &str, _| match key {
        "collect" => Some(json!({ "items": ["a", "b", "c", "d", "e"] }).to_string()),
        other => happy(other, ""),
    });
    // Cap 5, estimate 5 (under the threshold): the collector says five items, so the run needs 7 and the cap is 5.
    let Some(h) = prepare("wf_budget", reply, |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let (_routine, run) = h.start_script(1, script(5, 5, 5)).await;
    h.pass().await.expect("pass");
    let journal = h.journal(&run).await;
    assert_eq!(
        journal.calls_used(),
        1,
        "the collector ran; the fan-out would pass the cap"
    );
    let gate = journal.open_gate().expect("a gate").clone();
    assert_eq!(
        (gate["gate"].as_str(), gate["calls"].as_u64()),
        (Some("extend_budget"), Some(7))
    );
    assert!(journal.ended().is_none(), "paused, not failed");
    assert_eq!(h.run_status(&run).await, "WAITING_APPROVAL");
    assert_eq!(
        h.get(&format!("/v1/runs/{run}")).await["terminal_reason"],
        Value::Null
    );
    let paused = h
        .count("SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = 'routine.script_paused' AND $2 <> ''", "x")
        .await;
    assert_eq!(paused, 1);
    Notifier::new(h.pool.clone(), TENANT)
        .scan(1000)
        .await
        .expect("notifier");
    let attention = h.rows("SELECT title FROM notifications WHERE tenant_id = $1 AND kind = 'attention' AND $2 <> ''", "x").await;
    assert_eq!(attention.len(), 1);
    assert!(
        attention[0].get::<String, _>("title").contains("paused"),
        "{:?}",
        attention[0].get::<String, _>("title")
    );
    let projection = h
        .get(&format!("/v1/routines?workspace_id={WORKSPACE}"))
        .await;
    let overview = &projection["items"][0]["script_runs"][0];
    assert_eq!(overview["status"], "waiting");
    assert_eq!(overview["open_gate"]["gate"], "extend_budget");
    assert_eq!(overview["max_agent_calls"], 5);

    // Granted, the cap is raised to what the run needs and the run resumes where it paused.
    approve(&h, &run, 100).await;
    h.pass().await.expect("pass");
    let journal = h.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("succeeded")),
        "{:#?}",
        journal.entries
    );
    assert_eq!(journal.max_calls(), 7);
    assert_eq!(journal.calls_used(), 7);
    assert_eq!(h.hits_of("collect"), 1, "the collector was not run again");
    let resumed = h
        .count("SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = 'routine.script_resumed' AND $2 <> ''", "x")
        .await;
    assert_eq!(resumed, 1);
    h.finish().await;
}

#[tokio::test]
async fn the_orchestrator_and_the_scheduler_leave_a_scripts_nodes_and_runs_to_its_executor() {
    let Some(h) = prepare("wf_orchestrated", Arc::new(happy), |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let background = Background::new(h.pool.clone(), h.state.clone());
    let (_routine, run) = h.start_script(1, script(12, 5, 5)).await;
    // The full background pass (RUN-004 orchestration releasing and joining nodes, the scheduler tick, the routine triggers)
    // runs between the executor's passes, as it does in the deployable.
    for _ in 0..4 {
        background.pass().await.expect("background pass");
        background.workflow_pass().await.expect("workflow pass");
        if h.journal(&run).await.ended().is_some() {
            break;
        }
    }
    background.pass().await.expect("background pass");
    let journal = h.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("succeeded")),
        "{:#?}",
        journal.entries
    );
    for key in ["collect", "each#0", "each#1", "each#2", "summary"] {
        assert_eq!(h.hits_of(key), 1, "{key} reached the model once");
    }
    assert_eq!(h.run_status(&run).await, "SUCCEEDED");
    let statuses: Vec<String> = h
        .rows(
            "SELECT status FROM work_nodes WHERE tenant_id = $1 AND created_by ->> 'script_run' = $2",
            &run,
        )
        .await
        .iter()
        .map(|row| row.get("status"))
        .collect();
    assert_eq!(statuses.len(), 6, "three steps and three items");
    assert!(
        statuses.iter().all(|status| status == "done"),
        "the orchestrator did not move a script node: {statuses:?}"
    );
    h.finish().await;
}

/// Every table a script and its gate touch, as the least-privileged role: a statement run with no tenant context would read nothing
/// and write nothing there, and the dev superuser would hide it.
#[tokio::test]
async fn a_script_with_a_gate_runs_as_the_least_privileged_role() {
    let Some(h) = prepare_as(
        "wf_app_role",
        Arc::new(happy),
        |config| config.script.approval_threshold_calls = 4,
        true,
    )
    .await
    else {
        common::blocked_marker();
        return;
    };
    let (_routine, run) = h.start_script(1, script(12, 7, 5)).await;
    h.pass().await.expect("pass");
    assert_eq!(h.run_status(&run).await, "WAITING_APPROVAL");
    assert_eq!(h.total_hits(), 0);
    approve(&h, &run, 100).await;
    h.pass().await.expect("pass");
    let journal = h.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("succeeded")),
        "{:#?}",
        journal.entries
    );
    assert_eq!(h.total_hits(), 5);
    assert_eq!(h.run_status(&run).await, "SUCCEEDED");
    let projection = h
        .get(&format!("/v1/routines?workspace_id={WORKSPACE}"))
        .await;
    assert_eq!(projection["items"][0]["script_runs"][0]["calls_used"], 5);
    h.finish().await;
}

#[tokio::test]
async fn a_script_run_cancelled_from_outside_makes_no_further_call() {
    let Some(h) = prepare("wf_cancel", Arc::new(happy), |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let (_routine, run) = h.start_script(1, script(12, 5, 5)).await;
    let (status, cancelled) = h.command("CancelRun", 3, json!({ "run_id": run })).await;
    assert_eq!(status, 200, "{cancelled}");
    h.pass().await.expect("pass");
    let journal = h.journal(&run).await;
    assert_eq!(
        journal.ended().map(|end| end["status"].clone()),
        Some(json!("cancelled"))
    );
    assert_eq!(h.total_hits(), 0, "a cancelled script calls nothing");
    assert_eq!(h.run_status(&run).await, "CANCELLED");
    let open = h
        .count("SELECT count(*) FROM work_nodes WHERE tenant_id = $1 AND status NOT IN ('done', 'failed', 'cancelled') AND $2 <> ''", "x")
        .await;
    // Only the seed node of the harness is not part of the script.
    assert_eq!(open, 1, "every node of the script is settled");
    h.finish().await;
}

#[tokio::test]
async fn a_script_run_another_instance_is_driving_is_left_alone() {
    let Some(h) = prepare("wf_lock", Arc::new(happy), |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let (_routine, run) = h.start_script(1, script(12, 5, 5)).await;
    // Another instance holds the run's lock (a transaction that has taken it and not yet finished).
    let mut holder = h.pool.begin().await.expect("begin");
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("workflow:{TENANT}:{run}"))
        .fetch_one(&mut *holder)
        .await
        .expect("lock");
    assert!(got);
    let executor = quansio_server::api::workflow::executor(&h.state);
    let report = executor.drive_run(TENANT, &run).await.expect("drive");
    assert_eq!(
        report.status,
        quansio_server::scheduler::workflow::DriveStatus::Busy
    );
    assert_eq!(h.total_hits(), 0, "the second instance made no call");
    assert!(h
        .journal(&run)
        .await
        .entries
        .iter()
        .all(|e| e["entry"] != "call_started"));
    // Released, the run is driven to its end by whoever comes next.
    holder.commit().await.expect("release");
    h.pass().await.expect("pass");
    assert_eq!(
        h.journal(&run)
            .await
            .ended()
            .map(|end| end["status"].clone()),
        Some(json!("succeeded"))
    );
    assert_eq!(h.total_hits(), 5, "each call reached the model once");
    h.finish().await;
}
