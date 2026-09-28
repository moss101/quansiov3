//! RUN-011 coverage: the runtime-handled control tools (`user.ask`, `work.delegate`,
//! `work.propose_plan`, `memory.propose`), the role and projection seams, and the edge cases
//! of resuming a call that parked for approval (DOMAIN.md §5.6, §7.4, §7.5).
//!
//! The model, the tool host and the plan/memory ports are conformance stubs that record
//! what they were given; the Tool Registry, the Capability Projection check, policy and
//! approvals, the Effect Ledger, the operator switches and the Run/Turn/Step state machine
//! are real.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`;
//! absent, every test prints the `BLOCKED_EXTERNAL` marker and returns.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::effects::{EffectLedger, EffectStatus};
use quansio_server::policy::{
    ActorRoles, ApprovalRuntime, ApprovalSigner, DispatchBinding, PolicyStore, TenantRole,
    TrustLevel, WorkspaceRole,
};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::protocol_state::PendingToolCall;
use quansio_server::runtime::state_machine::{
    Budget, DelegationContext, DelegationOutcome, DelegationPort, DelegationRequest,
    ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedToolCall, Run, RunStatus,
    RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, StepKind, StepStatus,
    ToolDispatchOutcome, ToolDispatchPort, ToolDispatchRequest, TurnInput, TurnOutcome,
};
use quansio_server::runtime::turn_loop::dispatch::bound_output;
use quansio_server::runtime::turn_loop::{
    ActingActor, HostDispatch, HostFailure, HostOutcome, MemoryProposalContext, MemoryProposalPort,
    PlanApplicationContext, PlanApplicationPort, ProposalTrustSource, RoleProvider,
    StoredProjectionProvider, ToolDispatchService, ToolHostPort, UnavailableRoles,
};
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA0A1";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA0A1";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0A1";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0A1";
const FS_ROOT: &str = "/work/root";
const TEST_KEY: &[u8] = b"qa001-internal-tools-signing-key";

// ---------------------------------------------------------------------------------------
// Conformance seams
// ---------------------------------------------------------------------------------------

/// A model that returns scripted proposals, one per model call, then plain text.
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

/// One scripted model round proposing a single tool call.
fn one_call(call_id: &str, tool: &str, args: Value) -> StubModel {
    StubModel::with(vec![ModelProposal {
        tool_calls: vec![ProposedToolCall::new(call_id, tool, args)],
        ..ModelProposal::default()
    }])
}

/// A host that records every dispatch and answers success, or loses the outcome of the
/// tools named in `unknown`.
#[derive(Clone, Default)]
struct RecordingHost {
    dispatches: Arc<Mutex<Vec<HostDispatch>>>,
    unknown: Arc<Mutex<HashSet<String>>>,
}

impl RecordingHost {
    fn count(&self) -> usize {
        self.dispatches.lock().expect("dispatches").len()
    }

    fn losing(tool: &str) -> Self {
        let host = Self::default();
        host.unknown
            .lock()
            .expect("unknown")
            .insert(tool.to_string());
        host
    }
}

#[async_trait]
impl ToolHostPort for RecordingHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        self.dispatches
            .lock()
            .expect("dispatches")
            .push(dispatch.clone());
        if self
            .unknown
            .lock()
            .expect("unknown")
            .contains(&dispatch.tool)
        {
            return Ok(HostOutcome::Unknown {
                detail: "the connection dropped after dispatch".to_string(),
            });
        }
        Ok(HostOutcome::Success {
            output: json!({ "ok": true }),
            evidence_ids: Vec::new(),
            remote_ref: None,
        })
    }
}

/// The model's own proposals are `AGENT_GENERATED` (DOMAIN.md §12).
#[derive(Clone, Copy)]
struct AgentTrust;

impl ProposalTrustSource for AgentTrust {
    fn proposal_trust(&self, _run_id: &str) -> Option<TrustLevel> {
        Some(TrustLevel::AgentGenerated)
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

/// A delegation seam that joins inline with a fixed result and records what it was asked.
#[derive(Clone, Default)]
struct JoiningDelegation {
    seen: Arc<Mutex<Vec<(DelegationRequest, DelegationContext)>>>,
}

#[async_trait]
impl DelegationPort for JoiningDelegation {
    async fn delegate(
        &self,
        request: DelegationRequest,
        context: DelegationContext,
    ) -> Result<DelegationOutcome, RuntimeError> {
        self.seen.lock().expect("seen").push((request, context));
        Ok(DelegationOutcome::Joined {
            result: json!({ "summary": "vendor A is cheapest", "joined": true }),
        })
    }
}

/// A plan seam that accepts every proposal and records what it was given.
#[derive(Clone, Default)]
struct RecordingPlans {
    seen: Arc<Mutex<Vec<(String, String, PlanApplicationContext)>>>,
}

#[async_trait]
impl PlanApplicationPort for RecordingPlans {
    async fn apply(
        &self,
        proposal_id: &str,
        proposal_json: &str,
        context: PlanApplicationContext,
    ) -> Result<Value, RuntimeError> {
        self.seen.lock().expect("seen").push((
            proposal_id.to_string(),
            proposal_json.to_string(),
            context,
        ));
        Ok(json!({ "accepted": true, "revision": 7 }))
    }
}

/// A memory seam that accepts every candidate and records what it was given.
#[derive(Clone, Default)]
struct RecordingMemories {
    seen: Arc<Mutex<Vec<MemoryProposalContext>>>,
}

#[async_trait]
impl MemoryProposalPort for RecordingMemories {
    async fn propose(&self, context: MemoryProposalContext) -> Result<Value, RuntimeError> {
        self.seen.lock().expect("seen").push(context);
        Ok(json!({ "accepted": true }))
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
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "fs.write.host", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" },
        { "effect_class": "record.create", "resource": { "kind": "work", "selector": "*" }, "decision": "allow" },
        { "effect_class": "memory.write", "resource": { "kind": "work", "selector": "*" }, "decision": "allow" }
    ]);
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ($1, $2, NULL, 'tenant', $3)",
    )
    .bind("pol_01J8Z3K6F1N8VQ2X5W9Y0QA0A1")
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");

    let identity = RuntimeIdentity::system(
        TENANT,
        "internal-tools-test",
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

/// Store the run's capability projection (DOMAIN.md §6.2).
async fn seed_projection(fixture: &Fixture, run_id: &str) {
    let grants: Vec<Grant> = [
        ("read.internal", "fs", "**"),
        ("fs.write.host", "fs", "**"),
        ("record.create", "work", "*"),
        ("memory.write", "work", "*"),
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
}

/// The seams a dispatcher is built with; each test swaps the ones it exercises.
struct Seams {
    host: RecordingHost,
    roles: Arc<dyn RoleProvider>,
    delegation: Option<Arc<dyn DelegationPort>>,
    plans: Option<Arc<dyn PlanApplicationPort>>,
    memories: Option<Arc<dyn MemoryProposalPort>>,
    signer: bool,
}

impl Seams {
    fn new(host: RecordingHost) -> Self {
        Self {
            host,
            roles: Arc::new(TestRoles),
            delegation: None,
            plans: None,
            memories: None,
            signer: true,
        }
    }
}

fn dispatcher(fixture: &Fixture, seams: &Seams) -> ToolDispatchService {
    let delegation = seams.delegation.clone().unwrap_or_else(|| {
        Arc::new(
            AgentDelegationPort::with_structural_check(
                fixture.pool.clone(),
                fixture.identity.clone(),
            )
            .expect("delegation port"),
        )
    });
    let projections = StoredProjectionProvider::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("projection provider");
    let mut service = ToolDispatchService::new(
        fixture.pool.clone(),
        fixture.identity.clone(),
        Arc::new(projections),
        Arc::clone(&seams.roles),
        Arc::new(seams.host.clone()),
        delegation,
    )
    .expect("dispatch service")
    .with_proposal_trust(Arc::new(AgentTrust));
    if seams.signer {
        service = service.with_signer(ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer"));
    }
    if let Some(plans) = &seams.plans {
        service = service.with_plan_port(Arc::clone(plans));
    }
    if let Some(memories) = &seams.memories {
        service = service.with_memory_port(Arc::clone(memories));
    }
    service
}

fn engine(fixture: &Fixture, model: &StubModel, service: ToolDispatchService) -> RuntimeEngine {
    let delegation: Arc<dyn DelegationPort> = Arc::new(
        AgentDelegationPort::with_structural_check(fixture.pool.clone(), fixture.identity.clone())
            .expect("delegation port"),
    );
    RuntimeEngine::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("engine")
        .with_model_source(Arc::new(model.clone()))
        .with_tool_dispatch(Arc::new(service))
        .with_delegation(delegation)
}

/// Create, enqueue and start a run bound to the fixture's execution target, and store its
/// projection.
async fn start_run(fixture: &Fixture, engine: &RuntimeEngine) -> Run {
    let mut new_run = NewRun::new(
        WORKSPACE,
        CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
        fixture.agent_thread,
        RunTriggerKind::Manual,
    )
    .with_budget(Budget::new(8));
    new_run.execution_target_id = Some(fixture.target_id.clone());
    let run = engine.create_run(new_run).await.expect("create run");
    let run = engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    let run = engine.start(&run.id, run.generation).await.expect("start");
    seed_projection(fixture, &run.id.to_string()).await;
    run
}

async fn turn(engine: &RuntimeEngine, run: &Run) -> TurnOutcome {
    let current = engine.store().load_run(&run.id).await.expect("run");
    engine
        .run_turn(
            &run.id,
            current.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn")
}

/// `(tool_name, status, result)` of every tool call row, oldest first.
async fn tool_calls(fixture: &Fixture) -> Vec<(String, String, Option<Value>)> {
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

async fn events_of_type(pool: &PgPool, event_type: &str) -> Vec<Value> {
    quansio_events::EventStore::new(pool.clone())
        .read_events_after(TENANT, None, 1_000)
        .await
        .expect("events")
        .into_iter()
        .filter(|event| event.event_type.to_string() == event_type)
        .map(|event| event.payload)
        .collect()
}

fn ledger(fixture: &Fixture) -> EffectLedger {
    EffectLedger::new(fixture.pool.clone(), fixture.identity.clone()).expect("ledger")
}

/// Engage an operator kill switch (OPS-008) directly on the owner table.
async fn engage(fixture: &Fixture, id: &str, switch: &str, target: &str) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO ops_switches (id, tenant_id, switch, target, reason, engaged_by) \
         VALUES ($1, $2, $3, $4, 'qa001 fixture', $5)",
    )
    .bind(id)
    .bind(TENANT)
    .bind(switch)
    .bind(target)
    .bind(json!({ "kind": "user", "id": USER }))
    .execute(&mut *tx)
    .await
    .expect("engage switch");
    tx.commit().await.expect("commit");
}

/// Release every engaged switch.
async fn release_all(fixture: &Fixture) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "UPDATE ops_switches SET released_at = now(), released_by = $2, \
         release_reason = 'qa001 fixture' WHERE tenant_id = $1 AND released_at IS NULL",
    )
    .bind(TENANT)
    .bind(json!({ "kind": "user", "id": USER }))
    .execute(&mut *tx)
    .await
    .expect("release switches");
    tx.commit().await.expect("commit");
}

/// A call that parked for approval, with what a resume needs.
struct ParkedCall {
    run: Run,
    wait_key: String,
    pending: PendingToolCall,
    turn_id: String,
    step_id: String,
}

impl ParkedCall {
    /// The dispatch request a resumed turn would present, at the run's current generation.
    async fn request(&self, engine: &RuntimeEngine) -> ToolDispatchRequest {
        let current = engine.store().load_run(&self.run.id).await.expect("run");
        ToolDispatchRequest {
            run_id: self.run.id.to_string(),
            turn_id: self.turn_id.clone(),
            step_id: self.step_id.clone(),
            generation: current.generation.get(),
        }
    }
}

/// Run a turn whose only call is a tier 3 host write, which policy parks for approval.
async fn park_host_write(fixture: &Fixture, engine: &RuntimeEngine) -> ParkedCall {
    let run = start_run(fixture, engine).await;
    let outcome = turn(engine, &run).await;
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("a host write parks for approval: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingApproval);
    let protocol = engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol")
        .expect("state");
    let pending = protocol.pending_tool_calls[0].clone();
    let turns = engine.store().list_turns(&run.id).await.expect("turns");
    let steps = engine
        .store()
        .list_steps(&turns[0].id)
        .await
        .expect("steps");
    let step = steps
        .iter()
        .find(|step| step.kind == StepKind::ToolCall)
        .expect("tool step");
    ParkedCall {
        run,
        wait_key,
        pending,
        turn_id: turns[0].id.to_string(),
        step_id: step.id.to_string(),
    }
}

fn host_write_model() -> StubModel {
    one_call(
        "call_host_write",
        "fs.write",
        json!({
            "path": "/etc/hosts",
            "content": "127.0.0.1 example",
            "content_digest": "sha256:abc"
        }),
    )
}

/// A human grants the parked call's approval; the run resumes to RUNNING.
async fn grant(fixture: &Fixture, engine: &RuntimeEngine, parked: &ParkedCall) -> String {
    let current = engine.store().load_run(&parked.run.id).await.expect("run");
    let approvals =
        ApprovalRuntime::new(fixture.pool.clone(), fixture.identity.clone()).expect("approvals");
    let receipt = approvals
        .grant_and_resume(
            &parked.wait_key,
            USER,
            current.generation,
            &ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer"),
            chrono::Utc::now(),
        )
        .await
        .expect("grant");
    receipt.id.to_string()
}

/// Consume a granted receipt the way a resume does, leaving the effect AUTHORIZED: the state
/// a runtime that crashed between consuming the receipt and reserving leaves behind.
async fn consume_receipt(fixture: &Fixture, effect_id: &str, receipt_id: &str) {
    let effect = ledger(fixture).load(effect_id).await.expect("effect");
    PolicyStore::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("policy store")
        .verify_and_consume_receipt(
            receipt_id,
            &DispatchBinding {
                effect_id: effect_id.to_string(),
                params_digest: effect.params_digest.clone(),
                generation: effect.generation,
            },
            &ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer"),
            chrono::Utc::now(),
        )
        .await
        .expect("consume receipt");
    assert_eq!(
        ledger(fixture)
            .load(effect_id)
            .await
            .expect("effect")
            .status,
        EffectStatus::Authorized
    );
}

// ---------------------------------------------------------------------------------------
// Internal control tools
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_work_delegate_call_spawns_a_child_thread_and_parks_the_run_on_it() {
    let Some(fixture) = prepare("qa001_it_delegate").await else {
        blocked_marker();
        return;
    };
    let model = one_call(
        "call_delegate",
        "work.delegate",
        json!({ "instruction": "Compare the three vendor quotes", "work_node_id": WORK_NODE }),
    );
    let seams = Seams::new(RecordingHost::default());
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let run = start_run(&fixture, &engine).await;

    let outcome = turn(&engine, &run).await;
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!(
            "work.delegate parks the run on its child: {outcome:?} {:?}",
            tool_calls(&fixture).await
        );
    };
    assert_eq!(state, RunStatus::WaitingChild);
    assert!(wait_key.starts_with("ath_"), "the child thread is the wait");
    assert_eq!(
        seams.host.count(),
        0,
        "no execution host runs work.delegate"
    );

    // The child is a durable AgentThread under the run's thread.
    let children: Vec<String> =
        sqlx::query_scalar("SELECT id FROM agent_threads WHERE parent_id = $1")
            .bind(fixture.agent_thread.to_string())
            .fetch_all(&fixture.pool)
            .await
            .expect("children");
    assert_eq!(children, vec![wait_key.clone()]);

    // The record the call created is the effect: it settles with the child as its remote ref.
    let effects = ledger(&fixture)
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].status, EffectStatus::SettledSuccess);
    assert_eq!(
        effects[0]
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.remote_ref.as_deref()),
        Some(wait_key.as_str())
    );
    let calls = tool_calls(&fixture).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "completed");
    assert_eq!(
        calls[0].2,
        Some(json!({ "child_agent_thread_id": wait_key })),
        "the tool result names the child"
    );
    let completed = events_of_type(&fixture.pool, "tool.completed").await;
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["tool"], json!("work.delegate"));
    assert_eq!(completed[0]["effect_class"], json!("record.create"));
    finish(fixture).await;
}

#[tokio::test]
async fn a_work_delegate_call_that_joins_inline_completes_with_the_child_result() {
    let Some(fixture) = prepare("qa001_it_join").await else {
        blocked_marker();
        return;
    };
    let model = one_call(
        "call_delegate",
        "work.delegate",
        // `work_node_id` is optional even though it is a digest argument.
        json!({ "instruction": "Summarise the quotes" }),
    );
    let joining = JoiningDelegation::default();
    let mut seams = Seams::new(RecordingHost::default());
    seams.delegation = Some(Arc::new(joining.clone()));
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let run = start_run(&fixture, &engine).await;

    let outcome = turn(&engine, &run).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a joined delegation does not park the run: {outcome:?}"
    );
    assert_eq!(
        model.calls(),
        2,
        "the loop continued after the joined result"
    );

    let seen = joining.seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 1);
    let (request, context) = &seen[0];
    assert_eq!(request.instruction, "Summarise the quotes");
    assert_eq!(request.work_node_id, None);
    assert!(
        request.request_id.starts_with("tc_"),
        "the durable tool call is the delegation request: {}",
        request.request_id
    );
    assert_eq!(context.run_id, run.id.to_string());
    assert_eq!(context.generation, run.generation.get());

    let calls = tool_calls(&fixture).await;
    assert_eq!(calls[0].1, "completed");
    assert_eq!(
        calls[0].2,
        Some(json!({ "summary": "vendor A is cheapest", "joined": true })),
        "the child's result is the call's result"
    );
    let effects = ledger(&fixture)
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects[0].status, EffectStatus::SettledSuccess);
    assert_eq!(
        effects[0]
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.remote_ref.clone()),
        None,
        "a joined delegation spawned no thread to reference"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_work_propose_plan_call_is_applied_through_the_plan_port_as_the_proposing_run() {
    let Some(fixture) = prepare("qa001_it_plan").await else {
        blocked_marker();
        return;
    };
    let proposal_json = r#"{"nodes_add":[{"id":"n1","kind":"task","title":"draft"}]}"#;
    let model = one_call(
        "call_plan",
        "work.propose_plan",
        json!({ "proposal_id": "prop_qa001", "proposal_json": proposal_json }),
    );
    let plans = RecordingPlans::default();
    let mut seams = Seams::new(RecordingHost::default());
    seams.plans = Some(Arc::new(plans.clone()));
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let run = start_run(&fixture, &engine).await;

    let outcome = turn(&engine, &run).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "an applied plan lets the turn continue: {outcome:?}"
    );
    let seen = plans.seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 1, "the plan port is called exactly once");
    let (proposal_id, json_text, context) = &seen[0];
    assert_eq!(proposal_id, "prop_qa001");
    assert_eq!(
        json_text, proposal_json,
        "the proposal reaches the port verbatim"
    );
    assert_eq!(context.run_id, run.id.to_string());
    assert_eq!(context.workspace_id, WORKSPACE);
    assert_eq!(context.agent_thread_id, fixture.agent_thread.to_string());
    assert_eq!(context.generation, run.generation.get());

    let calls = tool_calls(&fixture).await;
    assert_eq!(calls[0].0, "work.propose_plan");
    assert_eq!(calls[0].1, "completed");
    assert_eq!(calls[0].2, Some(json!({ "accepted": true, "revision": 7 })));
    let effects = ledger(&fixture)
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].status, EffectStatus::SettledSuccess);
    assert_eq!(
        seams.host.count(),
        0,
        "no execution host runs a plan proposal"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_memory_propose_call_offers_the_candidate_to_the_gated_memory_port() {
    let Some(fixture) = prepare("qa001_it_memory").await else {
        blocked_marker();
        return;
    };
    let candidate = r#"{"fact":"the user prefers metric units"}"#;
    let model = one_call(
        "call_memory",
        "memory.propose",
        json!({ "candidate_json": candidate, "scope": "workspace" }),
    );
    let memories = RecordingMemories::default();
    let mut seams = Seams::new(RecordingHost::default());
    seams.memories = Some(Arc::new(memories.clone()));
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let run = start_run(&fixture, &engine).await;

    let outcome = turn(&engine, &run).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "an accepted candidate lets the turn continue: {outcome:?}"
    );
    let seen = memories.seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].run_id, run.id.to_string());
    assert_eq!(seen[0].workspace_id, WORKSPACE);
    assert_eq!(seen[0].candidate_json, candidate);
    assert_eq!(seen[0].scope.as_deref(), Some("workspace"));

    let calls = tool_calls(&fixture).await;
    assert_eq!(calls[0].1, "completed");
    assert_eq!(calls[0].2, Some(json!({ "accepted": true })));
    let effects = ledger(&fixture)
        .list_for_run(&run.id.to_string(), None)
        .await
        .expect("effects");
    assert_eq!(effects[0].effect_class.as_str(), "memory.write");
    assert_eq!(effects[0].status, EffectStatus::SettledSuccess);
    finish(fixture).await;
}

#[tokio::test]
async fn a_user_ask_call_records_its_choices_and_optional_flag_on_the_question() {
    let Some(fixture) = prepare("qa001_it_ask").await else {
        blocked_marker();
        return;
    };
    let model = one_call(
        "call_ask",
        "user.ask",
        json!({
            "kind": "single_choice",
            "prompt": "Which region?",
            "choices": ["eu-west", "us-east"],
            "required": false
        }),
    );
    let seams = Seams::new(RecordingHost::default());
    let service = dispatcher(&fixture, &seams);
    let questions = service.questions().clone();
    let engine = engine(&fixture, &model, service);
    let run = start_run(&fixture, &engine).await;

    let outcome = turn(&engine, &run).await;
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("user.ask parks the run: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingQuestion);
    let question = questions.load(&wait_key).await.expect("question");
    assert_eq!(question.kind, "single_choice");
    assert_eq!(
        question.options,
        vec!["eu-west".to_string(), "us-east".to_string()]
    );
    assert!(!question.required, "the model's required flag is kept");
    assert_eq!(question.run_id, run.id.to_string());
    finish(fixture).await;
}

// ---------------------------------------------------------------------------------------
// Seams the dispatcher is given
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_unresolved_acting_actor_denies_the_call_and_names_the_role_owner() {
    let Some(fixture) = prepare("qa001_it_roles").await else {
        blocked_marker();
        return;
    };
    let model = one_call(
        "call_read",
        "fs.read",
        json!({ "path": "/work/root/notes.md" }),
    );
    let mut seams = Seams::new(RecordingHost::default());
    seams.roles = Arc::new(UnavailableRoles);
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let run = start_run(&fixture, &engine).await;

    let outcome = turn(&engine, &run).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(seams.host.count(), 0, "nothing reached a host");
    let calls = tool_calls(&fixture).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "rejected");
    let result = calls[0].2.clone().expect("refusal");
    assert_eq!(result["code"], json!("POLICY_DENIED"));
    assert!(
        result["detail"]
            .as_str()
            .expect("detail")
            .contains("APP-002"),
        "the refusal names the role owner: {result}"
    );
    assert!(
        ledger(&fixture)
            .list_for_run(&run.id.to_string(), None)
            .await
            .expect("effects")
            .is_empty(),
        "a policy refusal reserves no effect"
    );
    let steps = engine
        .store()
        .list_steps(&engine.store().list_turns(&run.id).await.expect("turns")[0].id)
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
async fn the_tools_exposed_to_a_run_follow_its_stored_projection() {
    let Some(fixture) = prepare("qa001_it_exposed").await else {
        blocked_marker();
        return;
    };
    let seams = Seams::new(RecordingHost::default());
    let service = dispatcher(&fixture, &seams);
    let model = StubModel::default();
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let mut new_run = NewRun::new(
        WORKSPACE,
        CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
        fixture.agent_thread,
        RunTriggerKind::Manual,
    );
    new_run.execution_target_id = Some(fixture.target_id.clone());
    let run = engine.create_run(new_run).await.expect("run");
    let now = chrono::Utc::now();

    assert!(
        service
            .exposed_tools(&run.id.to_string(), now)
            .await
            .expect("exposed")
            .is_empty(),
        "a run without a projection is offered no tool"
    );

    seed_projection(&fixture, &run.id.to_string()).await;
    let exposed = service
        .exposed_tools(&run.id.to_string(), now)
        .await
        .expect("exposed");
    for tool in ["fs.read", "user.ask", "work.delegate", "memory.propose"] {
        assert!(
            exposed.iter().any(|name| name == tool),
            "{tool} is covered by the projection: {exposed:?}"
        );
        assert!(
            service.registry().get(tool).is_some(),
            "{tool} is registered"
        );
    }
    assert!(
        !exposed.iter().any(|name| name == "browser.navigate"),
        "an egress tool the projection does not grant is not offered: {exposed:?}"
    );
    finish(fixture).await;
}

#[test]
fn a_bounded_output_never_splits_a_multibyte_character() {
    // Every "é" is two bytes, so an odd bound lands inside a character.
    let output = json!({ "text": "é".repeat(64) });
    let bounded = bound_output(output, 33);
    assert_eq!(bounded["truncated"], json!(true));
    let preview = bounded["preview"].as_str().expect("preview");
    assert!(preview.len() <= 33);
    assert_eq!(
        bounded["bytes"].as_u64().expect("bytes") as usize,
        preview.len(),
        "the reported size is the preview's"
    );
    assert!(
        preview.ends_with('é'),
        "the preview backs off to the last whole character: {preview:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Resuming a parked call
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn resume_reports_an_unsettled_effect_and_refuses_a_call_it_has_no_record_of() {
    let Some(fixture) = prepare("qa001_it_resume_guard").await else {
        blocked_marker();
        return;
    };
    let seams = Seams::new(RecordingHost::default());
    let service = dispatcher(&fixture, &seams);
    let model = StubModel::default();
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let run = start_run(&fixture, &engine).await;
    let request = ToolDispatchRequest {
        run_id: run.id.to_string(),
        turn_id: "trn_01J8Z3K6F1N8VQ2X5W9Y0QA0A1".to_string(),
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9Y0QA0A1".to_string(),
        generation: run.generation.get(),
    };
    let unknown_effect = "eff_01J8Z3K6F1N8VQ2X5W9Y0QA0A1";

    // An effect ProtocolState recorded as dispatched is reconciled, never re-dispatched.
    let dispatched = PendingToolCall {
        tool_call_id: "tc_01J8Z3K6F1N8VQ2X5W9Y0QA0A1".to_string(),
        tool_name: "terminal.exec".to_string(),
        dispatch_token: "dtk_fixture".to_string(),
        effect_id: unknown_effect.to_string(),
        effect_status: "DISPATCHED".to_string(),
    };
    let outcome = service
        .resume(&dispatched, request.clone())
        .await
        .expect("resume");
    assert_eq!(
        outcome,
        ToolDispatchOutcome::OutcomeUnknown {
            effect_id: Some(unknown_effect.to_string())
        }
    );

    // A pending call whose durable tool-call row is gone cannot be resumed.
    let orphan = PendingToolCall {
        effect_status: "PROPOSED".to_string(),
        ..dispatched
    };
    let outcome = service
        .resume(&orphan, request.clone())
        .await
        .expect("resume");
    let ToolDispatchOutcome::Rejected { code, detail } = outcome else {
        panic!("an orphaned pending call is refused: {outcome:?}");
    };
    assert_eq!(code, "NOT_FOUND");
    assert!(detail.contains(unknown_effect), "{detail}");

    // Generation 0 is never a valid fence: the dispatch is refused before anything is written.
    let refused = ToolDispatchPort::dispatch(
        &service,
        ProposedToolCall::new("call_read", "fs.read", json!({ "path": "/work/root/a" })),
        ToolDispatchRequest {
            generation: 0,
            ..request
        },
    )
    .await
    .expect_err("generation 0 is refused");
    assert_eq!(refused.code(), "VALIDATION_SCHEMA");
    assert_eq!(seams.host.count(), 0);
    assert!(
        tool_calls(&fixture).await.is_empty(),
        "nothing was recorded"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_parked_call_is_not_resumed_without_a_signing_key_a_granted_receipt_or_valid_arguments() {
    let Some(fixture) = prepare("qa001_it_resume_refuse").await else {
        blocked_marker();
        return;
    };
    // The dispatcher reads the signing key from the environment when none is installed.
    std::env::remove_var(quansio_server::policy::APPROVAL_SIGNING_KEY_ENV);
    let model = host_write_model();
    let seams = Seams::new(RecordingHost::default());
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let parked = park_host_write(&fixture, &engine).await;
    let effect_id = parked.pending.effect_id.clone();

    // Without a signing key the receipt cannot be verified, so nothing is consumed.
    let mut unsigned = Seams::new(seams.host.clone());
    unsigned.signer = false;
    let error = dispatcher(&fixture, &unsigned)
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect_err("no signing key");
    assert!(
        error.to_string().contains("signing key"),
        "the refusal says why: {error}"
    );

    // With a key but no human grant there is no receipt to consume.
    let service = dispatcher(&fixture, &seams);
    let outcome = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    let ToolDispatchOutcome::Rejected { code, .. } = outcome else {
        panic!("an ungranted call is refused: {outcome:?}");
    };
    assert_eq!(code, "APPROVAL_INVALID");

    // A recorded call whose arguments no longer validate is refused before the ledger.
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE tool_calls SET args = $2 WHERE effect_id = $1")
        .bind(&effect_id)
        .bind(json!({ "path": 42 }))
        .execute(&mut *tx)
        .await
        .expect("corrupt args");
    tx.commit().await.expect("commit");
    let outcome = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    let ToolDispatchOutcome::Rejected { code, .. } = outcome else {
        panic!("invalid recorded arguments are refused: {outcome:?}");
    };
    assert!(code.starts_with("VALIDATION"), "{code}");

    assert_eq!(seams.host.count(), 0, "nothing reached the host");
    assert_eq!(
        ledger(&fixture)
            .load(&effect_id)
            .await
            .expect("effect")
            .status,
        EffectStatus::Proposed,
        "every refusal left the effect waiting for its receipt"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn an_operator_switch_engaged_while_a_call_waits_stops_its_resume_until_released() {
    let Some(fixture) = prepare("qa001_it_resume_ops").await else {
        blocked_marker();
        return;
    };
    let model = host_write_model();
    let seams = Seams::new(RecordingHost::default());
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let parked = park_host_write(&fixture, &engine).await;
    let effect_id = parked.pending.effect_id.clone();
    let receipt = grant(&fixture, &engine, &parked).await;
    // The receipt was consumed and the runtime stopped before reserving: AUTHORIZED.
    consume_receipt(&fixture, &effect_id, &receipt).await;
    let service = dispatcher(&fixture, &seams);

    engage(
        &fixture,
        "ksw_01J8Z3K6F1N8VQ2X5W9Y0QA0B1",
        "worker.quarantine",
        &fixture.target_id,
    )
    .await;
    let outcome = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    let ToolDispatchOutcome::Rejected { code, detail } = outcome else {
        panic!("a quarantined target stops the resume: {outcome:?}");
    };
    assert_eq!(code, "TARGET_UNAVAILABLE");
    assert!(detail.contains(&fixture.target_id), "{detail}");
    release_all(&fixture).await;

    engage(
        &fixture,
        "ksw_01J8Z3K6F1N8VQ2X5W9Y0QA0B2",
        "policy.emergency_deny",
        "fs.write.*",
    )
    .await;
    let outcome = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    let ToolDispatchOutcome::Rejected { code, detail } = outcome else {
        panic!("an emergency deny stops the resume: {outcome:?}");
    };
    assert_eq!(code, "POLICY_DENIED");
    assert!(detail.contains("fs.write.*"), "{detail}");
    release_all(&fixture).await;

    assert_eq!(seams.host.count(), 0, "no switch let the call through");
    assert_eq!(
        ledger(&fixture)
            .load(&effect_id)
            .await
            .expect("effect")
            .status,
        EffectStatus::Authorized,
        "the approved effect still waits"
    );

    // Released, the approved call is reserved and dispatched exactly once.
    let outcome = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    assert!(
        matches!(outcome, ToolDispatchOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(seams.host.count(), 1);
    assert_eq!(
        ledger(&fixture)
            .load(&effect_id)
            .await
            .expect("effect")
            .status,
        EffectStatus::SettledSuccess
    );

    // A second resume of the settled call is a conflict, never a second dispatch.
    let outcome = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    let ToolDispatchOutcome::Rejected { code, detail } = outcome else {
        panic!("a settled call cannot be resumed: {outcome:?}");
    };
    assert_eq!(code, "CONFLICT_STATE");
    assert!(detail.contains("SETTLED_SUCCESS"), "{detail}");
    assert_eq!(seams.host.count(), 1, "no duplicate dispatch");
    finish(fixture).await;
}

#[tokio::test]
async fn a_reservation_that_survived_a_crash_before_dispatch_resumes_exactly_once() {
    let Some(fixture) = prepare("qa001_it_resume_reserved").await else {
        blocked_marker();
        return;
    };
    let model = host_write_model();
    let seams = Seams::new(RecordingHost::default());
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let parked = park_host_write(&fixture, &engine).await;
    let effect_id = parked.pending.effect_id.clone();
    let receipt = grant(&fixture, &engine, &parked).await;
    consume_receipt(&fixture, &effect_id, &receipt).await;
    // The runtime reserved the effect and crashed before marking it dispatched.
    let reserved = ledger(&fixture)
        .reserve_authorized(&effect_id)
        .await
        .expect("reserve");
    assert_eq!(reserved.status, EffectStatus::Reserved);

    let service = dispatcher(&fixture, &seams);
    let outcome = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    assert!(
        matches!(outcome, ToolDispatchOutcome::Completed { .. }),
        "a reserved, never-dispatched call is safe to dispatch: {outcome:?}"
    );
    let dispatches = seams.host.dispatches.lock().expect("dispatches").clone();
    assert_eq!(dispatches.len(), 1);
    assert_eq!(
        Some(dispatches[0].dispatch_token.clone()),
        reserved.dispatch_token,
        "the host is fenced with the reservation's own token"
    );
    assert_eq!(dispatches[0].step_id, parked.step_id);
    let settled = service.ledger().load(&effect_id).await.expect("effect");
    assert_eq!(settled.status, EffectStatus::SettledSuccess);
    let calls = tool_calls(&fixture).await;
    assert_eq!(calls[0].1, "completed");
    finish(fixture).await;
}

#[tokio::test]
async fn a_resumed_call_whose_outcome_is_lost_is_reported_for_reconciliation_on_every_later_resume()
{
    let Some(fixture) = prepare("qa001_it_resume_lost").await else {
        blocked_marker();
        return;
    };
    let model = host_write_model();
    let seams = Seams::new(RecordingHost::losing("fs.write"));
    let engine = engine(&fixture, &model, dispatcher(&fixture, &seams));
    let parked = park_host_write(&fixture, &engine).await;
    let effect_id = parked.pending.effect_id.clone();
    grant(&fixture, &engine, &parked).await;
    let service = dispatcher(&fixture, &seams);

    let first = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    assert_eq!(
        first,
        ToolDispatchOutcome::OutcomeUnknown {
            effect_id: Some(effect_id.clone())
        }
    );
    assert_eq!(
        ledger(&fixture)
            .load(&effect_id)
            .await
            .expect("effect")
            .status,
        EffectStatus::OutcomeUnknown
    );

    // The pending entry still says PROPOSED, but the ledger is authoritative.
    let again = service
        .resume(&parked.pending, parked.request(&engine).await)
        .await
        .expect("resume");
    assert_eq!(
        again,
        ToolDispatchOutcome::OutcomeUnknown {
            effect_id: Some(effect_id.clone())
        }
    );
    assert_eq!(seams.host.count(), 1, "an unknown outcome is never retried");
    let calls = tool_calls(&fixture).await;
    assert_eq!(calls[0].1, "unknown");
    finish(fixture).await;
}
