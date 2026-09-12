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
    ActorRoles, ApprovalRuntime, ApprovalSigner, TenantRole, WorkspaceRole,
};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    Budget, DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun,
    ProposedQuestion, ProposedToolCall, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError,
    RuntimeIdentity, StepKind, StepStatus, TurnInput, TurnOutcome,
};
use quansio_server::runtime::turn_loop::{
    load_turn_usage, ActingActor, HostDispatch, HostFailure, HostOutcome, QuestionService,
    RoleProvider, StoredProjectionProvider, ToolDispatchService, ToolHostPort,
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
#[derive(Clone, Default)]
struct ConformanceHost {
    dispatches: Arc<Mutex<Vec<HostDispatch>>>,
    timeline: Arc<Mutex<Vec<String>>>,
    unknown: Arc<Mutex<HashSet<String>>>,
    failing: Arc<Mutex<HashSet<String>>>,
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
        self.timeline
            .lock()
            .expect("timeline")
            .push(format!("start:{}", dispatch.tool_call_id));
        // Yielding here lets a concurrently dispatched call start before this one finishes,
        // which is what the ordering test observes.
        tokio::task::yield_now().await;
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
    assert_eq!(host.dispatch_count(), 2, "both tool calls were dispatched");

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
    assert_eq!(host.dispatch_count(), 0, "no host saw a refused call");

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

    // Both calls started before either finished: the host yielded while the other ran.
    let timeline = host.timeline();
    assert_eq!(timeline.len(), 4, "two starts and two ends: {timeline:?}");
    assert!(
        timeline[0].starts_with("start:") && timeline[1].starts_with("start:"),
        "both dispatches overlapped: {timeline:?}"
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
    ledger
        .reconcile(
            &effect_id,
            quansio_server::effects::ReconciliationEvidence::Determined {
                landed: true,
                remote_ref: Some("terminal://rm".to_string()),
                evidence_ids: vec!["evd_reconciled".to_string()],
            },
        )
        .await
        .expect("reconcile");
    assert_eq!(
        ledger.load(&effect_id).await.expect("effect").status,
        EffectStatus::ReconciledSuccess
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
