//! A composition script through the server's real turn-loop dispatcher (ADP-009, DOMAIN.md §7.4, §7.5), against a real
//! PostgreSQL database, a **real MCP server process** behind the tools the script calls, and the real engine.
//!
//! The model proposes one `compose.run`. The sandbox in `qworkerd` runs its script, and every `call(tool, args)` in it is handed
//! back to `ToolDispatchService::dispatch_nested` — the same validation, capability projection, policy, approval, Effect Ledger,
//! dispatch and settlement a call the model proposed directly goes through, recorded in the same tables:
//!
//! * **one governed call per host call**: each is a `tool_calls` row (caller `model`) with its own EffectRecord, and the script
//!   gets the tool's own result back; the `compose.run` call itself is a read that grants nothing;
//! * **projection narrowing**: a call the run's projection does not cover is `CAPABILITY_DENIED` inside the script, no effect is
//!   reserved and the real server never hears of it; `compose.run` is exposed only to a projection that grants it;
//! * **no control tools, no recursion**: `work.complete`, `user.ask` and `compose.run` itself are refused to a script and the
//!   refusals are recorded;
//! * **an approval or an unknown outcome inside a script ends it**: the call parks (nothing reaches the server while it waits) or
//!   settles `OUTCOME_UNKNOWN`, and the same call proposed again is refused by the ledger;
//! * **the budget is a count of rows**: a script that floods calls makes exactly its budget of `tool_calls` and effects.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` and `python3` on `PATH`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_qworkerd::browser::tools::ReplyStatus;
use quansio_qworkerd::compose::{
    ComposeConfig, ComposeContext, ComposeHost, HostCaller, HostResult,
};
use quansio_qworkerd::mcp::{EgressGate, McpCallContext, McpHost};
use quansio_server::control::schema;
use quansio_server::policy::{ActorRoles, TenantRole, WorkspaceRole};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedToolCall,
    RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, ToolDispatchRequest, TurnInput,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, HostDispatch, HostFailure, HostOutcome, NestedOutcome, RoleProvider,
    StoredProjectionProvider, ToolDispatchService, ToolHostPort,
};
use quansio_tools::{McpConfig, RegistryHandle};
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP00A";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP00A";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP00A";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP00A";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9YADP00A";

fn python() -> Option<String> {
    let output = Command::new("which").arg("python3").output().ok()?;
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if path.is_empty() {
        eprintln!("BLOCKED_EXTERNAL: python3 is not on PATH; the MCP fixture server cannot run");
        None
    } else {
        Some(path)
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../qworkerd/tests/fixtures/mcp_server.py")
}

struct Stdio;

#[async_trait]
impl EgressGate for Stdio {
    async fn permit(
        &self,
        _url: &str,
        _resolved: &[std::net::IpAddr],
        _at: &str,
    ) -> Result<(), String> {
        Err("no_grant".to_string())
    }
}

/// The script's way back into the runtime: the dispatcher's nested call, in the run the script belongs to.
struct ServerCaller {
    dispatcher: Arc<ToolDispatchService>,
    pool: PgPool,
    run_id: String,
    step_id: String,
    counter: Mutex<u32>,
}

impl ServerCaller {
    async fn request(&self) -> ToolDispatchRequest {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let (turn_id,): (String,) =
            sqlx::query_as("SELECT turn_id FROM steps WHERE id = $1 AND tenant_id = $2")
                .bind(&self.step_id)
                .bind(TENANT)
                .fetch_one(&mut *tx)
                .await
                .expect("the step belongs to a turn");
        let (generation,): (i64,) =
            sqlx::query_as("SELECT generation FROM runs WHERE id = $1 AND tenant_id = $2")
                .bind(&self.run_id)
                .bind(TENANT)
                .fetch_one(&mut *tx)
                .await
                .expect("run");
        tx.commit().await.expect("commit");
        ToolDispatchRequest {
            run_id: self.run_id.clone(),
            turn_id,
            step_id: self.step_id.clone(),
            generation: u64::try_from(generation).expect("generation"),
        }
    }
}

#[async_trait]
impl HostCaller for ServerCaller {
    async fn call(&self, tool: &str, args: Value) -> HostResult {
        let n = {
            let mut counter = self.counter.lock().expect("counter");
            *counter += 1;
            *counter
        };
        let request = self.request().await;
        let call = ProposedToolCall::new(
            format!("script_{n}_{}", UlidGenerator::new().generate()),
            tool,
            args,
        );
        match self.dispatcher.dispatch_nested(call, request).await {
            Ok(NestedOutcome::Completed { result }) => HostResult::Ok(result),
            Ok(NestedOutcome::Failed { error }) => HostResult::Failed {
                code: error["code"].as_str().unwrap_or("INTERNAL").to_string(),
                message: error["message"].as_str().unwrap_or("").to_string(),
                retryable: error["retryable"].as_bool().unwrap_or(false),
                details: None,
            },
            Ok(NestedOutcome::Rejected { code, detail }) => HostResult::Failed {
                code,
                message: detail,
                retryable: false,
                details: None,
            },
            Ok(NestedOutcome::Parked { wait_key }) => HostResult::Parked { detail: wait_key },
            Ok(NestedOutcome::Unknown { effect_id }) => HostResult::Unknown {
                detail: effect_id.unwrap_or_default(),
            },
            Err(error) => HostResult::Failed {
                code: error.code().to_string(),
                message: error.to_string(),
                retryable: false,
                details: None,
            },
        }
    }
}

/// What a worker would do with a dispatch: `compose.run` to the sandbox, `mcp.*` to the MCP client.
struct WorkerHost {
    compose: ComposeHost,
    mcp: Arc<McpHost>,
    dispatcher: OnceLock<Arc<ToolDispatchService>>,
    pool: PgPool,
}

#[async_trait]
impl ToolHostPort for WorkerHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        if dispatch.tool == "compose.run" {
            let caller = Arc::new(ServerCaller {
                dispatcher: Arc::clone(self.dispatcher.get().expect("composed")),
                pool: self.pool.clone(),
                run_id: dispatch.run_id.clone(),
                step_id: dispatch.step_id.clone(),
                counter: Mutex::new(0),
            });
            let reply = self
                .compose
                .run(
                    &dispatch.args,
                    &ComposeContext {
                        run_id: &dispatch.run_id,
                        dispatch_token: &dispatch.dispatch_token,
                    },
                    caller,
                )
                .await;
            return Ok(match reply.status {
                ReplyStatus::Ok => HostOutcome::Success {
                    output: reply.output,
                    evidence_ids: Vec::new(),
                    remote_ref: None,
                },
                _ => HostOutcome::Failure {
                    retryable: false,
                    error: reply.error.unwrap_or(Value::Null),
                    evidence_ids: Vec::new(),
                },
            });
        }
        let reply = self
            .mcp
            .dispatch(
                &dispatch.tool,
                dispatch.version,
                &dispatch.args,
                &McpCallContext {
                    dispatch_token: &dispatch.dispatch_token,
                    effect_id: &dispatch.effect_id,
                },
            )
            .await;
        Ok(match reply.status {
            ReplyStatus::Ok => HostOutcome::Success {
                output: reply.output,
                evidence_ids: Vec::new(),
                remote_ref: None,
            },
            ReplyStatus::Error => HostOutcome::Failure {
                retryable: reply
                    .error
                    .as_ref()
                    .and_then(|e| e["retryable"].as_bool())
                    .unwrap_or(false),
                error: reply.error.unwrap_or(Value::Null),
                evidence_ids: Vec::new(),
            },
            ReplyStatus::InDoubt => HostOutcome::Unknown {
                detail: "in doubt".to_string(),
            },
        })
    }
}

#[derive(Clone)]
struct Scripted {
    moves: Arc<Mutex<VecDeque<(String, Value)>>>,
}

#[async_trait]
impl ModelProposalSource for Scripted {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let next = self.moves.lock().expect("moves").pop_front();
        let n = UlidGenerator::new().generate().to_string();
        Ok(match next {
            Some((tool, arguments)) => ModelProposal {
                tool_calls: vec![ProposedToolCall::new(format!("call_{n}"), tool, arguments)],
                ..ModelProposal::default()
            },
            None => ModelProposal {
                assistant_text: Some("done".to_string()),
                ..ModelProposal::default()
            },
        })
    }
}

#[derive(Clone)]
struct Roles(Arc<Mutex<ActorRoles>>);

#[async_trait]
impl RoleProvider for Roles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: *self.0.lock().expect("roles"),
            user_id: Some(USER.to_string()),
        }))
    }
}

struct Harness {
    name: String,
    pool: PgPool,
    engine: RuntimeEngine,
    dispatcher: Arc<ToolDispatchService>,
    mcp: Arc<McpHost>,
    roles: Arc<Mutex<ActorRoles>>,
    moves: Arc<Mutex<VecDeque<(String, Value)>>>,
    log: PathBuf,
    _dir: tempfile::TempDir,
}

const TOOLS: &str = "      echo: { effect_class: read.external }
      mixed: { effect_class: read.external }
      slow: { effect_class: content.upload }
";

const COMPOSE_GRANT: (&str, &str, &str) = ("read.internal", "target", "*");

impl Harness {
    async fn prepare(prefix: &str) -> Option<Self> {
        let python = python()?;
        let name = admin_url().map(|_| scratch_name(prefix))?;
        let pool = fresh_database(&name).await?;
        schema::migrate(&pool).await.expect("migrate");
        seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
        let mut tx = pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query(
            "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
             VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
        )
        .bind(AGENT)
        .bind(TENANT)
        .bind(WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("agent thread");
        sqlx::query(
            "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
             VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YADP00A', $1, NULL, 'tenant', $2)",
        )
        .bind(TENANT)
        .bind(json!([
            { "effect_class": "read.internal", "resource": { "kind": "target", "selector": "*" }, "decision": "allow" },
            { "effect_class": "read.external", "resource": { "kind": "connector", "selector": "mcp.demo/*" }, "decision": "allow" },
            { "effect_class": "content.upload", "resource": { "kind": "connector", "selector": "mcp.demo/*" }, "decision": "allow" },
            { "effect_class": "data.upload.protected", "resource": { "kind": "connector", "selector": "mcp.demo/*" }, "decision": "ask" }
        ]))
        .execute(&mut *tx)
        .await
        .expect("policy");
        tx.commit().await.expect("commit");

        let dir = tempfile::Builder::new()
            .prefix("compose-dispatch")
            .tempdir()
            .expect("tempdir");
        let log = dir.path().join("calls.jsonl");
        let mut text = include_str!("../../../config/mcp.yaml").replace("servers: []", "");
        text = text.replace("  call_timeout_ms: 30000", "  call_timeout_ms: 400");
        text.push_str(&format!(
            "servers:
  - name: demo
    transport: stdio
    command: {python}
    args: [\"{}\", \"--transport\", \"stdio\", \"--log\", \"{}\", \"--state\", \"{}\"]
    tools:
{TOOLS}",
            fixture().display(),
            log.display(),
            dir.path().join("state").display()
        ));
        let handle = RegistryHandle::builtin();
        let mcp = Arc::new(McpHost::new(
            McpConfig::from_yaml(&text).expect("config"),
            handle.clone(),
            Arc::new(Stdio),
        ));
        let report = mcp.sync().await;
        assert!(report.servers[0].connected, "{report:#?}");

        let identity = RuntimeIdentity::system(
            TENANT,
            "compose-test",
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        let delegation: Arc<dyn DelegationPort> = Arc::new(
            AgentDelegationPort::with_structural_check(pool.clone(), identity.clone())
                .expect("delegation"),
        );
        let worker = Arc::new(WorkerHost {
            compose: ComposeHost::new(ComposeConfig::builtin()),
            mcp: Arc::clone(&mcp),
            dispatcher: OnceLock::new(),
            pool: pool.clone(),
        });
        let roles = Arc::new(Mutex::new(ActorRoles::new(
            Some(TenantRole::Member),
            Some(WorkspaceRole::Editor),
        )));
        let dispatcher = Arc::new(
            ToolDispatchService::new(
                pool.clone(),
                identity.clone(),
                Arc::new(
                    StoredProjectionProvider::new(pool.clone(), identity.clone())
                        .expect("projections"),
                ),
                Arc::new(Roles(Arc::clone(&roles))),
                Arc::clone(&worker) as Arc<dyn ToolHostPort>,
                Arc::clone(&delegation),
            )
            .expect("dispatch")
            .with_registry_handle(handle),
        );
        worker
            .dispatcher
            .set(Arc::clone(&dispatcher))
            .ok()
            .expect("set once");
        let moves = Arc::new(Mutex::new(VecDeque::new()));
        let engine = RuntimeEngine::new(pool.clone(), identity)
            .expect("engine")
            .with_model_source(Arc::new(Scripted {
                moves: Arc::clone(&moves),
            }))
            .with_tool_dispatch(dispatcher.clone())
            .with_delegation(delegation);
        Some(Self {
            name,
            pool,
            engine,
            dispatcher,
            mcp,
            roles,
            moves,
            log,
            _dir: dir,
        })
    }

    /// The model proposes `compose.run` with this script in a fresh run whose projection holds exactly `grants`.
    async fn compose(&self, script: &str, extra: Value, grants: &[(&str, &str, &str)]) -> String {
        let run = self
            .engine
            .create_run(NewRun::new(
                WORKSPACE,
                CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
                CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
                RunTriggerKind::Manual,
            ))
            .await
            .expect("run");
        let run = self
            .engine
            .enqueue(&run.id, run.generation)
            .await
            .expect("enqueue");
        let run = self
            .engine
            .start(&run.id, run.generation)
            .await
            .expect("start");
        self.seed_projection(&run.id.to_string(), grants).await;
        let mut arguments = json!({ "script": script });
        for (key, value) in extra.as_object().into_iter().flatten() {
            arguments[key] = value.clone();
        }
        self.moves
            .lock()
            .expect("moves")
            .push_back(("compose.run".to_string(), arguments));
        let current = self.engine.store().load_run(&run.id).await.expect("run");
        let _ = self
            .engine
            .run_turn(
                &run.id,
                current.generation,
                TurnInput::new(RunTriggerKind::Manual),
            )
            .await
            .expect("turn");
        run.id.to_string()
    }

    async fn seed_projection(&self, run_id: &str, grants: &[(&str, &str, &str)]) {
        let grants: Vec<Grant> = grants
            .iter()
            .map(|&(class, kind, selector)| {
                Grant::from_json(&json!({
                    "effect_class": class,
                    "resource": { "kind": kind, "selector": selector },
                    "constraints": {},
                }))
                .expect("grant")
            })
            .collect();
        let projection = CapabilityProjection::from_parts(
            CanonicalId::generate(Prefix::CapabilityProjection, &mut UlidGenerator::new()),
            ProjectionSubject::new(SubjectKind::Run, run_id),
            Vec::new(),
            grants,
            Utc::now(),
            Some(Utc::now() + chrono::Duration::hours(1)),
        )
        .expect("projection");
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query(
            "INSERT INTO capability_projections (id, tenant_id, workspace_id, subject_kind, \
             subject_id, inputs, grants, inputs_digest, computed_at, expires_at) \
             VALUES ($1, $2, $3, 'run', $4, $5, $6, $7, $8, $9)",
        )
        .bind(projection.id.to_string())
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(run_id)
        .bind(serde_json::to_value(&projection.inputs).expect("inputs"))
        .bind(serde_json::to_value(&projection.grants).expect("grants"))
        .bind(projection.inputs_digest.to_string())
        .bind(projection.computed_at)
        .bind(projection.expires_at)
        .execute(&mut *tx)
        .await
        .expect("projection row");
        tx.commit().await.expect("commit");
    }

    /// The cell's output as the `compose.run` call row recorded it.
    async fn cell(&self, run: &str) -> Value {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let result: Option<Value> = sqlx::query_scalar(
            "SELECT result FROM tool_calls WHERE tenant_id = $1 AND tool_name = 'compose.run' AND step_id IN \
             (SELECT s.id FROM steps s JOIN turns t ON t.id = s.turn_id WHERE t.run_id = $2) \
             ORDER BY created_at ASC, id ASC LIMIT 1",
        )
        .bind(TENANT)
        .bind(run)
        .fetch_one(&mut *tx)
        .await
        .expect("the compose.run call was recorded");
        tx.commit().await.expect("commit");
        result.unwrap_or(Value::Null)
    }

    /// `(tool, status, caller, result)` of every call row of a run, oldest first.
    async fn calls(&self, run: &str) -> Vec<(String, String, String, Value)> {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows: Vec<(String, String, String, Option<Value>)> = sqlx::query_as(
            "SELECT tc.tool_name, tc.status, tc.caller, tc.result FROM tool_calls tc \
             JOIN steps s ON s.id = tc.step_id JOIN turns t ON t.id = s.turn_id \
             WHERE tc.tenant_id = $1 AND t.run_id = $2 ORDER BY tc.created_at, tc.id",
        )
        .bind(TENANT)
        .bind(run)
        .fetch_all(&mut *tx)
        .await
        .expect("calls");
        tx.commit().await.expect("commit");
        rows.into_iter()
            .map(|(a, b, c, d)| (a, b, c, d.unwrap_or(Value::Null)))
            .collect()
    }

    async fn effects(&self, run: &str) -> Vec<(String, String)> {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT effect_class, status FROM effect_records WHERE tenant_id = $1 AND run_id = $2 ORDER BY created_at, id",
        )
        .bind(TENANT)
        .bind(run)
        .fetch_all(&mut *tx)
        .await
        .expect("effects");
        tx.commit().await.expect("commit");
        rows
    }

    fn server_calls(&self, tool: &str) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|entry| entry["method"] == "tools/call" && entry["name"] == tool)
            .count()
    }

    async fn finish(self) {
        self.mcp.close_all().await;
        drop_pool(&self.pool, &self.name).await;
    }
}

async fn harness(prefix: &str) -> Option<Harness> {
    let harness = Harness::prepare(prefix).await;
    if harness.is_none() && admin_url().is_none() {
        blocked_marker();
    }
    harness
}

fn result_of(cell: &Value) -> Value {
    serde_json::from_str(cell["result_json"].as_str().unwrap_or("null")).unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn every_call_in_a_script_is_a_governed_call_with_its_own_row_and_effect() {
    let Some(h) = harness("adp009_script").await else {
        return;
    };
    let grants = [
        COMPOSE_GRANT,
        ("read.external", "connector", "mcp.demo/echo"),
    ];
    let run = h
        .compose(
            r#"
            const first = call("mcp.demo.echo", { text: "one" });
            const second = call("mcp.demo.echo", { text: first.text + " two" });
            let denied;
            try { call("mcp.demo.mixed", {}); denied = "ran"; } catch (error) { denied = error.code; }
            return { second: second.text, trust: second.trust, denied };
            "#,
            json!({}),
            &grants,
        )
        .await;
    let cell = h.cell(&run).await;
    assert_eq!(cell["ok"], json!(true), "{cell:#}");
    let result = result_of(&cell);
    assert_eq!(result["second"], json!("one two"));
    assert_eq!(result["trust"], json!("UNTRUSTED_EXTERNAL"));
    assert_eq!(
        result["denied"],
        json!("CAPABILITY_DENIED"),
        "a call the projection does not cover is refused inside the script"
    );
    assert_eq!(cell["host_calls"], json!(3));

    let calls = h.calls(&run).await;
    let names: Vec<&str> = calls.iter().map(|call| call.0.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "compose.run",
            "mcp.demo.echo",
            "mcp.demo.echo",
            "mcp.demo.mixed"
        ]
    );
    assert!(
        calls.iter().all(|call| call.2 == "model"),
        "a script's call is the model's call: {calls:#?}"
    );
    assert_eq!(calls[0].1, "completed");
    assert_eq!(calls[1].1, "completed");
    assert_eq!(calls[3].1, "rejected");
    assert_eq!(calls[3].3["code"], json!("CAPABILITY_DENIED"));
    assert_eq!(
        h.effects(&run).await,
        vec![
            ("read.internal".to_string(), "SETTLED_SUCCESS".to_string()),
            ("read.external".to_string(), "SETTLED_SUCCESS".to_string()),
            ("read.external".to_string(), "SETTLED_SUCCESS".to_string()),
        ],
        "the compose.run read, and one effect per call that was allowed: the refused one reserved nothing"
    );
    assert_eq!(h.server_calls("echo"), 2);
    assert_eq!(
        h.server_calls("mixed"),
        0,
        "the real server never heard of the refused call"
    );
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn compose_run_is_exposed_only_to_a_projection_that_grants_it() {
    let Some(h) = harness("adp009_expose").await else {
        return;
    };
    let with = h
        .compose(
            "return 1;",
            json!({}),
            &[
                COMPOSE_GRANT,
                ("read.external", "connector", "mcp.demo/echo"),
            ],
        )
        .await;
    let exposed = h
        .dispatcher
        .exposed_tools(&with, Utc::now())
        .await
        .expect("exposed");
    assert!(
        exposed.iter().any(|name| name == "compose.run"),
        "{exposed:?}"
    );
    assert!(exposed.iter().any(|name| name == "mcp.demo.echo"));
    // Without the target grant the model is not offered compose.run, and proposing it is refused.
    let without = h
        .compose(
            "return 1;",
            json!({}),
            &[("read.external", "connector", "mcp.demo/echo")],
        )
        .await;
    let exposed = h
        .dispatcher
        .exposed_tools(&without, Utc::now())
        .await
        .expect("exposed");
    assert!(
        !exposed.iter().any(|name| name == "compose.run"),
        "{exposed:?}"
    );
    let calls = h.calls(&without).await;
    assert_eq!(calls[0].0, "compose.run");
    assert_eq!(calls[0].1, "rejected", "{calls:#?}");
    assert_eq!(calls[0].3["code"], json!("CAPABILITY_DENIED"));
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_may_not_call_a_control_tool_or_itself() {
    let Some(h) = harness("adp009_control").await else {
        return;
    };
    let run = h
        .compose(
            r#"
            const codes = {};
            for (const [tool, args] of [["work.complete", { summary: "done" }], ["user.ask", { prompt: "?", kind: "free_text" }], ["compose.run", { script: "return 1;" }]]) {
              try { call(tool, args); codes[tool] = "ran"; } catch (error) { codes[tool] = error.code; }
            }
            return codes;
            "#,
            json!({}),
            &[COMPOSE_GRANT, ("read.internal", "work", "*")],
        )
        .await;
    let cell = h.cell(&run).await;
    assert_eq!(cell["ok"], json!(true), "{cell:#}");
    assert_eq!(
        result_of(&cell),
        json!({"work.complete": "POLICY_DENIED", "user.ask": "POLICY_DENIED", "compose.run": "POLICY_DENIED"})
    );
    let calls = h.calls(&run).await;
    let refused: Vec<&str> = calls
        .iter()
        .filter(|c| c.1 == "rejected")
        .map(|c| c.0.as_str())
        .collect();
    assert_eq!(
        refused,
        vec!["work.complete", "user.ask", "compose.run"],
        "the refusals are recorded"
    );
    assert_eq!(
        h.effects(&run).await.len(),
        1,
        "only the outer compose.run reserved an effect"
    );
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_that_needs_an_approval_ends_the_script_and_nothing_reaches_the_server() {
    let Some(h) = harness("adp009_approval").await else {
        return;
    };
    *h.roles.lock().expect("roles") =
        ActorRoles::new(Some(TenantRole::Owner), Some(WorkspaceRole::Admin));
    // `hint` is not classified in the configuration, so it is the highest-risk default: tier 4, always asks.
    let run = h
        .compose(
            r#"
            log("before");
            try { call("mcp.demo.hint", {}); } catch (error) { log("caught", error.message); }
            try { call("mcp.demo.echo", { text: "after the approval was asked for" }); } catch (error) { }
            return "the script thinks it finished";
            "#,
            json!({}),
            &[COMPOSE_GRANT, ("data.upload.protected", "connector", "mcp.demo/hint"), ("read.external", "connector", "mcp.demo/echo")],
        )
        .await;
    let cell = h.cell(&run).await;
    assert_eq!(cell["ok"], json!(false), "{cell:#}");
    assert_eq!(cell["error"]["code"], json!("COMPOSE_TERMINATED"));
    assert_eq!(cell["error"]["condition"], json!("approval_required"));
    assert_eq!(cell["result_json"], Value::Null);
    // The script saw the throw and caught it; that changed nothing about how the cell ended.
    assert_eq!(
        cell["logs"],
        json!("before\ncaught terminal condition: approval_required\n")
    );
    assert_eq!(
        h.server_calls("hint"),
        0,
        "nothing reached the server while an approval is open"
    );
    assert_eq!(
        h.server_calls("echo"),
        0,
        "and the script did not go on to the next call"
    );
    let effects = h.effects(&run).await;
    assert!(
        effects.contains(&("data.upload.protected".to_string(), "PROPOSED".to_string())),
        "{effects:?}"
    );
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_outcome_inside_a_script_ends_it_and_the_same_call_is_refused_afterwards() {
    let Some(h) = harness("adp009_unknown").await else {
        return;
    };
    let grants = [
        COMPOSE_GRANT,
        ("content.upload", "connector", "mcp.demo/slow"),
    ];
    let script = r#"
        try { call("mcp.demo.slow", { seconds: 3 }); } catch (error) { }
        try { call("mcp.demo.slow", { seconds: 3 }); } catch (error) { }
        return "retried";
    "#;
    let run = h.compose(script, json!({}), &grants).await;
    let cell = h.cell(&run).await;
    assert_eq!(
        cell["error"]["condition"],
        json!("outcome_unknown"),
        "{cell:#}"
    );
    assert_eq!(
        h.server_calls("slow"),
        1,
        "the script did not retry past the doubt"
    );
    assert!(h
        .effects(&run)
        .await
        .contains(&("content.upload".to_string(), "OUTCOME_UNKNOWN".to_string())));

    // A second script proposing the same call is refused by the ledger while the record is unresolved.
    let again = h
        .compose(
            r#"try { call("mcp.demo.slow", { seconds: 3 }); return "sent"; } catch (error) { return error.code; }"#,
            json!({}),
            &grants,
        )
        .await;
    let cell = h.cell(&again).await;
    assert_eq!(cell["ok"], json!(true), "{cell:#}");
    assert_eq!(result_of(&cell), json!("CONFLICT_IDEMPOTENCY_MISMATCH"));
    assert_eq!(h.server_calls("slow"), 1, "never sent a second time");
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_calls_makes_exactly_the_budget_of_rows_and_effects() {
    let Some(h) = harness("adp009_budget").await else {
        return;
    };
    let run = h
        .compose(
            r#"for (let i = 0; i < 100; i += 1) { try { call("mcp.demo.echo", { text: "n" + i }); } catch (e) { } } return "done";"#,
            json!({ "max_host_calls": 5 }),
            &[COMPOSE_GRANT, ("read.external", "connector", "mcp.demo/echo")],
        )
        .await;
    let cell = h.cell(&run).await;
    assert_eq!(
        cell["error"]["condition"],
        json!("host_call_budget"),
        "{cell:#}"
    );
    let calls = h.calls(&run).await;
    assert_eq!(
        calls.iter().filter(|c| c.0 == "mcp.demo.echo").count(),
        5,
        "exactly the budget of calls was recorded"
    );
    assert_eq!(
        h.effects(&run).await.len(),
        6,
        "five calls and the compose.run read"
    );
    assert_eq!(h.server_calls("echo"), 5);
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_that_talks_about_other_tools_gets_no_more_than_its_projection() {
    let Some(h) = harness("adp009_unregistered").await else {
        return;
    };
    let run = h
        .compose(
            r#"
            const out = {};
            const attempts = [
              ["mcp.demo.nothing", {}], ["fs.write", { path: "x", content: "y", content_digest: "d" }], ["fs.read", { path: "x" }],
              ["webdev.preview_scopes", {}], ["mcp.other.echo", { text: "t" }],
            ];
            for (const [tool, args] of attempts) {
              try { call(tool, args); out[tool] = "ran"; } catch (error) { out[tool] = error.code; }
            }
            return out;
            "#,
            json!({}),
            &[COMPOSE_GRANT],
        )
        .await;
    let cell = h.cell(&run).await;
    assert_eq!(cell["ok"], json!(true), "{cell:#}");
    let out = result_of(&cell);
    assert_eq!(
        out["mcp.demo.nothing"],
        json!("VALIDATION_SCHEMA"),
        "an unregistered tool"
    );
    assert_eq!(out["mcp.other.echo"], json!("VALIDATION_SCHEMA"));
    assert_eq!(
        out["webdev.preview_scopes"],
        json!("TOOL_NOT_MODEL_VISIBLE"),
        "a host-only tool is not reachable from a script"
    );
    assert_eq!(out["fs.write"], json!("CAPABILITY_DENIED"));
    assert_eq!(out["fs.read"], json!("CAPABILITY_DENIED"));
    assert_eq!(h.effects(&run).await.len(), 1, "only the compose.run read");
    h.finish().await;
}
