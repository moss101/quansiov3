//! Served `work.propose_plan` (RUN-003, INT-005) through the deployable's composition.
//!
//! HTTP `PostMessage` → `chat_engine` → Context plane (`BuildContext`, which is offered the
//! WorkGraph as a `work` segment) → model gateway → the dev stack's stub provider, scripted
//! with `[[stub:call work__propose_plan {...}]]` → the Tool Registry, capability, policy and
//! the Effect Ledger → the composed `RunPlanApplication` → the Planner and the graph
//! transaction. A refusal reaches the tool call with the planner's own code.
//!
//! The provider is the dev stub (deterministic), never real-boundary evidence.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `uv`, and the stub provider at
//! `QUANSIO_TEST_STUB_PROVIDER_URL` (default `http://127.0.0.1:59020`); without them the test
//! prints a `BLOCKED_EXTERNAL` marker and returns.

use std::net::SocketAddr;
use std::sync::Arc;

use quansio_server::control::schema;
use quansio_server::intelligence::model_source::{GatewayModelSource, ModelCallSettings};
use quansio_server::intelligence::IntelligenceClient;
use quansio_server::runtime::context_bridge::GatewayContextBridge;
use serde_json::{json, Value};
use sqlx::Row;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YRPQ003";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YRPQ003";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YRPQ003";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YRPQ003";
/// A test-only approval signing key: approvals are signed receipts.
const SIGNING_KEY: &str = "served-plan-test-signing-key";

/// Plans are `record.create` on `work`; the run's projection must grant it for the tool to
/// be offered at all.
async fn seed_policy(pool: &sqlx::PgPool) {
    let rules = json!([
        { "effect_class": "record.create", "resource": { "kind": "work", "selector": "*" }, "decision": "allow" },
        { "effect_class": "memory.write", "resource": { "kind": "work", "selector": "*" }, "decision": "allow" }
    ]);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YRPQ003', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
}

struct Served {
    addr: SocketAddr,
    pool: sqlx::PgPool,
    name: String,
    http: reqwest::Client,
    _service: common::intelligence::Service,
}

async fn serve(prefix: &str) -> Option<Served> {
    std::env::set_var("QUANSIO_APPROVAL_SIGNING_KEY", SIGNING_KEY);
    let service = common::intelligence::start().await?;
    let name = common::scratch_name(prefix);
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return None;
    };
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;
    let settings = ModelCallSettings::load().expect("config/intelligence.yaml");
    let client = IntelligenceClient::connect_lazy(&service.endpoint).expect("endpoint");
    let context = Arc::new(GatewayContextBridge::new(client.clone(), settings.deadline));
    let state = quansio_server::api::ApiState::new(pool.clone())
        .expect("api")
        .with_model_source(Arc::new(GatewayModelSource::new(
            pool.clone(),
            client,
            context,
            settings,
        )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = quansio_server::api::router(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    Some(Served {
        addr,
        pool,
        name,
        http: reqwest::Client::new(),
        _service: service,
    })
}

impl Served {
    async fn command(&self, name: &str, id: u32, params: Value) -> (u16, Value) {
        let response = self
            .http
            .post(format!("http://{}/v1/commands/{name}", self.addr))
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", USER)
            .json(&json!({
                "command_id": format!("cmd_01J8Z3K6F1N8VQ2X5W9YR{id:05}"),
                "params": params,
            }))
            .send()
            .await
            .expect("post");
        let status = response.status().as_u16();
        (status, response.json().await.expect("json"))
    }

    /// Post a message whose model turn makes exactly one scripted tool call.
    async fn scripted(&self, id: u32, thread: Option<&str>, tool: &str, args: &Value) -> Value {
        let mut params = json!({
            "workspace_id": WORKSPACE,
            "content": format!("Do it. [[stub:call {} {args}]]", tool.replace('.', "__")),
        });
        if let Some(thread) = thread {
            params["thread_id"] = json!(thread);
        }
        let (status, body) = self.command("PostMessage", id, params).await;
        assert_eq!(status, 200, "{body}");
        let result = body["result"].clone();
        if result["turn"]["state"] == "WAITING_APPROVAL" {
            // A tier-2 effect waits for the user, who approves exactly the previewed call.
            let run_id = result["run_id"].as_str().expect("run");
            let listed = self
                .get(&format!("/v1/approvals?run_id={run_id}&status=requested"))
                .await;
            let request = &listed["approvals"][0];
            let (status, granted) = self
                .command(
                    "ApproveEffect",
                    id * 100 + 1,
                    json!({
                        "approval_request_id": request["id"],
                        "params_digest": request["params_digest"],
                    }),
                )
                .await;
            assert_eq!(status, 200, "{granted}");
            assert_eq!(granted["result"]["turn"]["kind"], "completed", "{granted}");
        } else {
            assert!(
                result["reply_message_id"].is_string(),
                "the turn completed with a reply: {body}"
            );
        }
        result
    }

    async fn get(&self, path: &str) -> Value {
        self.http
            .get(format!("http://{}{path}", self.addr))
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", USER)
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

    /// `(tool, status, result)` of every call in the run, oldest first.
    async fn tool_calls(&self, run_id: &str) -> Vec<(String, String, Value)> {
        self.rows(
            "SELECT c.tool_name, c.status, c.result FROM tool_calls c \
             JOIN steps s ON s.id = c.step_id JOIN turns t ON t.id = s.turn_id \
             WHERE c.tenant_id = $1 AND t.run_id = $2 ORDER BY c.created_at",
            run_id,
        )
        .await
        .iter()
        .map(|row| {
            (
                row.get("tool_name"),
                row.get("status"),
                row.get::<Option<Value>, _>("result").unwrap_or(Value::Null),
            )
        })
        .collect()
    }

    async fn finish(self) {
        common::drop_pool(&self.pool, &self.name).await;
    }
}

async fn graph(served: &Served) -> Value {
    served
        .get(&format!("/v1/workgraph?workspace_id={WORKSPACE}"))
        .await
}

#[tokio::test]
async fn a_served_run_observes_the_workgraph_and_its_plan_is_committed_or_refused_with_its_code() {
    let Some(served) = serve("served_plan").await else {
        return;
    };
    let (status, objective) = served
        .command(
            "CreateObjective",
            1,
            json!({ "workspace_id": WORKSPACE, "title": "Ship the report" }),
        )
        .await;
    assert_eq!(status, 200, "{objective}");
    let objective_id = objective["result"]["objective_id"]
        .as_str()
        .expect("objective")
        .to_string();
    let before = graph(&served).await;
    let revision = before["revision"].as_u64().expect("revision");

    // The model plans a task under the objective, against the revision it was shown.
    let plan = json!({
        "base_revision": revision,
        "rationale": "split the objective",
        "nodes_add": [{ "id": "draft", "kind": "task", "title": "Draft the report",
                        "parent_id": objective_id }]
    });
    let args = json!({ "proposal_id": "prop_served_1", "proposal_json": plan.to_string() });
    let result = served.scripted(2, None, "work.propose_plan", &args).await;
    let run_id = result["run_id"].as_str().expect("run").to_string();
    let thread_id = result["thread_id"].as_str().expect("thread").to_string();

    let calls = served.tool_calls(&run_id).await;
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, "work.propose_plan");
    assert_eq!(calls[0].1, "completed", "{calls:?}");
    assert_eq!(calls[0].2["accepted"], true, "{calls:?}");
    assert_eq!(calls[0].2["revision"], revision + 1, "{calls:?}");
    assert_eq!(calls[0].2["proposal_id"], "prop_served_1", "{calls:?}");

    let after = graph(&served).await;
    assert_eq!(after["revision"], revision + 1, "{after}");
    let draft = after["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["title"] == "Draft the report")
        .cloned()
        .expect("the planned node exists");
    assert_eq!(draft["parent_id"], objective_id, "{draft}");
    assert_eq!(draft["origin"], "plan_proposal", "{draft}");
    assert_eq!(draft["created_by"]["kind"], "agent", "{draft}");

    // The model was offered the graph, at the revision it planned against.
    let projections = served
        .rows(
            "SELECT p.segments FROM context_projections p \
             WHERE p.tenant_id = $1 AND p.run_id = $2 ORDER BY p.created_at",
            &run_id,
        )
        .await;
    let first: Value = projections[0].get("segments");
    let work: Vec<&Value> = first
        .as_array()
        .expect("segments")
        .iter()
        .filter(|segment| segment["kind"] == "work")
        .collect();
    assert_eq!(work.len(), 1, "{first}");
    assert_eq!(
        work[0]["source"],
        format!("{WORKSPACE}@{revision}"),
        "{first}"
    );
    assert_eq!(work[0]["trust_level"], "trusted_user", "{first}");
    // After the plan, the graph holds a model-authored node: its titles are agent text.
    let latest: Value = projections.last().expect("latest").get("segments");
    let work = latest
        .as_array()
        .expect("segments")
        .iter()
        .find(|segment| segment["kind"] == "work")
        .cloned()
        .expect("work segment");
    assert_eq!(work["source"], format!("{WORKSPACE}@{}", revision + 1));
    assert_eq!(work["trust_level"], "agent_generated", "{latest}");

    // The same plan again is stale: the call is refused with the planner's code, the turn
    // goes on, and nothing is written.
    let stale = json!({ "proposal_id": "prop_served_2", "proposal_json": plan.to_string() });
    let stale_run = served
        .scripted(3, Some(&thread_id), "work.propose_plan", &stale)
        .await["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    // A proposal naming another workspace is refused before the planner reads anything.
    let mut foreign = plan.clone();
    foreign["base_revision"] = json!(revision + 1);
    foreign["workspace_id"] = json!("ws_01J8Z3K6F1N8VQ2X5W9YXTHER0");
    let foreign = json!({ "proposal_id": "prop_served_3", "proposal_json": foreign.to_string() });
    let foreign_run = served
        .scripted(4, Some(&thread_id), "work.propose_plan", &foreign)
        .await["run_id"]
        .as_str()
        .expect("run")
        .to_string();

    let mut calls = Vec::new();
    for run in [&run_id, &stale_run, &foreign_run] {
        for call in served.tool_calls(run).await {
            if !calls.contains(&call) {
                calls.push(call);
            }
        }
    }
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[1].1, "rejected", "{calls:?}");
    assert_eq!(calls[1].2["code"], "CONFLICT_REVISION", "{calls:?}");
    assert_eq!(calls[2].1, "rejected", "{calls:?}");
    assert_eq!(calls[2].2["code"], "VALIDATION_SCHEMA", "{calls:?}");
    assert_eq!(graph(&served).await["revision"], revision + 1);

    let events = served
        .rows(
            "SELECT type FROM runtime_events WHERE tenant_id = $1 AND type LIKE $2 \
             ORDER BY sequence",
            "work.plan_%",
        )
        .await
        .iter()
        .map(|row| row.get::<String, _>("type"))
        .collect::<Vec<_>>();
    assert_eq!(
        events.iter().filter(|e| *e == "work.plan_applied").count(),
        2,
        "the objective and the served plan: {events:?}"
    );
    assert!(
        events.iter().any(|e| e == "work.plan_rejected"),
        "the stale plan is recorded as rejected: {events:?}"
    );

    served.finish().await;
}
