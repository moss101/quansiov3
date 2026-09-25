//! OPS-008: operator kill switches.
//!
//! **Kill-switch E2E.** Real turns run through capability, policy and the Effect Ledger, while an
//! administrator engages and releases switches through the public command surface.
//! - Effect freeze stops new tier ≥ 2 effects, including an approved effect resuming. Reads,
//!   tier 0/1 calls, the evidence and effects already in the ledger are untouched.
//! - Emergency deny refuses matching classes before policy.
//! - Every engage and release is in the audit chain and the event log, and releasing restores
//!   service.
//!
//! **Worker quarantine.** The target's lease is revoked (its generation moves), every call of a
//! run on it is refused, and release restores dispatch. Drain and undrain move the target
//! lifecycle; connector revoke goes through the connector owner; stream shed disconnects live
//! sessions with `STREAM_BACKPRESSURE`.
//!
//! **Provider outage drill.** Run against the real Python plane and the dev stub provider with a
//! two-provider catalog:
//! 1. Disabling the primary provider fails model calls over to the backup.
//! 2. Disabling both refuses the call with `PROVIDER_UNAVAILABLE`, and nothing is sent.
//! 3. Enabling restores the primary.
//!
//! It needs `QUANSIO_TEST_POSTGRES_URL`, `uv` and the stub provider
//! (`QUANSIO_TEST_STUB_PROVIDER_URL`, default `http://127.0.0.1:59020`); without them it prints
//! `BLOCKED_EXTERNAL`.
//!
//! **Runbook.** `docs/runbooks/kill-switches.md` must equal `runbook_markdown()`;
//! `QUANSIO_WRITE_RUNBOOKS=1` rewrites it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_machine::control::{LeaseRequest, MachineControl, TargetFence, TargetStatus};
use quansio_server::control::ops::{runbook_markdown, KillSwitch};
use quansio_server::control::schema;
use quansio_server::effects::{EffectLedger, EffectStatus};
use quansio_server::policy::{
    ActorRoles, ApprovalRuntime, ApprovalSigner, TenantRole, TrustLevel, WorkspaceRole,
};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedToolCall,
    RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput, TurnOutcome,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, HostDispatch, HostFailure, HostOutcome, ProposalTrustSource, RoleProvider,
    StoredProjectionProvider, ToolDispatchService, ToolHostPort,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0PS008";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0PS008";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0PS008";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0PS008";
const TEST_KEY: &[u8] = b"ops008-approval-signing-key";

// ------------------------------------------------------------------ runtime seams

#[derive(Clone, Default)]
struct ScriptedModel {
    proposals: Arc<Mutex<VecDeque<ModelProposal>>>,
}

impl ScriptedModel {
    fn push(&self, proposal: ModelProposal) {
        self.proposals.lock().expect("model").push_back(proposal);
    }

    fn tools(&self, calls: Vec<ProposedToolCall>) {
        self.push(ModelProposal {
            tool_calls: calls,
            ..ModelProposal::default()
        });
        self.push(ModelProposal {
            assistant_text: Some("done".to_string()),
            ..ModelProposal::default()
        });
    }
}

#[async_trait]
impl ModelProposalSource for ScriptedModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        Ok(self
            .proposals
            .lock()
            .expect("model")
            .pop_front()
            .unwrap_or(ModelProposal {
                assistant_text: Some("done".to_string()),
                ..ModelProposal::default()
            }))
    }
}

#[derive(Clone, Default)]
struct RecordingHost {
    dispatched: Arc<Mutex<Vec<String>>>,
}

impl RecordingHost {
    fn tools(&self) -> Vec<String> {
        self.dispatched.lock().expect("host").clone()
    }
}

#[async_trait]
impl ToolHostPort for RecordingHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        self.dispatched
            .lock()
            .expect("host")
            .push(dispatch.tool.clone());
        Ok(HostOutcome::Success {
            output: json!({ "ok": true }),
            evidence_ids: Vec::new(),
            remote_ref: None,
        })
    }
}

#[derive(Clone, Copy)]
struct AgentTrust;

impl ProposalTrustSource for AgentTrust {
    fn proposal_trust(&self, _run_id: &str) -> Option<TrustLevel> {
        Some(TrustLevel::AgentGenerated)
    }
}

#[derive(Clone, Copy)]
struct Roles;

#[async_trait]
impl RoleProvider for Roles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Approver)),
            user_id: Some(USER.to_string()),
        }))
    }
}

// ------------------------------------------------------------------ fixture

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
    agent_thread: CanonicalId,
    target_id: String,
    router: axum::Router,
    engine: RuntimeEngine,
    model: ScriptedModel,
    host: RecordingHost,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let mut generator = UlidGenerator::new();
    let agent_thread = CanonicalId::generate(Prefix::AgentThread, &mut generator);
    let target_id = CanonicalId::generate(Prefix::ExecutionTarget, &mut generator).to_string();
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(agent_thread.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    sqlx::query(
        "INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, \
         status, resources) VALUES ($1, $2, $3, 'persistent_workspace_computer', \
         'local_capsule_macos', 'READY', $4)",
    )
    .bind(&target_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(json!({ "workspace_root": "/work/root" }))
    .execute(&mut *tx)
    .await
    .expect("target");
    // Tier 0/1 allow; egress allows once granted; a host write asks.
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) VALUES ($1, $2, NULL, 'tenant', $3)",
    )
    .bind("pol_01J8Z3K6F1N8VQ2X5W9Y0PS008")
    .bind(TENANT)
    .bind(json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "fs.write.workspace", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "fs.write.host", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" },
        { "effect_class": "network.egress.new_destination", "resource": { "kind": "domain", "selector": "*" }, "decision": "allow" }
    ]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");

    let identity = RuntimeIdentity::system(
        TENANT,
        "ops008-test",
        CorrelationId::generate(&mut generator),
    );
    let model = ScriptedModel::default();
    let host = RecordingHost::default();
    let delegation: Arc<dyn DelegationPort> = Arc::new(
        AgentDelegationPort::with_structural_check(pool.clone(), identity.clone())
            .expect("delegation"),
    );
    let dispatcher = ToolDispatchService::new(
        pool.clone(),
        identity.clone(),
        Arc::new(
            StoredProjectionProvider::new(pool.clone(), identity.clone()).expect("projections"),
        ),
        Arc::new(Roles),
        Arc::new(host.clone()),
        Arc::clone(&delegation),
    )
    .expect("dispatch")
    .with_proposal_trust(Arc::new(AgentTrust))
    .with_signer(ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer"));
    let engine = RuntimeEngine::new(pool.clone(), identity.clone())
        .expect("engine")
        .with_model_source(Arc::new(model.clone()))
        .with_tool_dispatch(Arc::new(dispatcher))
        .with_delegation(delegation);
    let router = quansio_server::api::router(
        quansio_server::api::ApiState::new(pool.clone()).expect("api state"),
    );
    Some(Fixture {
        name,
        pool,
        identity,
        agent_thread,
        target_id,
        router,
        engine,
        model,
        host,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

/// A run on the fixture's target, with a projection that grants the suite's classes.
async fn start_run(fixture: &Fixture) -> quansio_server::runtime::state_machine::Run {
    let mut new_run = NewRun::new(
        WORKSPACE,
        CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
        fixture.agent_thread,
        RunTriggerKind::Manual,
    );
    new_run.execution_target_id = Some(fixture.target_id.clone());
    let run = fixture.engine.create_run(new_run).await.expect("create");
    let run = fixture
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    let run = fixture
        .engine
        .start(&run.id, run.generation)
        .await
        .expect("start");
    let grants: Vec<Grant> = [
        ("read.internal", "fs", "**"),
        ("fs.write.workspace", "fs", "**"),
        ("fs.write.host", "fs", "**"),
        ("network.egress.new_destination", "domain", "*"),
    ]
    .into_iter()
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
    let mut tx = fixture.pool.begin().await.expect("begin");
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

async fn turn(fixture: &Fixture, run_id: &CanonicalId) -> TurnOutcome {
    let run = fixture.engine.store().load_run(run_id).await.expect("run");
    fixture
        .engine
        .run_turn(
            run_id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn")
}

fn read(id: &str) -> ProposedToolCall {
    ProposedToolCall::new(id, "fs.read", json!({ "path": "notes.md" }))
}

fn navigate(id: &str) -> ProposedToolCall {
    ProposedToolCall::new(
        id,
        "browser.navigate",
        json!({ "url": "https://example.com/", "session_id": "bsn_01J8Z3K6F1N8VQ2X5W9Y0PS008" }),
    )
}

async fn command(fixture: &Fixture, name: &str, params: Value) -> (StatusCode, Value) {
    let id = CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new()).to_string();
    let response = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/commands/{name}"))
                .header("x-quansio-tenant", TENANT)
                .header("x-quansio-user", USER)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({ "command_id": id, "params": params }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(fixture: &Fixture, path: &str) -> (StatusCode, Value) {
    let response = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("x-quansio-tenant", TENANT)
                .header("x-quansio-user", USER)
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn ok(status: StatusCode, body: &Value) {
    assert!(status.is_success(), "{status}: {body}");
}

/// Refusal codes of the run's rejected tool calls, oldest first.
async fn refusals(fixture: &Fixture) -> Vec<(String, String)> {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows: Vec<(String, Option<Value>)> = sqlx::query_as(
        "SELECT tool_name, result FROM tool_calls WHERE tenant_id = $1 AND status = 'rejected' ORDER BY created_at",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("tool calls");
    tx.commit().await.expect("commit");
    rows.into_iter()
        .map(|(tool, result)| {
            (
                tool,
                result
                    .and_then(|r| r["code"].as_str().map(str::to_string))
                    .unwrap_or_default(),
            )
        })
        .collect()
}

async fn effect_rows(fixture: &Fixture) -> Vec<(String, String, Option<Value>)> {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query_as(
        "SELECT id, status, outcome FROM effect_records WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("effects");
    tx.commit().await.expect("commit");
    rows
}

async fn audit_actions(fixture: &Fixture) -> Vec<(String, String)> {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query_as(
        "SELECT action, decision FROM audit_entries WHERE tenant_id = $1 AND action LIKE 'ops.%' ORDER BY occurred_at",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("audit");
    tx.commit().await.expect("commit");
    rows
}

async fn ops_events(fixture: &Fixture) -> Vec<String> {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query_scalar(
        "SELECT type FROM runtime_events WHERE tenant_id = $1 AND type LIKE 'ops.%' ORDER BY sequence",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    rows
}

// ------------------------------------------------------------------ kill-switch E2E

#[tokio::test]
async fn effect_freeze_stops_new_consequential_actions_without_touching_reads_or_evidence() {
    let Some(fixture) = prepare("ops008_freeze").await else {
        blocked_marker();
        return;
    };
    let run = start_run(&fixture).await;

    // Before the incident: a tier 2 call settles, and a tier 3 host write waits for approval.
    fixture.model.tools(vec![navigate("call_before")]);
    assert!(matches!(
        turn(&fixture, &run.id).await,
        TurnOutcome::Completed { .. }
    ));
    fixture.model.push(ModelProposal {
        tool_calls: vec![ProposedToolCall::new(
            "call_host_write",
            "fs.write",
            json!({ "path": "/etc/hosts", "content": "127.0.0.1 x", "content_digest": "sha256:abc" }),
        )],
        ..ModelProposal::default()
    });
    let TurnOutcome::Parked { wait_key, .. } = turn(&fixture, &run.id).await else {
        panic!("the host write parks for approval");
    };
    let before = effect_rows(&fixture).await;
    assert_eq!(before.len(), 2, "{before:?}");
    assert_eq!(fixture.host.tools(), vec!["browser.navigate".to_string()]);

    // The operator freezes the tenant.
    let (status, body) = command(
        &fixture,
        "FreezeEffects",
        json!({ "reason": "INC-42 exfiltration suspected" }),
    )
    .await;
    ok(status, &body);
    assert_eq!(body["result"]["switch"]["switch"], "effect.freeze");
    assert_eq!(body["result"]["switch"]["target"], "*");
    let (status, again) = command(&fixture, "FreezeEffects", json!({ "reason": "twice" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");

    // A person approves the waiting write; the freeze still stops it before reservation.
    let parked = fixture.engine.store().load_run(&run.id).await.expect("run");
    ApprovalRuntime::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("approvals")
        .grant_and_resume(
            &wait_key,
            USER,
            parked.generation,
            &ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer"),
            chrono::Utc::now(),
        )
        .await
        .expect("grant");
    // Then the model tries a read (tier 0) and another navigation (tier 2).
    fixture
        .model
        .tools(vec![read("call_read"), navigate("call_during")]);
    let outcome = turn(&fixture, &run.id).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        fixture.host.tools(),
        vec!["browser.navigate".to_string(), "fs.read".to_string()],
        "only the read ran while frozen"
    );
    let refused = refusals(&fixture).await;
    assert!(
        refused.contains(&("browser.navigate".to_string(), "EFFECTS_FROZEN".to_string())),
        "{refused:?}"
    );

    // Effects already in the ledger are untouched: the settled one is still settled with its
    // outcome, the approved one did not advance, and the read added nothing consequential.
    let during = effect_rows(&fixture).await;
    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    assert_eq!(
        during[0], before[0],
        "a settled effect is not rewritten by a freeze"
    );
    assert_eq!(
        ledger.load(&before[1].0).await.expect("effect").status,
        EffectStatus::Proposed,
        "the approved write was stopped before reservation"
    );
    assert!(
        during
            .iter()
            .all(|(_, status, _)| status != "RESERVED" && status != "DISPATCHED"),
        "{during:?}"
    );
    // Reads keep working through the public surface.
    let (status, effects) = get(&fixture, &format!("/v1/effects?workspace_id={WORKSPACE}")).await;
    assert_eq!(status, StatusCode::OK, "{effects}");
    assert!(effects.to_string().contains(&before[0].0), "{effects}");
    let (status, switches) = get(&fixture, "/v1/ops/switches").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(switches["switches"][0]["active"], true, "{switches}");
    assert_eq!(
        switches["switches"][0]["reason"],
        "INC-42 exfiltration suspected"
    );

    // Release restores service.
    let (status, body) = command(
        &fixture,
        "UnfreezeEffects",
        json!({ "reason": "INC-42 contained" }),
    )
    .await;
    ok(status, &body);
    fixture.model.tools(vec![navigate("call_after")]);
    assert!(matches!(
        turn(&fixture, &run.id).await,
        TurnOutcome::Completed { .. }
    ));
    assert_eq!(
        fixture.host.tools().last().map(String::as_str),
        Some("browser.navigate")
    );
    let (status, body) = command(&fixture, "UnfreezeEffects", json!({})).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "nothing left to release: {body}"
    );

    // Audited and evented, engage and release, with the reason and the runtime.control class.
    assert_eq!(
        audit_actions(&fixture).await,
        vec![
            (
                "ops.effect.freeze.engage".to_string(),
                "engaged".to_string()
            ),
            (
                "ops.effect.freeze.release".to_string(),
                "released".to_string()
            ),
        ]
    );
    assert_eq!(
        ops_events(&fixture).await,
        vec![
            "ops.kill_switch_engaged".to_string(),
            "ops.kill_switch_released".to_string()
        ]
    );
    let (_, audit) = get(&fixture, "/v1/audit").await;
    assert!(
        audit
            .to_string()
            .contains("runtime.control (tier 3): INC-42 exfiltration suspected"),
        "{audit}"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn emergency_deny_refuses_matching_classes_before_policy_and_scoped_freezes_stay_scoped() {
    let Some(fixture) = prepare("ops008_deny").await else {
        blocked_marker();
        return;
    };
    let run = start_run(&fixture).await;
    let (status, body) = command(
        &fixture,
        "EngageKillSwitch",
        json!({ "switch": "policy.emergency_deny", "target": "network.*", "reason": "INC-7 egress" }),
    )
    .await;
    ok(status, &body);
    fixture.model.tools(vec![read("r1"), navigate("n1")]);
    turn(&fixture, &run.id).await;
    assert_eq!(fixture.host.tools(), vec!["fs.read".to_string()]);
    assert!(refusals(&fixture)
        .await
        .contains(&("browser.navigate".to_string(), "POLICY_DENIED".to_string())));
    let (status, body) = command(
        &fixture,
        "ReleaseKillSwitch",
        json!({ "switch": "policy.emergency_deny", "target": "network.*" }),
    )
    .await;
    ok(status, &body);

    // A freeze on another workspace does not reach this one.
    let (status, body) = command(
        &fixture,
        "FreezeEffects",
        json!({ "workspace_id": "ws_01J8Z3K6F1N8VQ2X5W9Y0PS009", "reason": "x" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown workspace: {body}"
    );
    fixture.model.tools(vec![navigate("n2")]);
    turn(&fixture, &run.id).await;
    assert_eq!(
        fixture.host.tools().last().map(String::as_str),
        Some("browser.navigate")
    );

    // Malformed switches and targets are refused.
    for (params, why) in [
        (
            json!({ "switch": "everything.off", "target": "*", "reason": "x" }),
            "unknown switch",
        ),
        (
            json!({ "switch": "stream.shed", "target": "ws_x", "reason": "x" }),
            "bad target",
        ),
        (
            json!({ "switch": "effect.freeze", "target": "*" }),
            "no reason",
        ),
    ] {
        let (status, body) = command(&fixture, "EngageKillSwitch", params).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }
    finish(fixture).await;
}

// ------------------------------------------------------------------ targets and workers

async fn target_state(fixture: &Fixture) -> (TargetStatus, i64, Option<String>) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    let target = MachineControl::load(&mut tx, TENANT, &fixture.target_id)
        .await
        .expect("target");
    tx.commit().await.expect("commit");
    (target.status, target.generation, target.lease_id)
}

async fn lease(fixture: &Fixture) -> Result<(), String> {
    let (_, generation, _) = target_state(fixture).await;
    let mut tx = fixture.pool.begin().await.expect("begin");
    let lease_id = CanonicalId::generate(Prefix::Lease, &mut UlidGenerator::new()).to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let result = MachineControl::acquire_lease(
        &mut tx,
        TENANT,
        &LeaseRequest {
            lease_id: &lease_id,
            target_id: &fixture.target_id,
            controller_id: "ops008-controller",
            fence: &TargetFence { generation },
            ttl_seconds: 300,
            now: &now,
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| error.to_string());
    tx.commit().await.expect("commit");
    result
}

#[tokio::test]
async fn worker_quarantine_fences_the_target_and_release_restores_dispatch() {
    let Some(fixture) = prepare("ops008_quarantine").await else {
        blocked_marker();
        return;
    };
    let run = start_run(&fixture).await;
    lease(&fixture).await.expect("the worker holds a lease");
    let (_, generation, lease_id) = target_state(&fixture).await;
    assert!(lease_id.is_some());

    let (status, body) = command(
        &fixture,
        "QuarantineWorker",
        json!({ "target_id": fixture.target_id, "reason": "INC-9 worker compromised" }),
    )
    .await;
    ok(status, &body);
    let (_, fenced_generation, fenced_lease) = target_state(&fixture).await;
    assert!(fenced_lease.is_none(), "the lease is revoked");
    assert!(
        fenced_generation > generation,
        "the generation moved, fencing the old worker"
    );

    fixture.model.tools(vec![read("q1")]);
    turn(&fixture, &run.id).await;
    assert!(
        fixture.host.tools().is_empty(),
        "nothing reaches a quarantined worker"
    );
    assert!(refusals(&fixture)
        .await
        .contains(&("fs.read".to_string(), "TARGET_UNAVAILABLE".to_string())));

    let (status, body) = command(
        &fixture,
        "ReleaseKillSwitch",
        json!({ "switch": "worker.quarantine", "target": fixture.target_id, "reason": "reimaged" }),
    )
    .await;
    ok(status, &body);
    fixture.model.tools(vec![read("q2")]);
    turn(&fixture, &run.id).await;
    assert_eq!(fixture.host.tools(), vec!["fs.read".to_string()]);
    lease(&fixture).await.expect("a fresh lease after release");
    assert_eq!(
        audit_actions(&fixture).await,
        vec![
            (
                "ops.worker.quarantine.engage".to_string(),
                "engaged".to_string()
            ),
            (
                "ops.worker.quarantine.release".to_string(),
                "released".to_string()
            ),
        ]
    );
    finish(fixture).await;
}

#[tokio::test]
async fn drain_and_undrain_move_the_target_lifecycle_and_are_audited() {
    let Some(fixture) = prepare("ops008_drain").await else {
        blocked_marker();
        return;
    };
    let (status, body) = command(
        &fixture,
        "DrainTarget",
        json!({ "target_id": fixture.target_id, "reason": "host maintenance" }),
    )
    .await;
    ok(status, &body);
    assert_eq!(target_state(&fixture).await.0, TargetStatus::Draining);
    assert!(
        lease(&fixture).await.is_err(),
        "a draining target grants no new lease"
    );
    let (status, _) = command(
        &fixture,
        "DrainTarget",
        json!({ "target_id": fixture.target_id, "reason": "again" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = command(
        &fixture,
        "ReleaseKillSwitch",
        json!({ "switch": "target.drain", "target": fixture.target_id }),
    )
    .await;
    ok(status, &body);
    assert_eq!(target_state(&fixture).await.0, TargetStatus::Ready);
    lease(&fixture)
        .await
        .expect("a lease after the drain is released");
    let (status, body) = command(
        &fixture,
        "DrainTarget",
        json!({ "target_id": "tgt_01J8Z3K6F1N8VQ2X5W9Y0PS009", "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    finish(fixture).await;
}

#[tokio::test]
async fn connector_revoke_goes_through_the_connector_owner() {
    let Some(fixture) = prepare("ops008_connector").await else {
        blocked_marker();
        return;
    };
    let instance = "cnx_01J8Z3K6F1N8VQ2X5W9Y0PS008";
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO connector_instances (id, tenant_id, workspace_id, connector_id, display_name, status) \
         VALUES ($1, $2, $3, 'github', 'GitHub', 'connected')",
    )
    .bind(instance)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("connector");
    tx.commit().await.expect("commit");
    let (status, body) = command(
        &fixture,
        "RevokeConnector",
        json!({ "instance_id": instance }),
    )
    .await;
    ok(status, &body);
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let state: (String, Option<String>) =
        sqlx::query_as("SELECT status, credential_handle FROM connector_instances WHERE id = $1")
            .bind(instance)
            .fetch_one(&mut *tx)
            .await
            .expect("state");
    tx.commit().await.expect("commit");
    assert_eq!(state, ("revoked".to_string(), None));
    assert!(ops_events(&fixture)
        .await
        .contains(&"ops.kill_switch_engaged".to_string()));
    let (status, body) = command(
        &fixture,
        "ReleaseKillSwitch",
        json!({ "switch": "connector.revoke", "target": instance, "reason": "reconnected" }),
    )
    .await;
    ok(status, &body);
    finish(fixture).await;
}

// ------------------------------------------------------------------ stream shed

#[tokio::test]
async fn stream_shed_disconnects_live_sessions_with_backpressure() {
    let Some(fixture) = prepare("ops008_shed").await else {
        blocked_marker();
        return;
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = fixture.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    let connect = || async {
        let mut request = format!("ws://{addr}/v1/stream")
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("x-quansio-tenant", TENANT.parse().expect("h"));
        request
            .headers_mut()
            .insert("x-quansio-user", USER.parse().expect("h"));
        let (mut ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect");
        ws.send(Message::Text(
            json!({ "subscribe": [{ "channel": format!("workspace:{WORKSPACE}") }] }).to_string(),
        ))
        .await
        .expect("subscribe");
        ws
    };
    let mut first = connect().await;
    let mut second = connect().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (status, body) = command(
        &fixture,
        "EngageKillSwitch",
        json!({ "switch": "stream.shed", "target": "*", "reason": "INC-3 stream storm" }),
    )
    .await;
    ok(status, &body);
    for ws in [&mut first, &mut second] {
        let mut shed = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, ws.next()).await {
            if let Message::Text(text) = message {
                if text.contains("STREAM_BACKPRESSURE") {
                    shed = true;
                }
            }
        }
        assert!(
            shed,
            "every live session is told STREAM_BACKPRESSURE and closed"
        );
    }
    // Clients reconnect (with their cursor) and are served.
    let mut again = connect().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(500), again.next())
            .await
            .is_err(),
        "a reconnected session stays open"
    );
    let (status, body) = command(
        &fixture,
        "ReleaseKillSwitch",
        json!({ "switch": "stream.shed", "target": "*" }),
    )
    .await;
    ok(status, &body);
    finish(fixture).await;
}

// ------------------------------------------------------------------ provider outage drill

const DRILL_CATALOG: &str = "version: 1
providers:
  primary-cloud:
    kind: openai_compatible
    base_url: {provider}/v1
    credential_handle: provider/int001-stub
  backup-cloud:
    kind: openai_compatible
    base_url: {provider}/v1
    credential_handle: provider/int001-stub
routing:
  strategy: deterministic
  primary: route-primary
  fallback_order: [route-backup]
models:
  - id: route-primary
    provider: primary-cloud
    model_id: ops008-primary-model
    capabilities: [tools, streaming]
    context_window: 32768
    cost_class: low
    dlp_eligible: false
  - id: route-backup
    provider: backup-cloud
    model_id: ops008-backup-model
    capabilities: [tools, streaming]
    context_window: 32768
    cost_class: low
    dlp_eligible: false
";

#[tokio::test]
async fn provider_outage_drill_fails_over_refuses_and_restores() {
    use quansio_server::intelligence::model_source::{GatewayModelSource, ModelCallSettings};
    use quansio_server::intelligence::IntelligenceClient;
    use quansio_server::runtime::context_bridge::GatewayContextBridge;

    let Some(service) = common::intelligence::start_with(common::intelligence::Options {
        catalog: Some(DRILL_CATALOG.to_string()),
        ..common::intelligence::Options::default()
    })
    .await
    else {
        return;
    };
    let name = scratch_name("ops008_drill");
    let Some(pool) = fresh_database(&name).await else {
        blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let settings = ModelCallSettings::load().expect("config/intelligence.yaml");
    let client = IntelligenceClient::connect_lazy(&service.endpoint).expect("endpoint");
    let context = Arc::new(GatewayContextBridge::new(client.clone(), settings.deadline));
    let router = quansio_server::api::router(
        quansio_server::api::ApiState::new(pool.clone())
            .expect("api")
            .with_model_source(Arc::new(GatewayModelSource::new(
                pool.clone(),
                client,
                context,
                settings,
            ))),
    );
    let post = |content: &'static str| {
        let router = router.clone();
        async move {
            let id = CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new()).to_string();
            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/commands/PostMessage")
                        .header("x-quansio-tenant", TENANT)
                        .header("x-quansio-user", USER)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            json!({ "command_id": id, "params": { "workspace_id": WORKSPACE, "content": content } })
                                .to_string(),
                        ))
                        .expect("request"),
                )
                .await
                .expect("response");
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .expect("body");
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let switch = |name: &'static str, provider: &'static str| {
        let router = router.clone();
        async move {
            let id = CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new()).to_string();
            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/v1/commands/{name}"))
                        .header("x-quansio-tenant", TENANT)
                        .header("x-quansio-user", USER)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            json!({ "command_id": id, "params": { "provider": provider, "reason": "outage drill" } })
                                .to_string(),
                        ))
                        .expect("request"),
                )
                .await
                .expect("response");
            assert!(
                response.status().is_success(),
                "{name} {provider}: {}",
                response.status()
            );
        }
    };
    let providers_used = || {
        let pool = pool.clone();
        async move {
            let mut tx = pool.begin().await.expect("begin");
            schema::set_tenant_context(&mut tx, TENANT)
                .await
                .expect("tenant");
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT r.provider FROM model_calls c JOIN model_routes r ON r.id = c.route_id \
                 WHERE c.tenant_id = $1 ORDER BY c.created_at",
            )
            .bind(TENANT)
            .fetch_all(&mut *tx)
            .await
            .expect("model calls");
            tx.commit().await.expect("commit");
            rows
        }
    };

    let (status, body) = post("before the outage").await;
    ok(status, &body);
    assert_eq!(providers_used().await, vec!["primary-cloud".to_string()]);

    // The primary provider has an outage: calls fail over to the backup.
    switch("DisableProvider", "primary-cloud").await;
    let (status, body) = post("during the outage").await;
    ok(status, &body);
    assert_eq!(
        providers_used().await.last().map(String::as_str),
        Some("backup-cloud")
    );

    // Both are out: the call is refused, not sent.
    switch("DisableProvider", "backup-cloud").await;
    let calls_before = providers_used().await.len();
    let (status, body) = post("with nothing left").await;
    assert!(!status.is_success(), "{body}");
    assert_eq!(body["code"], "PROVIDER_UNAVAILABLE", "{body}");
    assert_eq!(
        providers_used().await.len(),
        calls_before,
        "no provider call was recorded"
    );

    // Recovery.
    switch("EnableProvider", "backup-cloud").await;
    switch("EnableProvider", "primary-cloud").await;
    let (status, body) = post("after recovery").await;
    ok(status, &body);
    assert_eq!(
        providers_used().await.last().map(String::as_str),
        Some("primary-cloud")
    );
    drop_pool(&pool, &name).await;
    drop(service);
}

// ------------------------------------------------------------------ runbook

#[test]
fn the_runbook_is_generated_from_the_switch_catalog() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/runbooks/kill-switches.md");
    let generated = runbook_markdown();
    if std::env::var("QUANSIO_WRITE_RUNBOOKS").is_ok_and(|value| value == "1") {
        std::fs::write(&path, &generated).expect("write the runbook");
    }
    let committed = std::fs::read_to_string(&path).expect("docs/runbooks/kill-switches.md");
    assert_eq!(
        committed, generated,
        "docs/runbooks/kill-switches.md drifted; regenerate with QUANSIO_WRITE_RUNBOOKS=1"
    );
    for switch in KillSwitch::ALL {
        assert!(committed.contains(switch.as_str()));
    }
}
