//! RUN-008 / RUN-011: how a model claims completion, and what the served build does with it.
//!
//! A model claims completion with the `work.complete` control tool (DOMAIN.md §5.6). The served
//! tests post a message through the deployable's router, whose turn runs through the real Python
//! intelligence process: the Context plane, the model gateway and this checkout's dev stub
//! provider. The user turn scripts the claim with `[[stub:call work__complete {...}]]`, which the
//! stub honours only when the tool is offered, and the claim summary scripts the independent
//! verifier's answer with `[[stub:verdict agree|disagree]]`. The claim passes the ToolCall
//! protocol, the composed `ContractVerifier` evaluates the WorkNode's CompletionContract, and the
//! plane's `VerifyCompletion` gives the semantic verdict — no seam is stubbed on the Rust side.
//!
//! The engine-level tests drive the same engine, dispatcher and verifier with a conformance model
//! to prove the per-turn bound on rejected claims, the refusals before verification and recovery
//! of a run stopped mid-verification.
//!
//! Real boundary: none (the providers are the deterministic dev and conformance stubs).
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `uv` and `python3`; without them each test prints a
//! `BLOCKED_EXTERNAL` marker and returns.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::intelligence::model_source::{GatewayModelSource, ModelCallSettings};
use quansio_server::intelligence::IntelligenceClient;
use quansio_server::policy::{ActorRoles, TenantRole, TrustLevel, WorkspaceRole};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::context_bridge::GatewayContextBridge;
use quansio_server::runtime::state_machine::{
    Budget, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedToolCall,
    RecoveryOutcome, Run, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity,
    TurnInput, TurnOutcome,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, ProposalTrustSource, RoleProvider, StoredProjectionProvider, ToolDispatchService,
    UnavailableToolHost,
};
use quansio_server::runtime::verification::ContractVerifier;
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YRXN008";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YRXN008";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YRXN008";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YRXN008";

// ---------------------------------------------------------------------------------------
// Shared fixture
// ---------------------------------------------------------------------------------------

/// A tenant whose policy offers the run `fs.read` and, on `{kind: work}`, `work.complete`.
async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = common::admin_url().map(|_| common::scratch_name(prefix))?;
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "read.internal", "resource": { "kind": "work", "selector": "*" }, "decision": "allow" }
    ]);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YRXN008', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    Some((name, pool))
}

async fn set_contract(pool: &PgPool, contract: Value) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("UPDATE work_nodes SET completion_contract = $2 WHERE id = $1")
        .bind(NODE)
        .bind(contract)
        .execute(&mut *tx)
        .await
        .expect("contract");
    tx.commit().await.expect("commit");
}

async fn scalar<T>(pool: &PgPool, sql: &str, bind: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let value: T = sqlx::query_scalar(sql)
        .bind(bind)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    tx.commit().await.expect("commit");
    value
}

async fn run_status(pool: &PgPool, run_id: &str) -> String {
    scalar(pool, "SELECT status FROM runs WHERE id = $1", run_id).await
}

/// `(tool_name, status, result)` of the run's tool calls, oldest first.
async fn tool_calls(pool: &PgPool, run_id: &str) -> Vec<(String, String, String, Value)> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query_as::<_, (String, String, String, Option<Value>)>(
        "SELECT c.id, c.tool_name, c.status, c.result FROM tool_calls c \
         JOIN steps s ON s.id = c.step_id JOIN turns t ON t.id = s.turn_id \
         WHERE t.run_id = $1 ORDER BY c.created_at, c.id",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await
    .expect("tool calls");
    tx.commit().await.expect("commit");
    rows.into_iter()
        .map(|(id, name, status, result)| (id, name, status, result.unwrap_or(Value::Null)))
        .collect()
}

/// `(kind, status)` of the run's steps, in order.
async fn steps(pool: &PgPool, run_id: &str) -> Vec<(String, String)> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT s.kind, s.status FROM steps s JOIN turns t ON t.id = s.turn_id \
         WHERE t.run_id = $1 ORDER BY t.seq, s.seq",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await
    .expect("steps");
    tx.commit().await.expect("commit");
    rows
}

async fn run_events(pool: &PgPool, run_id: &str) -> Vec<String> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT type FROM runtime_events WHERE aggregate_id = $1 ORDER BY sequence",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    rows
}

// ---------------------------------------------------------------------------------------
// Served: PostMessage → real intelligence process → work.complete → verifier
// ---------------------------------------------------------------------------------------

struct Served {
    name: String,
    pool: PgPool,
    addr: std::net::SocketAddr,
    _service: common::intelligence::Service,
    _stub: common::intelligence::RepoStub,
}

async fn serve(prefix: &str, contract: Value) -> Option<Served> {
    let stub = common::intelligence::repo_stub().await?;
    let service = common::intelligence::start_with_provider(Some(&stub.url)).await?;
    let Some((name, pool)) = prepare(prefix).await else {
        common::blocked_marker();
        return None;
    };
    set_contract(&pool, contract).await;
    // The deployable's composition: the gateway model source over the Python process, and the
    // plane's client, which gives completion claims their independent semantic verifier.
    let settings = ModelCallSettings::load().expect("config/intelligence.yaml");
    let client = IntelligenceClient::connect_lazy(&service.endpoint).expect("endpoint");
    let context = Arc::new(GatewayContextBridge::new(client.clone(), settings.deadline));
    let state = quansio_server::api::ApiState::new(pool.clone())
        .expect("api")
        .with_intelligence(client.clone())
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
        name,
        pool,
        addr,
        _service: service,
        _stub: stub,
    })
}

impl Served {
    async fn post(&self, command_id: &str, content: &str) -> Value {
        self.post_in(command_id, content, None).await
    }

    async fn post_in(&self, command_id: &str, content: &str, thread_id: Option<&str>) -> Value {
        let mut params = json!({ "workspace_id": WORKSPACE, "content": content });
        if let Some(thread_id) = thread_id {
            params["thread_id"] = json!(thread_id);
        }
        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/commands/PostMessage", self.addr))
            .header("x-quansio-tenant", TENANT)
            .header("x-quansio-user", USER)
            .json(&json!({ "command_id": command_id, "params": params }))
            .send()
            .await
            .expect("post");
        let status = response.status();
        let body: Value = response.json().await.expect("json");
        assert_eq!(status, 200, "{body}");
        body["result"].clone()
    }

    async fn finish(self) {
        common::drop_pool(&self.pool, &self.name).await;
    }
}

fn claim(summary: &str) -> String {
    format!(
        "Deliver the report. [[stub:call work__complete {}]]",
        json!({ "summary": summary })
    )
}

#[tokio::test]
async fn a_claim_whose_contract_passes_succeeds_the_run_through_the_independent_verifier() {
    let Some(served) = serve(
        "run008_served_pass",
        json!({
            "deterministic_checks": [{ "kind": "effects_settled", "effect_classes": ["read.internal"] }],
            "semantic_verification": { "required": true, "rubric_id": "rubric_report", "independent_model": false }
        }),
    )
    .await
    else {
        return;
    };
    let result = served
        .post(
            "cmd_01J8Z3K6F1N8VQ2X5W9YRXN001",
            &claim("the report is delivered [[stub:verdict agree]]"),
        )
        .await;
    let run_id = result["run_id"].as_str().expect("run").to_string();
    assert_eq!(result["turn"]["kind"], "succeeded", "{result}");
    assert_eq!(run_status(&served.pool, &run_id).await, "SUCCEEDED");

    // The claim went through the ToolCall protocol and carries the verdict as its result.
    let calls = tool_calls(&served.pool, &run_id).await;
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].1, "work.complete");
    assert_eq!(calls[0].2, "completed");
    assert_eq!(
        calls[0].3,
        json!({ "verified": true, "run_status": "SUCCEEDED" })
    );
    // One model call, the claim's tool step, and a completed verify step.
    assert_eq!(
        steps(&served.pool, &run_id).await,
        vec![
            ("model_call".to_string(), "completed".to_string()),
            ("tool_call".to_string(), "completed".to_string()),
            ("verify".to_string(), "completed".to_string()),
        ]
    );
    let events = run_events(&served.pool, &run_id).await;
    let verifying = events.iter().position(|event| event == "run.verifying");
    let succeeded = events.iter().position(|event| event == "run.succeeded");
    assert!(
        verifying.is_some() && verifying < succeeded,
        "the run passed through VERIFYING: {events:?}"
    );
    served.finish().await;
}

#[tokio::test]
async fn a_failing_contract_returns_its_feedback_to_the_model_and_the_turn_continues() {
    let Some(served) = serve(
        "run008_served_fail",
        json!({ "deterministic_checks": [{ "kind": "artifact_exists", "artifact_role": "report", "min_versions": 1 }] }),
    )
    .await
    else {
        return;
    };
    let result = served
        .post(
            "cmd_01J8Z3K6F1N8VQ2X5W9YRXN002",
            &claim("the report is delivered"),
        )
        .await;
    let run_id = result["run_id"].as_str().expect("run").to_string();

    // The model's claim alone did not complete the work: the run is still open.
    assert_eq!(run_status(&served.pool, &run_id).await, "RUNNING");
    let calls = tool_calls(&served.pool, &run_id).await;
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (claim_id, tool, _, verdict) = &calls[0];
    assert_eq!(tool, "work.complete");
    assert_eq!(verdict["verified"], false, "{verdict}");
    assert_eq!(verdict["run_status"], "RUNNING");
    let feedback = verdict["feedback"].as_str().expect("feedback");
    assert!(
        feedback.contains("artifact_exists") && feedback.contains("report"),
        "the feedback names what is missing: {feedback}"
    );

    // The loop continued: a second model call read the verdict as the claim's result and the
    // stub answered it, and that answer is the conversation's reply.
    assert_eq!(result["turn"]["kind"], "completed", "{result}");
    assert_eq!(
        result["turn"]["assistant_text"],
        "stub completion: 1 tool result(s) received"
    );
    assert!(result["reply_message_id"].is_string(), "{result}");
    assert_eq!(
        steps(&served.pool, &run_id).await,
        vec![
            ("model_call".to_string(), "completed".to_string()),
            ("tool_call".to_string(), "completed".to_string()),
            ("verify".to_string(), "failed".to_string()),
            ("model_call".to_string(), "completed".to_string()),
        ]
    );
    let segments: Value = scalar(
        &served.pool,
        "SELECT segments FROM context_projections WHERE run_id = $1 ORDER BY created_at DESC LIMIT 1",
        &run_id,
    )
    .await;
    assert!(
        segments
            .as_array()
            .expect("segments")
            .iter()
            .any(|segment| segment["source"] == claim_id.as_str()),
        "the second call's context carried the claim's result: {segments}"
    );
    served.finish().await;
}

#[tokio::test]
async fn an_independent_verifier_that_disagrees_keeps_the_work_incomplete() {
    let Some(served) = serve(
        "run008_served_disagree",
        json!({
            "deterministic_checks": [{ "kind": "effects_settled", "effect_classes": ["read.internal"] }],
            "semantic_verification": { "required": true, "rubric_id": "rubric_report", "independent_model": false }
        }),
    )
    .await
    else {
        return;
    };
    let result = served
        .post(
            "cmd_01J8Z3K6F1N8VQ2X5W9YRXN003",
            &claim("the report is delivered [[stub:verdict disagree]]"),
        )
        .await;
    let run_id = result["run_id"].as_str().expect("run").to_string();
    assert_eq!(run_status(&served.pool, &run_id).await, "RUNNING");
    let calls = tool_calls(&served.pool, &run_id).await;
    let feedback = calls[0].3["feedback"]
        .as_str()
        .expect("feedback")
        .to_string();
    assert!(
        feedback.contains("disagrees")
            && feedback.contains("stub verdict: the claim is not supported"),
        "the verifier's own critique is the feedback: {feedback}"
    );
    served.finish().await;
}

#[tokio::test]
async fn a_verifier_that_is_the_claimant_cannot_certify_a_contract_requiring_independence() {
    // The test catalog has one route, so the verifier would judge with the claimant's model.
    let Some(served) = serve(
        "run008_served_independent",
        json!({
            "deterministic_checks": [{ "kind": "effects_settled", "effect_classes": ["read.internal"] }],
            "semantic_verification": { "required": true, "rubric_id": "rubric_report", "independent_model": true }
        }),
    )
    .await
    else {
        return;
    };
    let result = served
        .post(
            "cmd_01J8Z3K6F1N8VQ2X5W9YRXN004",
            &claim("the report is delivered [[stub:verdict agree]]"),
        )
        .await;
    let run_id = result["run_id"].as_str().expect("run").to_string();
    assert_eq!(run_status(&served.pool, &run_id).await, "RUNNING");
    let calls = tool_calls(&served.pool, &run_id).await;
    let feedback = calls[0].3["feedback"]
        .as_str()
        .expect("feedback")
        .to_string();
    // Refused as the verifier's own work: the runtime named the claimant by the model its
    // ledger recorded, and that is the model the verifier would have judged with.
    assert!(
        feedback.contains("POLICY_DENIED")
            && feedback.contains("cannot verify its own work (INDEPENDENCE_UNPROVABLE)"),
        "the claimant's recorded model was named and refused: {feedback}"
    );
    let claimant: String = scalar(
        &served.pool,
        "SELECT r.model_id FROM model_calls c JOIN model_routes r ON r.id = c.route_id \
         WHERE c.run_id = $1 LIMIT 1",
        &run_id,
    )
    .await;
    assert_eq!(claimant, common::intelligence::STUB_MODEL);
    served.finish().await;
}

#[tokio::test]
async fn a_plain_conversation_is_never_offered_a_completion_claim_and_stays_open() {
    // The workspace's conversation node certifies nothing, even though policy grants the tool.
    let Some(served) = serve("run008_served_chat", json!({})).await else {
        return;
    };
    let first = served
        .post(
            "cmd_01J8Z3K6F1N8VQ2X5W9YRXN005",
            &claim("the conversation is over"),
        )
        .await;
    let run_id = first["run_id"].as_str().expect("run").to_string();
    // The stub calls a scripted tool only when it is offered: it answered with text instead.
    assert_eq!(first["turn"]["kind"], "completed", "{first}");
    assert!(first["reply_message_id"].is_string(), "{first}");
    assert!(tool_calls(&served.pool, &run_id).await.is_empty());
    assert_eq!(run_status(&served.pool, &run_id).await, "RUNNING");

    // The thread carries on: the next message is answered in the same thread.
    let second = served
        .post_in(
            "cmd_01J8Z3K6F1N8VQ2X5W9YRXN006",
            "And one more thing.",
            first["thread_id"].as_str(),
        )
        .await;
    assert_eq!(second["thread_id"], first["thread_id"], "{second}");
    assert!(second["reply_message_id"].is_string(), "{second}");
    served.finish().await;
}

// ---------------------------------------------------------------------------------------
// Engine level: the real engine, dispatcher and verifier with a conformance model
// ---------------------------------------------------------------------------------------

/// A model that proposes scripted rounds, then repeats its last one.
#[derive(Clone)]
struct ScriptedModel {
    rounds: Arc<Mutex<VecDeque<ModelProposal>>>,
    last: Arc<Mutex<ModelProposal>>,
}

impl ScriptedModel {
    fn always(proposal: ModelProposal) -> Self {
        Self {
            rounds: Arc::new(Mutex::new(VecDeque::new())),
            last: Arc::new(Mutex::new(proposal)),
        }
    }
}

#[async_trait]
impl ModelProposalSource for ScriptedModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let next = self.rounds.lock().expect("rounds").pop_front();
        Ok(next.unwrap_or_else(|| self.last.lock().expect("last").clone()))
    }
}

fn claiming(args: Value) -> ModelProposal {
    ModelProposal {
        tool_calls: vec![ProposedToolCall::new("c1", "work.complete", args)],
        ..ModelProposal::default()
    }
}

struct AgentTrust;

#[async_trait]
impl ProposalTrustSource for AgentTrust {
    async fn proposal_trust(&self, _run_id: &str, _turn_id: &str) -> Option<TrustLevel> {
        Some(TrustLevel::AgentGenerated)
    }
}

struct Member;

#[async_trait]
impl RoleProvider for Member {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
            user_id: Some(USER.to_string()),
        }))
    }
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity::system(
        TENANT,
        "completion-claims-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

/// The served composition's engine, with a conformance model instead of the gateway.
fn engine(pool: &PgPool, model: ScriptedModel) -> RuntimeEngine {
    let identity = identity();
    let delegation = Arc::new(
        AgentDelegationPort::with_structural_check(pool.clone(), identity.clone())
            .expect("delegation"),
    );
    let dispatcher = ToolDispatchService::new(
        pool.clone(),
        identity.clone(),
        Arc::new(StoredProjectionProvider::new(pool.clone(), identity.clone()).expect("provider")),
        Arc::new(Member),
        Arc::new(UnavailableToolHost),
        delegation,
    )
    .expect("dispatcher")
    .with_proposal_trust(Arc::new(AgentTrust));
    RuntimeEngine::new(pool.clone(), identity.clone())
        .expect("engine")
        .with_model_source(Arc::new(model))
        .with_tool_dispatch(Arc::new(dispatcher))
        .with_verification(Arc::new(
            ContractVerifier::new(pool.clone(), identity).expect("verifier"),
        ))
}

async fn agent_thread(pool: &PgPool) -> CanonicalId {
    let id = CanonicalId::generate(Prefix::AgentThread, &mut UlidGenerator::new());
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(id.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    tx.commit().await.expect("commit");
    id
}

/// A started run whose projection holds `grants` (`(class, kind, selector)`).
async fn started_run(
    pool: &PgPool,
    engine: &RuntimeEngine,
    budget: Budget,
    grants: &[(&str, &str, &str)],
) -> Run {
    let run = engine
        .create_run(
            NewRun::new(
                WORKSPACE,
                CanonicalId::parse_typed(NODE, Prefix::WorkNode).expect("node"),
                agent_thread(pool).await,
                RunTriggerKind::Manual,
            )
            .with_budget(budget),
        )
        .await
        .expect("run");
    let run = engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    let run = engine.start(&run.id, run.generation).await.expect("start");
    let grants: Vec<Grant> = grants
        .iter()
        .map(|(class, kind, selector)| {
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
        ProjectionSubject::new(SubjectKind::Run, run.id.to_string()),
        Vec::new(),
        grants,
        chrono::Utc::now(),
        Some(chrono::Utc::now() + chrono::Duration::hours(1)),
    )
    .expect("projection");
    let mut tx = pool.begin().await.expect("begin");
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
    .bind(run.id.to_string())
    .bind(serde_json::to_value(&projection.inputs).expect("inputs"))
    .bind(serde_json::to_value(&projection.grants).expect("grants"))
    .bind(projection.inputs_digest.to_string())
    .bind(projection.computed_at)
    .bind(projection.expires_at)
    .execute(&mut *tx)
    .await
    .expect("projection row");
    tx.commit().await.expect("commit");
    run
}

const CLAIM_GRANT: (&str, &str, &str) = ("read.internal", "work", "*");

async fn turn(engine: &RuntimeEngine, run: &Run) -> TurnOutcome {
    engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn")
}

#[tokio::test]
async fn rejected_claims_are_bounded_per_turn_and_leave_the_work_incomplete() {
    let Some((name, pool)) = prepare("run008_bound").await else {
        common::blocked_marker();
        return;
    };
    set_contract(
        &pool,
        json!({ "deterministic_checks": [{ "kind": "artifact_exists", "artifact_role": "report", "min_versions": 1 }] }),
    )
    .await;
    // A model that claims completion on every call, whatever the feedback says.
    let engine = engine(
        &pool,
        ScriptedModel::always(claiming(json!({ "summary": "done" }))),
    );
    let mut budget = Budget::new(16);
    budget.max_verification_rounds = 2;
    let run = started_run(&pool, &engine, budget, &[CLAIM_GRANT]).await;
    let outcome = turn(&engine, &run).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the bound ends the turn, never the run: {outcome:?}"
    );
    assert_eq!(run_status(&pool, &run.id.to_string()).await, "RUNNING");
    let recorded = steps(&pool, &run.id.to_string()).await;
    let verifies: Vec<&(String, String)> = recorded
        .iter()
        .filter(|(kind, _)| kind == "verify")
        .collect();
    assert_eq!(
        verifies.len(),
        2,
        "exactly the bound's claims were verified: {recorded:?}"
    );
    assert!(verifies.iter().all(|(_, status)| status == "failed"));
    let calls = tool_calls(&pool, &run.id.to_string()).await;
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|call| call.3["verified"] == false));

    // The default bound (no snapshot key) is three.
    let run = started_run(&pool, &engine, Budget::new(16), &[CLAIM_GRANT]).await;
    turn(&engine, &run).await;
    let verifies = steps(&pool, &run.id.to_string())
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "verify")
        .count();
    assert_eq!(verifies, 3);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_claim_the_protocol_refuses_never_reaches_verification() {
    let Some((name, pool)) = prepare("run008_refused").await else {
        common::blocked_marker();
        return;
    };
    set_contract(
        &pool,
        json!({ "deterministic_checks": [{ "kind": "effects_settled", "effect_classes": ["read.internal"] }] }),
    )
    .await;
    // Outside the projection: the claim is refused before any dispatch.
    let model = ScriptedModel {
        rounds: Arc::new(Mutex::new(
            vec![claiming(json!({ "summary": "done" }))].into(),
        )),
        last: Arc::new(Mutex::new(ModelProposal {
            assistant_text: Some("stopped".to_string()),
            ..ModelProposal::default()
        })),
    };
    let engine = engine(&pool, model);
    let run = started_run(
        &pool,
        &engine,
        Budget::new(8),
        &[("read.internal", "fs", "**")],
    )
    .await;
    turn(&engine, &run).await;
    assert_eq!(run_status(&pool, &run.id.to_string()).await, "RUNNING");
    let recorded = steps(&pool, &run.id.to_string()).await;
    assert!(
        !recorded.iter().any(|(kind, _)| kind == "verify"),
        "{recorded:?}"
    );
    let calls = tool_calls(&pool, &run.id.to_string()).await;
    assert_eq!(calls[0].2, "rejected", "{calls:?}");
    assert_eq!(calls[0].3["code"], "CAPABILITY_DENIED", "{calls:?}");

    // An unknown field: refused by the registry's strict schema, before anything is recorded.
    let model = ScriptedModel {
        rounds: Arc::new(Mutex::new(
            vec![claiming(json!({ "summary": "done", "verified": true }))].into(),
        )),
        last: Arc::new(Mutex::new(ModelProposal {
            assistant_text: Some("stopped".to_string()),
            ..ModelProposal::default()
        })),
    };
    let engine = self::engine(&pool, model);
    let run = started_run(&pool, &engine, Budget::new(8), &[CLAIM_GRANT]).await;
    turn(&engine, &run).await;
    assert_eq!(run_status(&pool, &run.id.to_string()).await, "RUNNING");
    let recorded = steps(&pool, &run.id.to_string()).await;
    assert!(
        !recorded.iter().any(|(kind, _)| kind == "verify"),
        "{recorded:?}"
    );
    let calls = tool_calls(&pool, &run.id.to_string()).await;
    assert_eq!(calls[0].3["code"], "VALIDATION_SCHEMA", "{calls:?}");
    assert!(
        calls[0].3["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(".verified: is not a declared field")),
        "{calls:?}"
    );
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_run_stopped_mid_verification_recovers_to_running_and_can_claim_again() {
    let Some((name, pool)) = prepare("run008_recover").await else {
        common::blocked_marker();
        return;
    };
    set_contract(
        &pool,
        json!({ "deterministic_checks": [{ "kind": "effects_settled", "effect_classes": ["read.internal"] }] }),
    )
    .await;
    let engine = engine(
        &pool,
        ScriptedModel::always(claiming(json!({ "summary": "done" }))),
    );
    let run = started_run(&pool, &engine, Budget::new(8), &[CLAIM_GRANT]).await;
    // The process stopped after RUNNING → VERIFYING and before a verdict was committed.
    engine
        .store()
        .transition_run(&run.id, run.generation, RunStatus::Verifying, None)
        .await
        .expect("verifying");
    let refused = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await;
    assert!(
        refused.is_err(),
        "a VERIFYING run takes no turn before recovery"
    );

    let recovered = engine.recover(&run.id).await.expect("recover");
    assert!(
        matches!(recovered, RecoveryOutcome::Continue { .. }),
        "{recovered:?}"
    );
    assert_eq!(run_status(&pool, &run.id.to_string()).await, "RUNNING");
    // Recovery is idempotent, and the claim can now be made — and verified — again.
    assert!(matches!(
        engine.recover(&run.id).await.expect("recover again"),
        RecoveryOutcome::Continue { .. }
    ));
    let outcome = turn(&engine, &run).await;
    assert!(
        matches!(outcome, TurnOutcome::Succeeded { .. }),
        "{outcome:?}"
    );
    assert_eq!(run_status(&pool, &run.id.to_string()).await, "SUCCEEDED");
    common::drop_pool(&pool, &name).await;
}
