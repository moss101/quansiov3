//! RUN-011 acceptance: the agent turn loop, the ToolCall protocol and the human question
//! protocol (DOMAIN.md §5.6, §7.4, §7.5).
//!
//! The model and the tool hosts are conformance stubs (no provider, no external boundary),
//! but everything else is real: the Tool Registry, the Capability Projection check, policy
//! and approvals, the Effect Ledger, the Run/Turn/Step/Attempt state machine, delegation and
//! the durable question protocol.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`
//! (`postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio` on the dev stack).

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::effects::{EffectLedger, EffectStatus};
use quansio_server::policy::{
    ActorRoles, ApprovalRuntime, ApprovalSigner, PolicyStore, TenantRole, WorkspaceRole,
};
use quansio_server::policy::{StoredProposalTrust, TrustLevel};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    Budget, DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun,
    ProposedQuestion, ProposedToolCall, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError,
    RuntimeIdentity, StepKind, StepStatus, TurnInput, TurnOutcome,
};
use quansio_server::runtime::turn_loop::{
    load_turn_usage, ActingActor, HostDispatch, HostFailure, HostOutcome, ProposalTrustSource,
    QuestionService, RoleProvider, StoredProjectionProvider, ToolDispatchService, ToolHostPort,
    UnavailableProposalTrust,
};
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0RR011";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0RR011";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0RR011";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0RR011";
const FS_ROOT: &str = "/work/root";
const TEST_KEY: &[u8] = b"run011-approval-signing-key";

// ---------------------------------------------------------------------------------------
// Conformance seams
// ---------------------------------------------------------------------------------------

/// A model that returns scripted proposals, one per model call.
#[derive(Clone, Default)]
struct StubModel {
    proposals: Arc<Mutex<VecDeque<ModelProposal>>>,
    calls: Arc<Mutex<u32>>,
}

impl StubModel {
    fn with(proposals: Vec<ModelProposal>) -> Self {
        Self {
            proposals: Arc::new(Mutex::new(proposals.into())),
            calls: Arc::new(Mutex::new(0)),
        }
    }

    fn calls(&self) -> u32 {
        *self.calls.lock().expect("calls")
    }
}

#[async_trait]
impl ModelProposalSource for StubModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        *self.calls.lock().expect("calls") += 1;
        let next = self.proposals.lock().expect("proposals").pop_front();
        Ok(next.unwrap_or(ModelProposal {
            assistant_text: Some("done".to_string()),
            ..ModelProposal::default()
        }))
    }
}

/// A tool host that records what it was asked to do and can be scripted to fail or to lose
/// the outcome.
///
/// When a barrier is installed, every host call must reach it before any of them proceeds:
/// a runtime that dispatched the calls one after another would time out instead of passing,
/// which is how the concurrency test proves the calls really overlap.
#[derive(Clone, Default)]
struct ConformanceHost {
    dispatches: Arc<Mutex<Vec<HostDispatch>>>,
    timeline: Arc<Mutex<Vec<String>>>,
    unknown: Arc<Mutex<HashSet<String>>>,
    failing: Arc<Mutex<HashSet<String>>>,
    refusing: Arc<Mutex<HashSet<String>>>,
    barrier: Arc<Mutex<Option<Arc<tokio::sync::Barrier>>>>,
}

impl ConformanceHost {
    fn dispatch_count(&self) -> usize {
        self.dispatches.lock().expect("dispatches").len()
    }

    fn timeline(&self) -> Vec<String> {
        self.timeline.lock().expect("timeline").clone()
    }
}

#[async_trait]
impl ToolHostPort for ConformanceHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        if self
            .refusing
            .lock()
            .expect("refusing")
            .contains(&dispatch.tool)
        {
            return Err(HostFailure::Refused {
                detail: "the sandbox refuses this path".to_string(),
            });
        }
        self.timeline
            .lock()
            .expect("timeline")
            .push(format!("start:{}", dispatch.tool_call_id));
        let barrier = self.barrier.lock().expect("barrier").clone();
        if let Some(barrier) = barrier {
            if tokio::time::timeout(std::time::Duration::from_secs(5), barrier.wait())
                .await
                .is_err()
            {
                self.timeline
                    .lock()
                    .expect("timeline")
                    .push(format!("no-overlap:{}", dispatch.tool_call_id));
            }
        }
        self.dispatches
            .lock()
            .expect("dispatches")
            .push(dispatch.clone());
        self.timeline
            .lock()
            .expect("timeline")
            .push(format!("end:{}", dispatch.tool_call_id));

        if self
            .unknown
            .lock()
            .expect("unknown")
            .contains(&dispatch.tool)
        {
            return Ok(HostOutcome::Unknown {
                detail: "connection dropped after dispatch".to_string(),
            });
        }
        if self
            .failing
            .lock()
            .expect("failing")
            .contains(&dispatch.tool)
        {
            return Ok(HostOutcome::Failure {
                retryable: false,
                error: json!({ "code": "TOOL_FAILED" }),
                evidence_ids: Vec::new(),
            });
        }
        Ok(HostOutcome::Success {
            output: json!({ "ok": true, "tool": dispatch.tool }),
            evidence_ids: vec![format!("evd_{}", dispatch.tool.replace('.', "_"))],
            remote_ref: None,
        })
    }
}

/// The proposal trust the conformance model declares. The model's own proposal is
/// `AGENT_GENERATED` (DOMAIN.md §12); the default source proves nothing and fails closed.
#[derive(Clone, Copy)]
struct StaticProposalTrust(TrustLevel);

#[async_trait::async_trait]
impl ProposalTrustSource for StaticProposalTrust {
    async fn proposal_trust(&self, _run_id: &str, _turn_id: &str) -> Option<TrustLevel> {
        Some(self.0)
    }
}

/// The acting user's roles, as the session layer would resolve them.
#[derive(Clone, Copy, Default)]
struct TestRoles;

#[async_trait]
impl RoleProvider for TestRoles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Approver)),
            user_id: Some(USER.to_string()),
        }))
    }
}

// ---------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
    agent_thread: CanonicalId,
    target_id: String,
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
        .expect("tenant context");
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
         status, resources) \
         VALUES ($1, $2, $3, 'persistent_workspace_computer', 'local_capsule_macos', 'READY', $4)",
    )
    .bind(&target_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(json!({ "workspace_root": FS_ROOT }))
    .execute(&mut *tx)
    .await
    .expect("execution target");
    tx.commit().await.expect("commit");

    let identity = RuntimeIdentity::system(
        TENANT,
        "turn-loop-test",
        CorrelationId::generate(&mut generator),
    );
    Some(Fixture {
        name,
        pool,
        identity,
        agent_thread,
        target_id,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

/// Store the tenant policy that governs the suite's calls: tier 0 and 1 allow, a host write
/// asks, and internal record creation allows.
async fn seed_policy(fixture: &Fixture) {
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "read.external", "resource": { "kind": "domain", "selector": "*" }, "decision": "allow" },
        { "effect_class": "process.exec.sandboxed", "resource": { "kind": "target", "selector": "*" }, "decision": "allow" },
        { "effect_class": "fs.write.workspace", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "fs.write.host", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" },
        { "effect_class": "record.create", "resource": { "kind": "work", "selector": "*" }, "decision": "allow" }
    ]);
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ($1, $2, NULL, 'tenant', $3)",
    )
    .bind("pol_01J8Z3K6F1N8VQ2X5W9Y0RR011")
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
}

/// Store the run's capability projection (DOMAIN.md §6.2).
async fn seed_projection(fixture: &Fixture, run_id: &str) -> CapabilityProjection {
    let grants: Vec<Grant> = [
        ("read.internal", "fs", "**"),
        ("read.external", "domain", "*"),
        ("process.exec.sandboxed", "target", "*"),
        ("fs.write.workspace", "fs", "**"),
        ("fs.write.host", "fs", "**"),
        ("record.create", "work", "*"),
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

    let mut generator = UlidGenerator::new();
    let projection = CapabilityProjection::from_parts(
        CanonicalId::generate(Prefix::CapabilityProjection, &mut generator),
        ProjectionSubject::new(SubjectKind::Run, run_id),
        Vec::new(),
        grants,
        chrono::Utc::now(),
        Some(chrono::Utc::now() + chrono::Duration::hours(1)),
    )
    .expect("projection");

    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
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
    projection
}

struct Seams {
    engine: RuntimeEngine,
    model: StubModel,
    questions: QuestionService,
}

/// Build the engine and its seams: real dispatch, real questions, real delegation.
fn build_seams(fixture: &Fixture, model: StubModel, host: ConformanceHost) -> Seams {
    build_seams_with_trust(
        fixture,
        model,
        host,
        Arc::new(StaticProposalTrust(TrustLevel::AgentGenerated)),
    )
}

/// Build the engine with an explicit proposal-trust source.
fn build_seams_with_trust(
    fixture: &Fixture,
    model: StubModel,
    host: ConformanceHost,
    trust: Arc<dyn ProposalTrustSource>,
) -> Seams {
    let delegation: Arc<dyn DelegationPort> = Arc::new(
        AgentDelegationPort::with_structural_check(fixture.pool.clone(), fixture.identity.clone())
            .expect("delegation port"),
    );
    let questions = QuestionService::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("question service");
    let projections = StoredProjectionProvider::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("projection provider");
    let dispatcher = ToolDispatchService::new(
        fixture.pool.clone(),
        fixture.identity.clone(),
        Arc::new(projections),
        Arc::new(TestRoles),
        Arc::new(host.clone()),
        Arc::clone(&delegation),
    )
    .expect("dispatch service")
    .with_proposal_trust(trust)
    .with_signer(ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer"));

    let engine = RuntimeEngine::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("engine")
        .with_model_source(Arc::new(model.clone()))
        .with_tool_dispatch(Arc::new(dispatcher))
        .with_question_port(Arc::new(questions.clone()))
        .with_delegation(delegation);
    Seams {
        engine,
        model,
        questions,
    }
}

/// Create, enqueue and start a run with a step budget.
async fn start_run(
    fixture: &Fixture,
    engine: &RuntimeEngine,
    max_steps: u32,
) -> quansio_server::runtime::state_machine::Run {
    let mut new_run = NewRun::new(
        WORKSPACE,
        CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
        fixture.agent_thread,
        RunTriggerKind::Manual,
    )
    .with_budget(Budget::new(max_steps));
    new_run.execution_target_id = Some(fixture.target_id.clone());
    let run = engine.create_run(new_run).await.expect("create run");
    let run = engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    engine.start(&run.id, run.generation).await.expect("start")
}

async fn events_of_type(pool: &PgPool, event_type: &str) -> Vec<Value> {
    let store = quansio_events::EventStore::new(pool.clone());
    let events = store
        .read_events_after(TENANT, None, 1_000)
        .await
        .expect("events");
    events
        .into_iter()
        .filter(|event| event.event_type.to_string() == event_type)
        .map(|event| event.payload)
        .collect()
}

/// The durable tool-call rows, used in assertion messages so a refusal names itself.
async fn tool_call_rows(fixture: &Fixture) -> Vec<(String, String, Option<Value>)> {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let rows = sqlx::query_as::<_, (String, String, Option<Value>)>(
        "SELECT tool_name, status, result FROM tool_calls WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("tool calls");
    tx.commit().await.expect("commit");
    rows
}

fn read_tool(call_id: &str, path: &str) -> ProposedToolCall {
    ProposedToolCall::new(call_id, "fs.read", json!({ "path": path }))
}

// ---------------------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_turn_executes_tool_proposals_through_capability_policy_and_the_effect_ledger() {
    let Some(fixture) = prepare("run011_turn").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            assistant_text: Some("working".to_string()),
            tool_calls: vec![
                read_tool("call_read", "/work/root/notes.md"),
                ProposedToolCall::new(
                    "call_term",
                    "terminal.exec",
                    json!({ "command": "ls -la", "cwd": "/work/root" }),
                ),
            ],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("finished".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a turn without a completion claim completes: {outcome:?}"
    );
    assert_eq!(seams.model.calls(), 2, "one model call per loop iteration");
    assert_eq!(
        host.dispatch_count(),
        2,
        "both tool calls were dispatched; rows: {:?}",
        tool_call_rows(&fixture).await
    );

    let turns = seams
        .engine
        .store()
        .list_turns(&run.id)
        .await
        .expect("turns");
    assert_eq!(turns.len(), 1);
    let steps = seams
        .engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps");
    assert_eq!(steps.len(), 4, "two model calls and two tool calls");
    assert_eq!(
        steps
            .iter()
            .filter(|s| s.kind == StepKind::ToolCall)
            .count(),
        2
    );
    for step in &steps {
        assert_eq!(
            step.status,
            StepStatus::Completed,
            "every step completed: {step:?}"
        );
        let attempts = seams
            .engine
            .store()
            .list_attempts(&step.id)
            .await
            .expect("attempts");
        assert_eq!(attempts.len(), 1, "one attempt per step");
        assert!(attempts[0].status.is_terminal());
    }

    // The Effect Ledger is the record of what actually happened externally.
    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    let effects = ledger
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects.len(), 2, "one effect record per consequential call");
    for effect in &effects {
        assert_eq!(
            effect.status,
            EffectStatus::SettledSuccess,
            "dispatch settled: {effect:?}"
        );
        assert!(effect.dispatch_token.is_some(), "reserved with a token");
    }

    let completed = events_of_type(&fixture.pool, "tool.completed").await;
    assert_eq!(completed.len(), 2, "one tool.completed per call");
    let dispatched = events_of_type(&fixture.pool, "tool.dispatched").await;
    assert_eq!(dispatched.len(), 2);

    // And the per-turn usage view the UI inspects.
    let usage = load_turn_usage(&fixture.pool, &fixture.identity, &turns[0].id.to_string())
        .await
        .expect("usage");
    assert_eq!(usage.tools.len(), 2);
    assert_eq!(usage.tools[0].tool, "fs.read");
    assert_eq!(usage.tools[0].status, "completed");
    assert_eq!(usage.tools[0].declaration_version, 1);
    assert_eq!(usage.step_count, 4);
    finish(fixture).await;
}

#[tokio::test]
async fn a_host_refusal_after_dispatch_settles_as_a_known_failure_not_an_unknown_outcome() {
    let Some(fixture) = prepare("run011_refused").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![read_tool("call_read", "/work/root/notes.md")],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("the read was refused".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    host.refusing
        .lock()
        .expect("refusing")
        .insert("fs.read".to_string());
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    // The refusal reaches the model as the call's result; the turn itself does not fail.
    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("a refused call is a result, not a failed turn");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let rows = tool_call_rows(&fixture).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "rejected", "{rows:?}");

    // The host refused before acting: the outcome is known, so nothing awaits reconciliation.
    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    let effects = ledger
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].status, EffectStatus::SettledFailed);
    assert!(!effects[0].status.is_unsettled());
    let outcome = effects[0].outcome.as_ref().expect("outcome");
    assert!(!outcome.is_retryable(), "a refusal is not retried");
    let recovery = seams.engine.recover(&run.id).await.expect("recover");
    assert!(
        !matches!(
            recovery,
            quansio_server::runtime::state_machine::RecoveryOutcome::ReconcileEffect { .. }
        ),
        "a refused call never needs reconciliation: {recovery:?}"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn an_internal_tool_refused_by_its_port_settles_as_a_known_failure() {
    let Some(fixture) = prepare("run011_internal_refused").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![ProposedToolCall::new(
                "call_plan",
                "work.propose_plan",
                json!({ "proposal_id": "prp_refused", "proposal_json": "{}" }),
            )],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("the plan was refused".to_string()),
            ..ModelProposal::default()
        },
    ]);
    // No plan port is installed, so the port refuses the proposal.
    let seams = build_seams(&fixture, model, ConformanceHost::default());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("a refused internal call is a result, not a failed turn");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let rows = tool_call_rows(&fixture).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1, "rejected", "{rows:?}");
    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    let effects = ledger
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].status, EffectStatus::SettledFailed);
    finish(fixture).await;
}

#[tokio::test]
async fn a_call_outside_the_capability_projection_is_rejected_before_dispatch() {
    let Some(fixture) = prepare("run011_cap").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![ProposedToolCall::new(
                "call_fetch",
                "web.fetch",
                json!({ "url": "https://example.com/" }),
            )],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("stopping".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    // A projection that grants nothing removes every tool from the model's surface.
    seed_projection(&fixture, &run.id.to_string()).await;
    clear_grants(&fixture).await;

    seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");

    assert_eq!(host.dispatch_count(), 0, "nothing reached a host");
    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    assert!(
        ledger
            .list_for_run(&run.id.to_string(), None)
            .await
            .expect("effects")
            .is_empty(),
        "no effect was reserved"
    );
    let turns = seams
        .engine
        .store()
        .list_turns(&run.id)
        .await
        .expect("turns");
    let steps = seams
        .engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps");
    let tool_step = steps
        .iter()
        .find(|step| step.kind == StepKind::ToolCall)
        .expect("tool step");
    assert_eq!(tool_step.status, StepStatus::Failed);
    finish(fixture).await;
}

#[tokio::test]
async fn unknown_tools_and_unknown_fields_are_rejected_before_dispatch() {
    let Some(fixture) = prepare("run011_reject").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![
                ProposedToolCall::new("call_unknown", "fs.teleport", json!({ "path": "/a" })),
                ProposedToolCall::new(
                    "call_extra",
                    "fs.read",
                    json!({ "path": "/work/root/a", "extra": true }),
                ),
                ProposedToolCall::new("call_type", "fs.read", json!({ "path": 42 })),
            ],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("correcting".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "refusals are model-correctable, not fatal: {outcome:?}"
    );
    assert_eq!(
        host.dispatch_count(),
        0,
        "no host saw a refused call; rows: {:?}",
        tool_call_rows(&fixture).await
    );

    let turns = seams
        .engine
        .store()
        .list_turns(&run.id)
        .await
        .expect("turns");
    let steps = seams
        .engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps");
    let refused: Vec<_> = steps
        .iter()
        .filter(|step| step.kind == StepKind::ToolCall)
        .collect();
    assert_eq!(refused.len(), 3);
    for step in refused {
        assert_eq!(step.status, StepStatus::Failed, "{step:?}");
    }
    assert_eq!(
        events_of_type(&fixture.pool, "tool.rejected").await.len(),
        3
    );

    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    assert!(
        ledger
            .list_for_run(&run.id.to_string(), None)
            .await
            .expect("effects")
            .is_empty(),
        "a refused call reserves no effect"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn independent_tool_calls_run_together_and_settle_in_proposal_order() {
    let Some(fixture) = prepare("run011_parallel").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![
                read_tool("call_a", "/work/root/a.md"),
                read_tool("call_b", "/work/root/b.md"),
            ],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("done".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    // Two parties: neither host call can return until the other has started, so a runtime
    // that dispatched them one after another would time out instead of passing.
    *host.barrier.lock().expect("barrier") = Some(Arc::new(tokio::sync::Barrier::new(2)));
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");

    let timeline = host.timeline();
    assert!(
        !timeline
            .iter()
            .any(|entry| entry.starts_with("no-overlap:")),
        "both dispatches were in flight together: {timeline:?}"
    );
    assert_eq!(timeline.len(), 4, "two starts and two ends: {timeline:?}");
    assert!(
        timeline[0].starts_with("start:") && timeline[1].starts_with("start:"),
        "both dispatches started before either finished: {timeline:?}"
    );

    // The durable order is the proposal order, not the completion order.
    let turns = seams
        .engine
        .store()
        .list_turns(&run.id)
        .await
        .expect("turns");
    let steps = seams
        .engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps");
    let tool_steps: Vec<_> = steps
        .iter()
        .filter(|step| step.kind == StepKind::ToolCall)
        .collect();
    assert_eq!(tool_steps[0].step_ref.as_deref(), Some("call_a"));
    assert_eq!(tool_steps[1].step_ref.as_deref(), Some("call_b"));
    finish(fixture).await;
}

#[tokio::test]
async fn a_tier_three_call_parks_for_approval_and_resumes_without_a_duplicate_dispatch() {
    let Some(fixture) = prepare("run011_approval").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![ProposedToolCall::new(
                "call_host_write",
                "fs.write",
                json!({
                    "path": "/etc/hosts",
                    "content": "127.0.0.1 example",
                    "content_digest": "sha256:abc"
                }),
            )],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("wrote the host file".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("a tier 3 call parks for approval: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingApproval);
    assert_eq!(
        host.dispatch_count(),
        0,
        "nothing dispatched before approval"
    );

    let parked = seams.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(parked.status, RunStatus::WaitingApproval);
    let protocol = seams
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol")
        .expect("state");
    assert_eq!(protocol.pending_approvals, vec![wait_key.clone()]);
    assert_eq!(protocol.pending_tool_calls.len(), 1);
    let effect_id = protocol.pending_tool_calls[0].effect_id.clone();

    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    assert_eq!(
        ledger.load(&effect_id).await.expect("effect").status,
        EffectStatus::Proposed,
        "the effect waits for the receipt"
    );

    // A human grants it; the run resumes through its own authoritative transition.
    let signer = ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer");
    let approvals =
        ApprovalRuntime::new(fixture.pool.clone(), fixture.identity.clone()).expect("approvals");
    approvals
        .grant_and_resume(
            &wait_key,
            USER,
            parked.generation,
            &signer,
            chrono::Utc::now(),
        )
        .await
        .expect("grant");
    let resumed = seams.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(resumed.status, RunStatus::Running);

    // Continuing the turn resumes the recorded call; the model is not asked to propose it
    // again and the host sees exactly one dispatch.
    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            resumed.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("resumed turn");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the resumed turn completes: {outcome:?}"
    );
    assert_eq!(host.dispatch_count(), 1, "exactly one dispatch");
    assert_eq!(
        ledger.load(&effect_id).await.expect("effect").status,
        EffectStatus::SettledSuccess
    );
    let effects = ledger
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects.len(), 1, "no duplicate effect was reserved");
    finish(fixture).await;
}

#[tokio::test]
async fn a_crash_between_dispatch_and_settlement_reconciles_instead_of_redispatching() {
    let Some(fixture) = prepare("run011_unknown").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![ModelProposal {
        tool_calls: vec![ProposedToolCall::new(
            "call_term",
            "terminal.exec",
            json!({ "command": "rm -rf build", "cwd": "/work/root" }),
        )],
        ..ModelProposal::default()
    }]);
    let host = ConformanceHost::default();
    host.unknown
        .lock()
        .expect("unknown")
        .insert("terminal.exec".to_string());
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::EffectUnsettled { effect_id, .. } = outcome else {
        panic!("an unknown outcome parks the run: {outcome:?}");
    };
    assert_eq!(host.dispatch_count(), 1);

    let effect_id = effect_id.expect("effect");
    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    assert_eq!(
        ledger.load(&effect_id).await.expect("effect").status,
        EffectStatus::OutcomeUnknown
    );
    let parked = seams.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(parked.status, RunStatus::Suspended);

    // A restarted runtime reads durable state and reports reconciliation, never a retry.
    let restarted = build_seams(&fixture, seams.model.clone(), host.clone());
    let recovery = restarted.engine.recover(&run.id).await.expect("recover");
    assert!(
        matches!(
            recovery,
            quansio_server::runtime::state_machine::RecoveryOutcome::ReconcileEffect { .. }
        ),
        "recovery reconciles: {recovery:?}"
    );
    assert_eq!(
        host.dispatch_count(),
        1,
        "no duplicate dispatch after restart"
    );

    // Reconciliation resolves it with evidence, and only then does the run continue.
    // A `process.exec.sandboxed` record carries the `none` strategy (config/effects.yaml):
    // its unknown outcome is resolved by a human with evidence, never by a guess, and a
    // "determined" claim is refused.
    let determined = ledger
        .reconcile(
            &effect_id,
            quansio_server::effects::ReconciliationEvidence::Determined {
                landed: true,
                remote_ref: None,
                evidence_ids: Vec::new(),
            },
        )
        .await;
    assert!(
        determined.is_err(),
        "a class with no reconciliation strategy cannot be resolved by query"
    );
    ledger
        .reconcile(
            &effect_id,
            quansio_server::effects::ReconciliationEvidence::Manual {
                evidence_ids: vec!["evd_manual_check".to_string()],
            },
        )
        .await
        .expect("manual reconcile");
    assert_eq!(
        ledger.load(&effect_id).await.expect("effect").status,
        EffectStatus::ReconciliationManual
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_question_wait_survives_a_restart_and_resumes_on_the_answer() {
    let Some(fixture) = prepare("run011_question").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            question: Some(ProposedQuestion {
                question_id: "question_1".to_string(),
                prompt: "Which environment should I deploy to?".to_string(),
                kind: "single_choice".to_string(),
                options: vec!["staging".to_string(), "production".to_string()],
                required: true,
            }),
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("deployed".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("a question parks the run: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingQuestion);
    let question = seams.questions.load(&wait_key).await.expect("question");
    assert!(question.is_open());
    assert_eq!(question.run_id, run.id.to_string());

    // A restarted runtime finds the wait from durable state alone.
    let restarted = build_seams(&fixture, seams.model.clone(), host.clone());
    let recovery = restarted.engine.recover(&run.id).await.expect("recover");
    assert!(
        matches!(
            recovery,
            quansio_server::runtime::state_machine::RecoveryOutcome::Waiting {
                state: RunStatus::WaitingQuestion,
                ..
            }
        ),
        "recovery waits for the question: {recovery:?}"
    );

    // Answering it releases exactly this wait.
    let answered = restarted
        .questions
        .answer(
            &wait_key,
            quansio_server::runtime::turn_loop::QuestionAnswer {
                user_id: USER.to_string(),
                value: json!("staging"),
            },
        )
        .await
        .expect("answer");
    assert_eq!(
        answered.resumed_run_id.as_deref(),
        Some(run.id.to_string().as_str())
    );
    let resumed = restarted
        .engine
        .store()
        .load_run(&run.id)
        .await
        .expect("run");
    assert_eq!(resumed.status, RunStatus::Running);

    let outcome = restarted
        .engine
        .run_turn(
            &run.id,
            resumed.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("resumed turn");
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    assert_eq!(
        events_of_type(&fixture.pool, "question.asked").await.len(),
        1
    );
    assert_eq!(
        events_of_type(&fixture.pool, "question.answered")
            .await
            .len(),
        1
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_delegation_wait_survives_a_restart() {
    let Some(fixture) = prepare("run011_delegate").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![ModelProposal {
        delegate_requests: vec![quansio_server::runtime::state_machine::DelegationRequest {
            request_id: "delegate_1".to_string(),
            work_node_id: None,
            instruction: "Research the vendor options".to_string(),
        }],
        ..ModelProposal::default()
    }]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("a delegation parks the run: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingChild);
    assert!(
        wait_key.starts_with("ath_"),
        "the child thread is the wait key"
    );

    let restarted = build_seams(&fixture, seams.model.clone(), host.clone());
    let recovery = restarted.engine.recover(&run.id).await.expect("recover");
    assert!(
        matches!(
            recovery,
            quansio_server::runtime::state_machine::RecoveryOutcome::Waiting {
                state: RunStatus::WaitingChild,
                ..
            }
        ),
        "recovery waits for the child: {recovery:?}"
    );
    let children =
        sqlx::query_scalar::<_, String>("SELECT id FROM agent_threads WHERE parent_id = $1")
            .bind(fixture.agent_thread.to_string())
            .fetch_all(&fixture.pool)
            .await
            .expect("children");
    assert_eq!(children, vec![wait_key.clone()], "one durable child thread");
    finish(fixture).await;
}

#[tokio::test]
async fn a_user_ask_call_parks_the_run_and_the_question_is_durable() {
    let Some(fixture) = prepare("run011_ask").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![ModelProposal {
        tool_calls: vec![ProposedToolCall::new(
            "call_ask",
            "user.ask",
            json!({ "kind": "confirm", "prompt": "Delete the staging database?" }),
        )],
        ..ModelProposal::default()
    }]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("user.ask parks the run: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingQuestion);
    assert!(wait_key.starts_with("q_"), "a durable question is created");
    assert_eq!(host.dispatch_count(), 0, "no host executes user.ask");

    let question = seams.questions.load(&wait_key).await.expect("question");
    assert_eq!(question.kind, "confirm");
    assert_eq!(question.prompt, "Delete the staging database?");
    assert!(question.is_open());

    // An answer of the wrong shape is refused and leaves the question open.
    let refused = seams
        .questions
        .answer(
            &wait_key,
            quansio_server::runtime::turn_loop::QuestionAnswer {
                user_id: USER.to_string(),
                value: json!("yes please"),
            },
        )
        .await;
    assert!(refused.is_err(), "a confirm answer must be a boolean");
    assert!(seams
        .questions
        .load(&wait_key)
        .await
        .expect("question")
        .is_open());
    finish(fixture).await;
}

#[tokio::test]
async fn a_question_past_its_ttl_cannot_be_answered_and_the_background_pass_fails_its_run() {
    let Some(fixture) = prepare("run011_expiry").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![ModelProposal {
        tool_calls: vec![ProposedToolCall::new(
            "call_ask",
            "user.ask",
            json!({ "kind": "confirm", "prompt": "Ship it?" }),
        )],
        ..ModelProposal::default()
    }]);
    let seams = build_seams(&fixture, model, ConformanceHost::default());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;
    let TurnOutcome::Parked { wait_key, .. } = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn")
    else {
        panic!("user.ask parks the run");
    };
    let question = seams.questions.load(&wait_key).await.expect("question");
    assert!(
        question.expires_at.is_some(),
        "a question carries the policy TTL"
    );

    // Its TTL passes (the default is a day; the clock is moved rather than waited on).
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE questions SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(&wait_key)
        .execute(&mut *tx)
        .await
        .expect("age the question");
    tx.commit().await.expect("commit");

    // Before the expiry pass runs, a late answer is already refused and releases nothing.
    let late = seams
        .questions
        .answer(
            &wait_key,
            quansio_server::runtime::turn_loop::QuestionAnswer {
                user_id: USER.to_string(),
                value: json!(true),
            },
        )
        .await
        .expect_err("a question past its TTL is closed");
    assert_eq!(late.code(), "CONFLICT_STATE");
    let waiting = seams.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(waiting.status, RunStatus::WaitingQuestion);

    // The served background pass expires it and fails the run waiting on it.
    let api = quansio_server::api::ApiState::new(fixture.pool.clone()).expect("api state");
    quansio_server::composition::Background::new(fixture.pool.clone(), api)
        .pass()
        .await
        .expect("background pass");
    let expired = seams.questions.load(&wait_key).await.expect("question");
    assert!(!expired.is_open(), "{:?}", expired.status);
    assert_eq!(expired.status.as_str(), "expired");
    let failed = seams.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(failed.status, RunStatus::Failed);
    finish(fixture).await;
}

#[tokio::test]
async fn every_declared_effect_class_has_a_catalog_tier() {
    let taxonomy = quansio_server::effects::EffectTaxonomy::builtin();
    for declaration in quansio_tools::ToolRegistry::builtin().declarations() {
        for template in &declaration.grant_templates {
            assert!(
                taxonomy.get(template.effect_class.as_str()).is_some(),
                "{} declares effect class {} which is missing from config/effects.yaml",
                declaration.name,
                template.effect_class.as_str()
            );
        }
    }
}

#[tokio::test]
async fn an_unprovable_proposal_chain_escalates_a_tier_two_call_to_approval() {
    let Some(fixture) = prepare("run011_trust").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![ModelProposal {
        // `user.ask` is `record.create` (tier 2) and would be allowed outright.
        tool_calls: vec![ProposedToolCall::new(
            "call_ask",
            "user.ask",
            json!({ "kind": "confirm", "prompt": "Proceed?" }),
        )],
        ..ModelProposal::default()
    }]);
    let host = ConformanceHost::default();
    // The default source can prove nothing about the proposal's causal chain.
    let seams = build_seams_with_trust(
        &fixture,
        model,
        host.clone(),
        Arc::new(UnavailableProposalTrust),
    );
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked { state, .. } = outcome else {
        panic!("an untrusted chain escalates a tier 2 call to approval: {outcome:?}");
    };
    assert_eq!(
        state,
        RunStatus::WaitingApproval,
        "DOMAIN.md §12 rule 2 escalates one tier, and tier 3 needs a receipt"
    );
    assert_eq!(host.dispatch_count(), 0);
    finish(fixture).await;
}

/// INT-012 build item 3, end to end: an escalated call reaches the operator with its
/// untrusted origin visible, and the call it parked completes through that approval.
#[tokio::test]
async fn an_escalated_call_shows_its_untrusted_origin_and_completes_through_approval() {
    let Some(fixture) = prepare("int012_origin").await else {
        blocked_marker();
        return;
    };
    seed_policy(&fixture).await;
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![ProposedToolCall::new(
                "call_navigate",
                "browser.navigate",
                json!({ "url": "https://runbook.example.invalid/", "session_id": "brs_seed" }),
            )],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("navigated".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    // The chain proves nothing about where the proposal came from, so it fails closed to
    // UNTRUSTED_EXTERNAL and the tier 2 egress call escalates to tier 3.
    let seams = build_seams_with_trust(
        &fixture,
        model,
        host.clone(),
        Arc::new(UnavailableProposalTrust),
    );
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;
    allow_network_egress(&fixture).await;

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("an unprovable chain escalates the egress call to approval: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingApproval);
    assert_eq!(
        host.dispatch_count(),
        0,
        "nothing dispatched before the receipt"
    );

    // What the operator is shown: the origin is present, typed and named.
    let policies =
        PolicyStore::new(fixture.pool.clone(), fixture.identity.clone()).expect("policy store");
    let request = policies
        .load_approval_request(&wait_key)
        .await
        .expect("approval request");
    let origin = request
        .consequence_preview
        .untrusted_origin
        .as_ref()
        .expect("an escalated proposal reaches the operator with its origin");
    assert_eq!(origin.trust(), TrustLevel::UntrustedExternal);
    assert_eq!(
        origin
            .source_refs
            .iter()
            .filter(|reference| reference.starts_with("tool_call:"))
            .count(),
        1,
        "the origin names the call it came from: {:?}",
        origin.source_refs
    );
    assert!(origin
        .source_refs
        .iter()
        .any(|reference| reference == &format!("run:{}", run.id)));
    assert!(request.is_grantable(chrono::Utc::now()));

    let parked = seams.engine.store().load_run(&run.id).await.expect("run");
    let protocol = seams
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol")
        .expect("state");
    let effect_id = protocol.pending_tool_calls[0].effect_id.clone();
    let ledger = EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger");
    assert_eq!(
        ledger.load(&effect_id).await.expect("effect").status,
        EffectStatus::Proposed,
        "the effect waits for the receipt"
    );

    let signer = ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer");
    let approvals =
        ApprovalRuntime::new(fixture.pool.clone(), fixture.identity.clone()).expect("approvals");
    approvals
        .grant_and_resume(
            &wait_key,
            USER,
            parked.generation,
            &signer,
            chrono::Utc::now(),
        )
        .await
        .expect("grant");
    let resumed = seams.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(resumed.status, RunStatus::Running);

    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            resumed.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("resumed turn");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the approved call completes: {outcome:?}"
    );
    assert_eq!(host.dispatch_count(), 1, "exactly one dispatch");
    assert_eq!(
        ledger.load(&effect_id).await.expect("effect").status,
        EffectStatus::SettledSuccess
    );
    finish(fixture).await;
}

/// Allow one tier 2 egress class for a single test, so that *escalation* rather than a
/// policy rule is what parks the call for approval.
async fn allow_network_egress(fixture: &Fixture) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "UPDATE policies SET rules = rules || \
         '[{\"effect_class\": \"network.egress.new_destination\", \
            \"resource\": {\"kind\": \"domain\", \"selector\": \"*\"}, \
            \"decision\": \"allow\"}]'::jsonb WHERE tenant_id = $1",
    )
    .bind(TENANT)
    .execute(&mut *tx)
    .await
    .expect("egress policy rule");
    sqlx::query(
        "UPDATE capability_projections SET grants = grants || \
         '[{\"effect_class\": \"network.egress.new_destination\", \
            \"resource\": {\"kind\": \"domain\", \"selector\": \"*\"}, \
            \"constraints\": {}}]'::jsonb WHERE tenant_id = $1",
    )
    .bind(TENANT)
    .execute(&mut *tx)
    .await
    .expect("egress grant");
    tx.commit().await.expect("commit");
}

/// Remove every grant from the run's stored projection, leaving a projection that
/// authorizes nothing (the capability filter is the boundary under test).
async fn clear_grants(fixture: &Fixture) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE capability_projections SET grants = '[]'::jsonb WHERE tenant_id = $1")
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("clear grants");
    tx.commit().await.expect("commit");
}

/// A model that, like the gateway source, records the ContextProjection it was shown for the
/// turn before it proposes (INT-005), with the segment labels the test chooses.
#[derive(Clone)]
struct ProjectingModel {
    inner: StubModel,
    pool: PgPool,
    labels: Vec<(&'static str, &'static str)>,
}

#[async_trait]
impl ModelProposalSource for ProjectingModel {
    async fn propose(&self, request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let segments: Vec<Value> = self
            .labels
            .iter()
            .map(|(segment, level)| json!({ "segment_id": segment, "trust_level": level }))
            .collect();
        let mut generator = UlidGenerator::new();
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant context");
        sqlx::query(
            "INSERT INTO context_projections (id, tenant_id, run_id, turn_id, segments) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(format!("ctx_{}", generator.generate()))
        .bind(TENANT)
        .bind(&request.run_id)
        .bind(&request.turn_id)
        .bind(Value::Array(segments))
        .execute(&mut *tx)
        .await
        .expect("context projection");
        tx.commit().await.expect("commit");
        self.inner.propose(request).await
    }
}

/// Run one `user.ask` turn (tier 2, allowed by policy) with the composed proposal-trust
/// source over a recorded projection labelled `labels`, and return the state it parked in.
async fn ask_with_recorded_context(
    prefix: &str,
    labels: Vec<(&'static str, &'static str)>,
) -> Option<(RunStatus, usize)> {
    let fixture = prepare(prefix).await?;
    seed_policy(&fixture).await;
    let model = ProjectingModel {
        inner: StubModel::with(vec![ModelProposal {
            tool_calls: vec![ProposedToolCall::new(
                "call_ask",
                "user.ask",
                json!({ "kind": "confirm", "prompt": "Proceed?" }),
            )],
            ..ModelProposal::default()
        }]),
        pool: fixture.pool.clone(),
        labels,
    };
    let host = ConformanceHost::default();
    // The source the served composition installs (`api::commands::chat_engine`).
    let seams = build_seams_with_trust(
        &fixture,
        StubModel::default(),
        host.clone(),
        Arc::new(StoredProposalTrust::new(
            fixture.pool.clone(),
            &fixture.identity,
        )),
    );
    let engine = seams.engine.with_model_source(Arc::new(model));
    let run = start_run(&fixture, &engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;
    let outcome = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked { state, .. } = outcome else {
        panic!("user.ask parks the run either way: {outcome:?}");
    };
    let dispatched = host.dispatch_count();
    finish(fixture).await;
    Some((state, dispatched))
}

/// INT-012 composed: a call whose turn was shown only trusted segments is labelled from the
/// recorded projection and is not escalated — the tier 2 `user.ask` goes straight to its
/// question instead of an approval.
#[tokio::test]
async fn a_proposal_from_a_trusted_context_window_is_not_escalated() {
    let Some((state, dispatched)) = ask_with_recorded_context(
        "int012_trusted",
        vec![("system", "TRUSTED_SYSTEM"), ("msg_1", "TRUSTED_USER")],
    )
    .await
    else {
        blocked_marker();
        return;
    };
    assert_eq!(
        state,
        RunStatus::WaitingQuestion,
        "a trusted window derives TRUSTED_USER, so policy's allow stands"
    );
    assert_eq!(dispatched, 0);
}

/// The same call after the turn was shown an untrusted segment is escalated to approval.
#[tokio::test]
async fn a_proposal_from_an_untrusted_context_window_is_escalated() {
    let Some((state, _)) = ask_with_recorded_context(
        "int012_untrusted",
        vec![("msg_1", "TRUSTED_USER"), ("page_1", "UNTRUSTED_EXTERNAL")],
    )
    .await
    else {
        blocked_marker();
        return;
    };
    assert_eq!(state, RunStatus::WaitingApproval);
}

/// An unknown label in the window fails closed, exactly as no record at all does.
#[tokio::test]
async fn a_proposal_from_an_unlabelled_context_window_fails_closed() {
    let Some((state, _)) = ask_with_recorded_context(
        "int012_unlabelled",
        vec![("msg_1", "TRUSTED_USER"), ("odd", "SOMEWHAT_TRUSTED")],
    )
    .await
    else {
        blocked_marker();
        return;
    };
    assert_eq!(state, RunStatus::WaitingApproval);
}

// ---------------------------------------------------------------------------------------
// Autonomy modes (RUN-006 over RUN-011): a setting that only ever narrows what policy allowed
// ---------------------------------------------------------------------------------------

/// Set `autonomy_mode` on one settings layer (`tenants`, `workspaces` or `users`).
async fn set_autonomy(fixture: &Fixture, table: &str, id: &str, mode: &str) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(&format!(
        "UPDATE {table} SET settings = jsonb_set(settings, '{{autonomy_mode}}', to_jsonb($1::text)) WHERE id = $2"
    ))
    .bind(mode)
    .bind(id)
    .execute(&mut *tx)
    .await
    .expect("set the mode");
    tx.commit().await.expect("commit");
}

/// One turn proposing a read and a sandboxed command (tier 0 and tier 1, both allowed by the seeded policy).
async fn turn_with_read_and_command(
    label: &str,
    mode: Option<(&str, &str, &str)>,
) -> (usize, TurnOutcome, Vec<(String, String, Option<Value>)>) {
    let fixture = prepare(label).await.expect("the dev database");
    seed_policy(&fixture).await;
    if let Some((table, id, mode)) = mode {
        set_autonomy(&fixture, table, id, mode).await;
    }
    let model = StubModel::with(vec![
        ModelProposal {
            tool_calls: vec![
                read_tool("call_read", "/work/root/notes.md"),
                ProposedToolCall::new(
                    "call_term",
                    "terminal.exec",
                    json!({ "command": "ls -la", "cwd": "/work/root" }),
                ),
            ],
            ..ModelProposal::default()
        },
        ModelProposal {
            assistant_text: Some("done".to_string()),
            ..ModelProposal::default()
        },
    ]);
    let host = ConformanceHost::default();
    let seams = build_seams(&fixture, model, host.clone());
    let run = start_run(&fixture, &seams.engine, 8).await;
    seed_projection(&fixture, &run.id.to_string()).await;
    let outcome = seams
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let rows = tool_call_rows(&fixture).await;
    let dispatched = host.dispatch_count();
    finish(fixture).await;
    (dispatched, outcome, rows)
}

#[tokio::test]
async fn the_autonomy_mode_in_force_decides_what_an_agent_may_do_without_asking() {
    if admin_url().is_none() {
        return blocked_marker();
    }
    // The control: with no mode set, both calls run.
    let (dispatched, outcome, _) = turn_with_read_and_command("run006_standard", None).await;
    assert_eq!(dispatched, 2, "standard runs the read and the command");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    // `read_only`, set by the workspace: the read runs and the command is refused before any host.
    let (dispatched, _, rows) = turn_with_read_and_command(
        "run006_readonly",
        Some(("workspaces", WORKSPACE, "read_only")),
    )
    .await;
    assert_eq!(dispatched, 1, "only the read reached a host: {rows:?}");
    let command = rows
        .iter()
        .find(|(tool, _, _)| tool == "terminal.exec")
        .expect("the command's row");
    assert_eq!(command.1, "rejected", "{rows:?}");
    assert!(
        command
            .2
            .as_ref()
            .is_some_and(|result| result.to_string().contains("autonomy_read_only")),
        "{rows:?}"
    );

    // `ask_every_action`, set by the person themselves (their layer, stricter than the workspace's): the command parks for
    // approval and nothing but the read is dispatched.
    let (dispatched, outcome, _) =
        turn_with_read_and_command("run006_askevery", Some(("users", USER, "ask_every_action")))
            .await;
    assert_eq!(dispatched, 1, "the command waits for a person");
    let TurnOutcome::Parked { state, .. } = outcome else {
        panic!("the command parks for approval: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingApproval);

    // `ask_for_consequences` leaves the sandboxed command (tier 1) alone.
    let (dispatched, outcome, _) = turn_with_read_and_command(
        "run006_asksome",
        Some(("tenants", TENANT, "ask_for_consequences")),
    )
    .await;
    assert_eq!(dispatched, 2);
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
}
