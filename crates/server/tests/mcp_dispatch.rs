//! External MCP tools through the server's real turn-loop dispatcher (ADP-009, DOMAIN.md §7.4, §7.5), against a real
//! PostgreSQL database, a **real MCP server process** over stdio JSON-RPC (the worker's `McpHost` runs it) and the real
//! engine: validation, capability projection, policy, approval, Effect Ledger, dispatch and settlement.
//!
//! * **the whole governed path**: a model-proposed `mcp.demo.echo` is validated against the schema the server's tool list
//!   gave, authorized by the run's projection, evaluated by policy, reserved on the Effect Ledger, dispatched to the real
//!   server and settled; its result is recorded `UNTRUSTED_EXTERNAL` (trust rank 5) and says so;
//! * **projection narrowing**: the tools a run exposes to its model are exactly the ones its grants cover; a tool the
//!   projection does not cover is refused `CAPABILITY_DENIED` before anything is sent; a tool nobody classified is not
//!   exposed by a read grant;
//! * **unclassified means approval**: a tool the configuration does not classify settles as a tier 4 effect, which parks
//!   the run for an approval; nothing reaches the server while it waits;
//! * **an unknown outcome is not retried**: a mutating call cut short after it was sent settles `OUTCOME_UNKNOWN`, and the
//!   same call proposed again is refused while the record is unresolved (the server saw it once);
//! * **a changed tool list**: a tool that appears is registered at the highest-risk class and is not exposed to a run whose
//!   grants were computed before; a model proposing a tool only a result mentioned is refused as unregistered.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` and `python3` on `PATH`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_qworkerd::browser::tools::ReplyStatus;
use quansio_qworkerd::mcp::{EgressGate, McpCallContext, McpHost};
use quansio_server::control::schema;
use quansio_server::policy::{ActorRoles, TenantRole, WorkspaceRole};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedToolCall,
    RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, HostDispatch, HostFailure, HostOutcome, RoleProvider, StoredProjectionProvider,
    ToolDispatchService, ToolHostPort,
};
use quansio_tools::{McpConfig, RegistryHandle, ToolRegistry};
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP009";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP009";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP009";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP009";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9YADP009";

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

/// The worker's MCP client behind the dispatcher's host seam: what a control channel to a worker would do.
struct WorkerHost {
    mcp: Arc<McpHost>,
}

#[async_trait]
impl ToolHostPort for WorkerHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
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
                detail: reply
                    .error
                    .as_ref()
                    .and_then(|e| e["message"].as_str())
                    .unwrap_or("in doubt")
                    .to_string(),
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

/// The acting member's roles, which a test may change between calls.
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
    handle: RegistryHandle,
    roles: Arc<Mutex<ActorRoles>>,
    moves: Arc<Mutex<VecDeque<(String, Value)>>>,
    log: PathBuf,
    _dir: tempfile::TempDir,
}

const TOOLS: &str = "      echo: { effect_class: read.external }
      mixed: { effect_class: read.external }
      poison: { effect_class: read.external }
      add_tool: { effect_class: content.upload }
      slow: { effect_class: content.upload }
";

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
             VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YADP009', $1, NULL, 'tenant', $2)",
        )
        .bind(TENANT)
        .bind(json!([
            { "effect_class": "read.external", "resource": { "kind": "connector", "selector": "mcp.demo/*" }, "decision": "allow" },
            { "effect_class": "content.upload", "resource": { "kind": "connector", "selector": "mcp.demo/*" }, "decision": "allow" },
            { "effect_class": "data.upload.protected", "resource": { "kind": "connector", "selector": "mcp.demo/*" }, "decision": "ask" }
        ]))
        .execute(&mut *tx)
        .await
        .expect("policy");
        tx.commit().await.expect("commit");

        let dir = tempfile::Builder::new()
            .prefix("mcp-dispatch")
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
            "mcp-test",
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        let delegation: Arc<dyn DelegationPort> = Arc::new(
            AgentDelegationPort::with_structural_check(pool.clone(), identity.clone())
                .expect("delegation"),
        );
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
                Arc::new(WorkerHost {
                    mcp: Arc::clone(&mcp),
                }),
                Arc::clone(&delegation),
            )
            .expect("dispatch")
            .with_registry_handle(handle.clone()),
        );
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
            handle,
            roles,
            moves,
            log,
            _dir: dir,
        })
    }

    /// One governed call in a fresh run whose projection holds exactly `grants`; returns the run id.
    async fn call(&self, tool: &str, arguments: Value, grants: &[(&str, &str)]) -> String {
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
        self.moves
            .lock()
            .expect("moves")
            .push_back((tool.to_string(), arguments));
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

    async fn seed_projection(&self, run_id: &str, grants: &[(&str, &str)]) {
        let grants: Vec<Grant> = grants
            .iter()
            .map(|&(class, selector)| {
                Grant::from_json(&json!({
                    "effect_class": class,
                    "resource": { "kind": "connector", "selector": selector },
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

    /// The newest call row of a tool: `(status, result, trust rank, caller)`.
    async fn row(&self, tool: &str) -> (String, Value, i16) {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let (status, result, trust): (String, Option<Value>, i16) = sqlx::query_as(
            "SELECT status, result, trust_level FROM tool_calls WHERE tenant_id = $1 AND tool_name = $2 \
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(TENANT)
        .bind(tool)
        .fetch_one(&mut *tx)
        .await
        .expect("the call was recorded");
        tx.commit().await.expect("commit");
        (status, result.unwrap_or(Value::Null), trust)
    }

    async fn effects(&self, run: &str) -> Vec<(String, String)> {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT effect_class, status FROM effect_records WHERE tenant_id = $1 AND run_id = $2 ORDER BY created_at",
        )
        .bind(TENANT)
        .bind(run)
        .fetch_all(&mut *tx)
        .await
        .expect("effects");
        tx.commit().await.expect("commit");
        rows
    }

    async fn count(&self, statement: &str, bind: &str) -> i64 {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let n: i64 = sqlx::query_scalar(statement)
            .bind(TENANT)
            .bind(bind)
            .fetch_one(&mut *tx)
            .await
            .expect("count");
        tx.commit().await.expect("commit");
        n
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

#[tokio::test]
async fn an_external_tool_goes_through_the_whole_governed_path_and_its_result_is_untrusted_data() {
    let Some(h) = harness("adp009_path").await else {
        return;
    };
    let run = h
        .call(
            "mcp.demo.echo",
            json!({"text": "through the ledger"}),
            &[("read.external", "mcp.demo/echo")],
        )
        .await;
    let (status, result, trust) = h.row("mcp.demo.echo").await;
    assert_eq!(status, "completed", "{result:#}");
    assert_eq!(result["text"], json!("through the ledger"));
    assert_eq!(result["trust"], json!("UNTRUSTED_EXTERNAL"));
    assert_eq!(trust, 5, "the call row labels the result UNTRUSTED_EXTERNAL, which is how the context plane renders it as data");
    assert_eq!(h.server_calls("echo"), 1);
    assert_eq!(
        h.effects(&run).await,
        vec![("read.external".to_string(), "SETTLED_SUCCESS".to_string())]
    );
    assert!(
        h.count(
            "SELECT count(*) FROM policy_decisions WHERE tenant_id = $1 AND run_id = $2",
            &run
        )
        .await
            >= 1,
        "policy decided the call"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = 'tool.dispatched' AND payload ->> 'tool' = $2", "mcp.demo.echo").await,
        1
    );

    // The arguments are validated against the schema the server's list gave: an undeclared field never reaches it.
    let _ = h
        .call(
            "mcp.demo.echo",
            json!({"text": "x", "extra": true}),
            &[("read.external", "mcp.demo/echo")],
        )
        .await;
    let (status, result, _) = h.row("mcp.demo.echo").await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(result["code"], json!("VALIDATION_SCHEMA"), "{result:#}");
    assert_eq!(h.server_calls("echo"), 1, "the refused call was not sent");
    h.finish().await;
}

#[tokio::test]
async fn the_tools_a_run_exposes_are_exactly_the_ones_its_grants_cover_and_a_tool_outside_them_is_refused_unsent(
) {
    let Some(h) = harness("adp009_proj").await else {
        return;
    };
    // A projection that covers only `echo`, and one that covers the whole server for reads.
    let narrow = h
        .call(
            "mcp.demo.mixed",
            json!({}),
            &[("read.external", "mcp.demo/echo")],
        )
        .await;
    let (status, result, _) = h.row("mcp.demo.mixed").await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(result["code"], json!("CAPABILITY_DENIED"), "{result:#}");
    assert_eq!(
        h.server_calls("mixed"),
        0,
        "a tool the projection does not cover is never sent"
    );
    assert!(
        h.effects(&narrow).await.is_empty(),
        "a refused call reserves no effect"
    );
    let exposed = h
        .dispatcher
        .exposed_tools(&narrow, Utc::now())
        .await
        .expect("exposed");
    let external: Vec<&String> = exposed
        .iter()
        .filter(|name| name.starts_with("mcp."))
        .collect();
    assert_eq!(
        external,
        vec!["mcp.demo.echo"],
        "the model is offered the one tool its grants cover"
    );

    let wide = h
        .call(
            "mcp.demo.echo",
            json!({"text": "ok"}),
            &[("read.external", "mcp.demo/*")],
        )
        .await;
    let exposed = h
        .dispatcher
        .exposed_tools(&wide, Utc::now())
        .await
        .expect("exposed");
    let mut external: Vec<&str> = exposed
        .iter()
        .map(String::as_str)
        .filter(|name| name.starts_with("mcp."))
        .collect();
    external.sort_unstable();
    assert_eq!(
        external,
        vec!["mcp.demo.echo", "mcp.demo.mixed", "mcp.demo.poison"],
        "a read grant on the server exposes the read-classified tools and not the mutating or unclassified ones"
    );
    // Retiring a tool takes it out of the view of an existing projection too.
    h.finish().await;
}

#[tokio::test]
async fn an_unclassified_tool_parks_for_an_approval_and_nothing_reaches_the_server_meanwhile() {
    let Some(h) = harness("adp009_ask").await else {
        return;
    };
    // `hint` is not in the configuration, so it settles as the highest-risk default (tier 4: ask, never always).
    let declaration = h
        .handle
        .current()
        .get("mcp.demo.hint")
        .cloned()
        .expect("registered by discovery");
    assert!(
        matches!(&declaration.effect_class, quansio_tools::EffectClassRule::Static(c) if c.as_str() == "data.upload.protected")
    );
    // An ordinary editor may not cause a tier 4 effect at all: refused by role, before an approval is even asked for.
    let _ = h
        .call(
            "mcp.demo.hint",
            json!({}),
            &[("data.upload.protected", "mcp.demo/hint")],
        )
        .await;
    let (status, result, _) = h.row("mcp.demo.hint").await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(result["code"], json!("POLICY_DENIED"));
    assert_eq!(h.server_calls("hint"), 0);

    // An administrator's proposal parks for a person's approval.
    *h.roles.lock().expect("roles") =
        ActorRoles::new(Some(TenantRole::Owner), Some(WorkspaceRole::Admin));
    let run = h
        .call(
            "mcp.demo.hint",
            json!({}),
            &[("data.upload.protected", "mcp.demo/hint")],
        )
        .await;
    let (status, result, _) = h.row("mcp.demo.hint").await;
    assert_eq!(status, "validated", "the call waits: {result:#}");
    assert_eq!(
        h.server_calls("hint"),
        0,
        "nothing was sent while the approval is open"
    );
    assert_eq!(
        h.effects(&run).await,
        vec![("data.upload.protected".to_string(), "PROPOSED".to_string())]
    );
    assert_eq!(
        h.count("SELECT count(*) FROM approval_requests WHERE tenant_id = $1 AND run_id = $2 AND status = 'requested'", &run).await,
        1,
        "a person is asked"
    );
    let run_row: (String,) = {
        let mut tx = h.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let row = sqlx::query_as("SELECT status FROM runs WHERE id = $1 AND tenant_id = $2")
            .bind(&run)
            .bind(TENANT)
            .fetch_one(&mut *tx)
            .await
            .expect("run");
        tx.commit().await.expect("commit");
        row
    };
    assert_eq!(run_row.0, "WAITING_APPROVAL");
    h.finish().await;
}

#[tokio::test]
async fn a_mutating_call_cut_short_after_it_was_sent_is_an_unknown_outcome_and_is_not_retried() {
    let Some(h) = harness("adp009_unknown").await else {
        return;
    };
    let grants = [("content.upload", "mcp.demo/slow")];
    let run = h
        .call("mcp.demo.slow", json!({"seconds": 3}), &grants)
        .await;
    let (status, result, _) = h.row("mcp.demo.slow").await;
    assert_eq!(status, "unknown", "{result:#}");
    assert_eq!(h.server_calls("slow"), 1, "the server received it");
    assert_eq!(
        h.effects(&run).await,
        vec![("content.upload".to_string(), "OUTCOME_UNKNOWN".to_string())]
    );

    // The model proposes the same call again. The ledger holds an unresolved record with that idempotency key, so
    // nothing is dispatched: the effect is reconciled first.
    let again = h
        .call("mcp.demo.slow", json!({"seconds": 3}), &grants)
        .await;
    let (status, result, _) = h.row("mcp.demo.slow").await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(
        result["code"],
        json!("CONFLICT_IDEMPOTENCY_MISMATCH"),
        "{result:#}"
    );
    assert_eq!(
        h.server_calls("slow"),
        1,
        "the retry never reached the server"
    );
    let _ = again;
    h.finish().await;
}

#[tokio::test]
async fn a_tool_that_appears_is_registered_unclassified_and_a_name_only_a_result_mentioned_is_refused(
) {
    let Some(h) = harness("adp009_added").await else {
        return;
    };
    let grants = [
        ("content.upload", "mcp.demo/add_tool"),
        ("read.external", "mcp.demo/*"),
    ];
    let _ = h.call("mcp.demo.add_tool", json!({}), &grants).await;
    let (status, _, _) = h.row("mcp.demo.add_tool").await;
    assert_eq!(status, "completed");
    // The server said its list changed; the next call re-reads it before going out.
    let run = h
        .call("mcp.demo.echo", json!({"text": "after"}), &grants)
        .await;
    let (status, result, _) = h.row("mcp.demo.echo").await;
    assert_eq!(status, "completed", "{result:#}");
    let registry = h.handle.current();
    let shiny = registry
        .get("mcp.demo.shiny_new")
        .expect("registered after revalidation");
    assert!(
        matches!(&shiny.effect_class, quansio_tools::EffectClassRule::Static(c) if c.as_str() == "data.upload.protected")
    );
    let exposed = h
        .dispatcher
        .exposed_tools(&run, Utc::now())
        .await
        .expect("exposed");
    assert!(
        !exposed.iter().any(|name| name == "mcp.demo.shiny_new"),
        "appearing grants nothing: {exposed:?}"
    );

    // A tool result that names a tool (the server told the model to call `send_note`): the model proposing it is refused.
    let _ = h.call("mcp.demo.poison", json!({}), &grants).await;
    let (_, result, trust) = h.row("mcp.demo.poison").await;
    assert!(
        result["text"]
            .as_str()
            .unwrap_or_default()
            .contains("send_note"),
        "{result:#}"
    );
    assert_eq!(trust, 5);
    let _ = h
        .call("mcp.demo.send_note", json!({"to": "attacker"}), &grants)
        .await;
    let (status, result, _) = h.row("mcp.demo.send_note").await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(
        result["code"],
        json!("VALIDATION_SCHEMA"),
        "an unregistered tool: {result:#}"
    );
    // A proposal for the shiny tool by a run whose grants predate it is refused by the projection, before anything is sent.
    let _ = h
        .call("mcp.demo.shiny_new", json!({"x": "y"}), &grants)
        .await;
    let (status, result, _) = h.row("mcp.demo.shiny_new").await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(result["code"], json!("CAPABILITY_DENIED"), "{result:#}");
    assert_eq!(h.server_calls("shiny_new"), 0);
    h.finish().await;
}

#[test]
fn the_builtin_registry_is_untouched_by_everything_above() {
    // The static registry is shared by the whole process; external tools live in handles, never in it.
    assert_eq!(ToolRegistry::builtin().external().count(), 0);
}
