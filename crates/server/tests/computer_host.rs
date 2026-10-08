//! Native computer tools through the server's real turn-loop dispatcher (ADP-002, DOMAIN.md §7.5, §8.6, §8.6.1), against a
//! real PostgreSQL database and the real engine: validation, capability projection, policy, the Effect Ledger, the
//! composed `ComputerToolHost` walking a `ComputerSequence`, the control row's fence, and the run's park.
//!
//! The platform bridge is a scripted stand-in: the real one needs Accessibility and Screen Recording granted to a signed
//! helper that does not exist yet (`config/computer-use.yaml: helper.status: unpublished`), so the macOS-bound proof stays
//! `BLOCKED_EXTERNAL` (`QUANSIO_TEST_MACOS_COMPUTER_USE=1 swift test --package-path native/macos`, see ADP-002's notes).
//! Everything between the model and the bridge is real.
//!
//! * **composed**: `ApiState` routes `computer.*` to the computer host; with no bridge composed a call is refused before
//!   any input and the effect settles failed (nothing is "declared" and simulated);
//! * **receipts settle the ledger**: an `observed` read settles `SETTLED_SUCCESS`; an `unverified` click is
//!   `OUTCOME_UNKNOWN`, the run is suspended for reconciliation and nothing is retried (the bridge saw it once);
//! * **a missing permission parks the run**: nothing reaches the bridge, the effect settles failed, the run is
//!   `SUSPENDED` with `COMPUTER_PERMISSION_REQUIRED` and the attention item on `run.suspended`, a `blocked` Notification
//!   carries the same item, and only `ResumeRun` leaves the state;
//! * **terminal conditions**: a takeover (the holder is not the agent) and a run that is no longer running end the call
//!   before the bridge is asked; an application nobody recognised is `POLICY_DENIED`.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_machine::computer_use::bridge::{
    NativeBridgeError, NativeComputerAction, NativeComputerBridge,
};
use quansio_machine::computer_use::permission::{
    ForegroundControl, Permission, PermissionState, PermissionStatus,
};
use quansio_machine::computer_use::receipt::{
    ActionReceipt, BridgeReport, ExecutionMethod, ReceiptMethod,
};
use quansio_machine::computer_use::{AppIdentity, ComputerGrant, ComputerTier, KnownApps};
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use quansio_server::notify::notifier::Notifier;
use quansio_server::policy::{
    ActorRoles, ApprovalRuntime, ApprovalSigner, TenantRole, WorkspaceRole,
};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedToolCall,
    RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
    TurnOutcome,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, HostDispatch, HostOutcome, RoleProvider, StoredProjectionProvider,
    ToolDispatchService,
};
use quansio_tools::RegistryHandle;
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP002";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP002";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP002";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP002";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9YADP002";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9YADP002";
const THREAD: &str = "thr_01J8Z3K6F1N8VQ2X5W9YADP002";
const MESSAGE: &str = "msg_01J8Z3K6F1N8VQ2X5W9YADP002";
const APP: &str = "com.example.editor";
const SIGNING_KEY: &[u8] = b"computer-host-test-approval-key!";

#[derive(Default)]
struct BridgeState {
    permissions: Mutex<Option<PermissionStatus>>,
    performed: AtomicUsize,
    actions: Mutex<Vec<String>>,
    reports: Mutex<VecDeque<BridgeReport>>,
    foreground: Mutex<Option<String>>,
}

/// A bridge whose answers the test scripts: the real one needs permissions only a person at a Mac can grant.
#[derive(Clone, Default)]
struct ScriptedBridge(Arc<BridgeState>);

impl ScriptedBridge {
    fn deny(&self, permission: Permission, state: PermissionState) {
        *self.0.permissions.lock().expect("permissions") =
            Some(PermissionStatus::all_granted().with(permission, state));
    }
    fn set_foreground(&self, bundle_id: &str) {
        *self.0.foreground.lock().expect("foreground") = Some(bundle_id.to_string());
    }
    fn grant_all(&self) {
        *self.0.permissions.lock().expect("permissions") = Some(PermissionStatus::all_granted());
    }
    fn push(&self, report: BridgeReport) {
        self.0.reports.lock().expect("reports").push_back(report);
    }
    fn performed(&self) -> usize {
        self.0.performed.load(Ordering::SeqCst)
    }
    fn actions(&self) -> Vec<String> {
        self.0.actions.lock().expect("actions").clone()
    }
}

impl NativeComputerBridge for ScriptedBridge {
    fn foreground_app(&self) -> Result<AppIdentity, NativeBridgeError> {
        let bundle = self
            .0
            .foreground
            .lock()
            .expect("foreground")
            .clone()
            .unwrap_or_else(|| APP.to_string());
        Ok(AppIdentity::new(bundle, "Editor", 4242))
    }
    fn execute(&self, action: &NativeComputerAction) -> Result<Value, NativeBridgeError> {
        Ok(json!({ "application": action.application() }))
    }
    fn permission_status(&self) -> Result<PermissionStatus, NativeBridgeError> {
        Ok(self
            .0
            .permissions
            .lock()
            .expect("permissions")
            .unwrap_or_else(PermissionStatus::all_granted))
    }
    fn foreground_control(&self) -> Result<ForegroundControl, NativeBridgeError> {
        Ok(ForegroundControl::Held)
    }
    fn perform(&self, action: &NativeComputerAction) -> Result<BridgeReport, NativeBridgeError> {
        self.0.performed.fetch_add(1, Ordering::SeqCst);
        self.0
            .actions
            .lock()
            .expect("actions")
            .push(format!("{:?}", action.computer_tier()));
        if let Some(report) = self.0.reports.lock().expect("reports").pop_front() {
            return Ok(report);
        }
        let method = action.default_method();
        let receipt = if action.is_consequential() {
            ActionReceipt::unverified(method, ExecutionMethod::Foreground)
        } else {
            ActionReceipt::observed(method, ExecutionMethod::Foreground)
        };
        Ok(BridgeReport {
            output: match action {
                NativeComputerAction::Read { application, .. } => {
                    json!({ "application": application, "nodes": [], "truncated": false })
                }
                _ => json!({ "clicked": true }),
            },
            receipt,
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

struct Harness {
    name: String,
    pool: PgPool,
    engine: RuntimeEngine,
    state: ApiState,
    bridge: ScriptedBridge,
    moves: Arc<Mutex<VecDeque<(String, Value)>>>,
    approvals: ApprovalRuntime,
}

impl Harness {
    /// A tenant with a native computer (a target and its control row, the agent holding it) and the composed host,
    /// with a bridge when `bridge` is true.
    async fn prepare(prefix: &str, bridge: bool) -> Option<Self> {
        let name = admin_url().map(|_| scratch_name(prefix))?;
        let pool = fresh_database(&name).await?;
        schema::migrate(&pool).await.expect("migrate");
        seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
        let mut tx = pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        for statement in [
            "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
             VALUES ($2, $1, $3, 'teammate', 1, 'ACTIVE')",
            "INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, status, owner_kind) \
             VALUES ($4, $1, $3, 'persistent_workspace_computer', 'local_capsule_macos', 'READY', 'workspace')",
            "INSERT INTO computer_controls (target_id, tenant_id, holder, generation, control_since) \
             VALUES ($4, $1, 'agent', 1, now())",
            "INSERT INTO threads (id, tenant_id, workspace_id, kind, title) VALUES ($5, $1, $3, 'direct', 'chat')",
            "INSERT INTO thread_participants (id, tenant_id, thread_id, kind, principal_id, role) \
             VALUES ('thp_01J8Z3K6F1N8VQ2X5W9YADP002', $1, $5, 'user', $6, 'owner')",
            "INSERT INTO messages (id, tenant_id, workspace_id, thread_id, seq, author_kind, author_id) \
             VALUES ($7, $1, $3, $5, 1, 'user', $6)",
        ] {
            sqlx::query(statement)
                .bind(TENANT)
                .bind(AGENT)
                .bind(WORKSPACE)
                .bind(TARGET)
                .bind(THREAD)
                .bind(USER)
                .bind(MESSAGE)
                .execute(&mut *tx)
                .await
                .unwrap_or_else(|error| panic!("{statement}: {error}"));
        }
        sqlx::query(
            "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
             VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YADP002', $1, NULL, 'tenant', $2)",
        )
        .bind(TENANT)
        .bind(json!([
            { "effect_class": "computer.input.privileged", "resource": { "kind": "app", "selector": "*" }, "decision": "allow" },
            { "effect_class": "read.internal", "resource": { "kind": "app", "selector": "*" }, "decision": "allow" }
        ]))
        .execute(&mut *tx)
        .await
        .expect("policy");
        tx.commit().await.expect("commit");

        let pool_for_approvals = pool.clone();
        let scripted = ScriptedBridge::default();
        let mut state = ApiState::new(pool.clone()).expect("api");
        if bridge {
            state = state.with_computer_bridge(
                Arc::new(scripted.clone()),
                KnownApps::of([APP]),
                ComputerGrant::up_to(ComputerTier::SystemKey),
            );
        }
        let identity = RuntimeIdentity::system(
            TENANT,
            "computer-test",
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        let delegation: Arc<dyn DelegationPort> = Arc::new(
            AgentDelegationPort::with_structural_check(pool.clone(), identity.clone())
                .expect("delegation"),
        );
        let dispatcher = Arc::new(
            ToolDispatchService::new(
                pool.clone(),
                identity.clone(),
                Arc::new(
                    StoredProjectionProvider::new(pool.clone(), identity.clone())
                        .expect("projections"),
                ),
                Arc::new(Roles),
                state.host(),
                Arc::clone(&delegation),
            )
            .expect("dispatch")
            .with_signer(ApprovalSigner::new(SIGNING_KEY.to_vec()).expect("signer"))
            .with_registry_handle(RegistryHandle::builtin()),
        );
        let identity_for_approvals = RuntimeIdentity::system(
            TENANT,
            "computer-approver",
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        let moves = Arc::new(Mutex::new(VecDeque::new()));
        let engine = RuntimeEngine::new(pool.clone(), identity)
            .expect("engine")
            .with_model_source(Arc::new(Scripted {
                moves: Arc::clone(&moves),
            }))
            .with_tool_dispatch(dispatcher)
            .with_delegation(delegation);
        Some(Self {
            name,
            pool,
            engine,
            state,
            bridge: scripted,
            moves,
            approvals: ApprovalRuntime::new(pool_for_approvals, identity_for_approvals)
                .expect("approvals"),
        })
    }

    async fn new_run(&self) -> CanonicalId {
        let mut run = NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
            CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
            RunTriggerKind::Manual,
        );
        run.trigger_ref = Some(MESSAGE.to_string());
        run.execution_target_id = Some(TARGET.to_string());
        let run = self.engine.create_run(run).await.expect("run");
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
        self.seed_projection(&run.id.to_string()).await;
        run.id
    }

    /// One governed call in a fresh run; returns the run id and the turn's outcome.
    async fn call(&self, tool: &str, arguments: Value) -> (CanonicalId, TurnOutcome) {
        let run = self.new_run().await;
        let outcome = self.turn(&run, tool, arguments).await;
        (run, outcome)
    }

    /// One model turn proposing `tool`. A tier 3 action waits for a person's approval (DOMAIN.md 7.1); the person here
    /// grants it and the turn continues with the recorded call, which is what this suite is about.
    async fn turn(&self, run: &CanonicalId, tool: &str, arguments: Value) -> TurnOutcome {
        self.moves
            .lock()
            .expect("moves")
            .push_back((tool.to_string(), arguments));
        let current = self.engine.store().load_run(run).await.expect("run");
        let mut outcome = self
            .engine
            .run_turn(
                run,
                current.generation,
                TurnInput::new(RunTriggerKind::Manual),
            )
            .await
            .expect("turn");
        if let TurnOutcome::Parked {
            state: RunStatus::WaitingApproval,
            wait_key,
            ..
        } = &outcome
        {
            let parked = self.engine.store().load_run(run).await.expect("run");
            self.approvals
                .grant_and_resume(
                    wait_key,
                    USER,
                    parked.generation,
                    &ApprovalSigner::new(SIGNING_KEY.to_vec()).expect("signer"),
                    Utc::now(),
                )
                .await
                .expect("grant");
            let resumed = self.engine.store().load_run(run).await.expect("run");
            outcome = self
                .engine
                .run_turn(
                    run,
                    resumed.generation,
                    TurnInput::new(RunTriggerKind::Manual),
                )
                .await
                .expect("resumed turn");
        }
        outcome
    }

    async fn seed_projection(&self, run_id: &str) {
        let grants: Vec<Grant> = [
            ("computer.input.privileged", "app", "*"),
            ("read.internal", "app", "*"),
        ]
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

    async fn rows<T>(&self, statement: &str, bind: &str) -> Vec<T>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin,
    {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows = sqlx::query_as(statement)
            .bind(TENANT)
            .bind(bind)
            .fetch_all(&mut *tx)
            .await
            .expect("rows");
        tx.commit().await.expect("commit");
        rows
    }

    /// The newest call row of a tool: `(status, result)`.
    async fn call_row(&self, run: &str) -> (String, Value) {
        let rows: Vec<(String, Option<Value>)> = self
            .rows(
                "SELECT tc.status, tc.result FROM tool_calls tc \
                 JOIN steps s ON s.tenant_id = tc.tenant_id AND s.id = tc.step_id \
                 JOIN turns t ON t.tenant_id = s.tenant_id AND t.id = s.turn_id \
                 WHERE tc.tenant_id = $1 AND t.run_id = $2 \
                 ORDER BY tc.created_at DESC, tc.id DESC LIMIT 1",
                run,
            )
            .await;
        let (status, result) = rows.into_iter().next().expect("the call was recorded");
        (status, result.unwrap_or(Value::Null))
    }

    async fn effects(&self, run: &str) -> Vec<(String, String)> {
        self.rows(
            "SELECT effect_class, status FROM effect_records WHERE tenant_id = $1 AND run_id = $2 ORDER BY created_at",
            run,
        )
        .await
    }

    async fn status(&self, run: &CanonicalId) -> (RunStatus, Option<String>) {
        let run = self.engine.store().load_run(run).await.expect("run");
        (run.status, run.terminal_reason)
    }

    async fn event_payloads(&self, run: &str, event_type: &str) -> Vec<Value> {
        let rows: Vec<(Value,)> = self
            .rows(
                &format!("SELECT payload FROM runtime_events WHERE tenant_id = $1 AND aggregate_id = $2 AND type = '{event_type}' ORDER BY sequence"),
                run,
            )
            .await;
        rows.into_iter().map(|(payload,)| payload).collect()
    }

    async fn control(&self) -> (String, i64, Option<String>) {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let row: (String, i64, Option<String>) = sqlx::query_as(
            "SELECT holder, generation, active_tool_call_id FROM computer_controls WHERE tenant_id = $1 AND target_id = $2",
        )
        .bind(TENANT)
        .bind(TARGET)
        .fetch_one(&mut *tx)
        .await
        .expect("control row");
        tx.commit().await.expect("commit");
        row
    }

    async fn finish(self) {
        drop_pool(&self.pool, &self.name).await;
    }
}

async fn harness(prefix: &str, bridge: bool) -> Option<Harness> {
    let harness = Harness::prepare(prefix, bridge).await;
    if harness.is_none() && admin_url().is_none() {
        blocked_marker();
    }
    harness
}

fn read_args() -> Value {
    json!({ "application": APP, "max_nodes": 16, "max_depth": 4, "include_screenshot": false })
}

fn click_args() -> Value {
    json!({ "application": APP, "x": 10, "y": 20, "button": "left" })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_goes_through_the_sequence_and_its_receipt_settles_the_ledger() {
    let Some(h) = harness("computer_read", true).await else {
        return;
    };
    let (run, _) = h.call("computer.read", read_args()).await;
    let run_id = run.to_string();
    let (status, result) = h.call_row(&run_id).await;
    assert_eq!(status, "completed", "{result:#}");
    assert_eq!(
        result["receipt"]["outcome"],
        json!("observed"),
        "{result:#}"
    );
    assert_eq!(result["receipt"]["effect_verified"], Value::Null);
    assert_eq!(
        h.effects(&run_id).await,
        vec![("read.internal".to_string(), "SETTLED_SUCCESS".to_string())]
    );
    assert_eq!(h.bridge.performed(), 1);
    // The slot was released and the holder is still the agent.
    assert_eq!(h.control().await, ("agent".to_string(), 1, None));
    assert_eq!(h.status(&run).await.0, RunStatus::Running);
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unverified_click_is_unknown_and_not_retried_and_the_run_waits_for_reconciliation() {
    let Some(h) = harness("computer_click", true).await else {
        return;
    };
    let (run, outcome) = h.call("computer.click", click_args()).await;
    let run_id = run.to_string();
    assert!(
        matches!(outcome, TurnOutcome::EffectUnsettled { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        h.effects(&run_id).await,
        vec![(
            "computer.input.privileged".to_string(),
            "OUTCOME_UNKNOWN".to_string()
        )]
    );
    assert_eq!(h.status(&run).await.0, RunStatus::Suspended);
    assert_eq!(
        h.bridge.performed(),
        1,
        "the input reached the platform once and is not repeated"
    );
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verified_click_settles_and_a_missing_permission_parks_the_run_without_sending_anything()
{
    let Some(h) = harness("computer_perm", true).await else {
        return;
    };
    h.bridge.push(BridgeReport {
        output: json!({ "clicked": true }),
        receipt: ActionReceipt::applied(ReceiptMethod::HidEvent, ExecutionMethod::Foreground),
    });
    let (applied, _) = h.call("computer.click", click_args()).await;
    assert_eq!(
        h.effects(&applied.to_string()).await,
        vec![(
            "computer.input.privileged".to_string(),
            "SETTLED_SUCCESS".to_string()
        )]
    );
    assert_eq!(h.bridge.performed(), 1);

    // Accessibility is withdrawn: the next click parks the run, and the bridge is not asked to do anything.
    h.bridge
        .deny(Permission::Accessibility, PermissionState::Denied);
    let (run, outcome) = h.call("computer.click", click_args()).await;
    let run_id = run.to_string();
    assert!(
        matches!(outcome, TurnOutcome::Suspended { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        h.bridge.performed(),
        1,
        "nothing was sent for the parked call"
    );
    assert_eq!(
        h.status(&run).await,
        (
            RunStatus::Suspended,
            Some("COMPUTER_PERMISSION_REQUIRED".to_string())
        )
    );
    assert_eq!(
        h.effects(&run_id).await,
        vec![(
            "computer.input.privileged".to_string(),
            "SETTLED_FAILED".to_string()
        )],
        "nothing reached the machine, so the effect is a known failure"
    );
    let (status, result) = h.call_row(&run_id).await;
    assert_eq!(status, "failed", "{result:#}");
    assert_eq!(result["code"], json!("COMPUTER_PERMISSION_REQUIRED"));
    assert_eq!(result["requires"], json!("permission_granted"));

    // The park carries the attention item on the event the desktop reads.
    let suspended = h.event_payloads(&run_id, "run.suspended").await;
    assert_eq!(suspended.len(), 1, "{suspended:#?}");
    let attention = &suspended[0]["attention"];
    assert_eq!(attention["code"], json!("COMPUTER_PERMISSION_REQUIRED"));
    assert_eq!(attention["permission"], json!("accessibility"));
    assert_eq!(attention["status"], json!("denied"));
    assert_eq!(attention["guide"]["action"], json!("open_settings"));
    assert_eq!(attention["guide"]["pane"], json!("Privacy_Accessibility"));
    assert_eq!(attention["resume_command"], json!("ResumeRun"));
    assert_eq!(suspended[0]["run_id"], json!(run_id));

    // A blocked Notification carries the same item to the person.
    let made = Notifier::new(h.pool.clone(), TENANT)
        .scan(1000)
        .await
        .expect("scan");
    assert!(made >= 1, "the notifier made no notification");
    let notifications: Vec<(String, String, Value)> = h
        .rows(
            "SELECT kind, user_id, ref FROM notifications WHERE tenant_id = $1 AND kind = 'blocked' AND ref ->> 'run_id' = $2",
            &run_id,
        )
        .await;
    assert_eq!(notifications.len(), 1, "{notifications:#?}");
    assert_eq!(notifications[0].0, "blocked");
    assert_eq!(notifications[0].1, USER);
    assert_eq!(notifications[0].2["attention"], *attention);

    // Only the person's ResumeRun leaves the state; the model has no way to.
    h.bridge.grant_all();
    let current = h.engine.store().load_run(&run).await.expect("run");
    let resumed = h
        .engine
        .resume(&run, current.generation)
        .await
        .expect("resume");
    assert_eq!(resumed.status, RunStatus::Running);
    // After the person put it right, the same action goes through.
    let again = h.turn(&run, "computer.click", click_args()).await;
    assert!(
        matches!(
            again,
            TurnOutcome::EffectUnsettled { .. } | TurnOutcome::Completed { .. }
        ),
        "{again:?}"
    );
    assert_eq!(h.bridge.performed(), 2);
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_takeover_ends_the_call_before_the_bridge_is_asked_and_a_stopped_run_sends_nothing() {
    let Some(h) = harness("computer_stop", true).await else {
        return;
    };
    // A person holds the computer.
    {
        let mut tx = h.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query("UPDATE computer_controls SET holder = 'user', generation = 2 WHERE tenant_id = $1 AND target_id = $2")
            .bind(TENANT)
            .bind(TARGET)
            .execute(&mut *tx)
            .await
            .expect("takeover");
        tx.commit().await.expect("commit");
    }
    let (run, _) = h.call("computer.click", click_args()).await;
    let (status, result) = h.call_row(&run.to_string()).await;
    assert_eq!(status, "failed", "{result:#}");
    assert_eq!(result["code"], json!("COMPUTER_TAKEOVER_ACTIVE"));
    assert_eq!(result["requires"], json!("handback"));
    assert_eq!(result["requires_new_observation"], json!(true));
    assert_eq!(h.bridge.performed(), 0);
    assert_eq!(
        h.effects(&run.to_string()).await,
        vec![(
            "computer.input.privileged".to_string(),
            "SETTLED_FAILED".to_string()
        )]
    );

    // A run that was paused sends nothing either: a person's stop is a terminal condition.
    let paused = h.new_run().await;
    let current = h.engine.store().load_run(&paused).await.expect("run");
    h.engine
        .suspend(&paused, current.generation, "api")
        .await
        .expect("pause");
    let host = h.state.host();
    let refused = host
        .execute(HostDispatch {
            tool_call_id: "tc_01J8Z3K6F1N8VQ2X5W9YADP00A".to_string(),
            tool: "computer.read".to_string(),
            version: 1,
            args: read_args(),
            effect_id: "eff_01J8Z3K6F1N8VQ2X5W9YADP00A".to_string(),
            dispatch_token: "token".to_string(),
            run_id: paused.to_string(),
            step_id: "stp_01J8Z3K6F1N8VQ2X5W9YADP00A".to_string(),
            workspace_id: WORKSPACE.to_string(),
            tenant_id: TENANT.to_string(),
            acting_user_id: Some(USER.to_string()),
            max_output_bytes: 65536,
            timeout_ms: 1000,
        })
        .await
        .expect("a typed failure");
    match refused {
        HostOutcome::Failure {
            error, retryable, ..
        } => {
            assert_eq!(error["code"], json!("COMPUTER_STOPPED_BY_USER"));
            assert!(!retryable);
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(h.bridge.performed(), 0);
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_bridge_nothing_is_simulated_and_an_unrecognised_application_is_refused() {
    let Some(h) = harness("computer_nobridge", false).await else {
        return;
    };
    // This build composes no bridge that reaches a machine: the call is refused before any input.
    let (run, _) = h.call("computer.click", click_args()).await;
    let (status, result) = h.call_row(&run.to_string()).await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(result["code"], json!("INTERNAL"));
    assert!(
        result["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("no native computer bridge"),
        "{result:#}"
    );
    assert_eq!(
        h.effects(&run.to_string()).await,
        vec![(
            "computer.input.privileged".to_string(),
            "SETTLED_FAILED".to_string()
        )]
    );
    h.finish().await;

    let Some(h) = harness("computer_unknown_app", true).await else {
        return;
    };
    // The application in front is one nobody vouched for: refused whatever the run was granted.
    h.bridge.set_foreground("com.example.stranger");
    let (run, _) = h
        .call(
            "computer.click",
            json!({ "application": "com.example.stranger", "x": 1, "y": 2, "button": "left" }),
        )
        .await;
    let (status, result) = h.call_row(&run.to_string()).await;
    assert_eq!(status, "rejected", "{result:#}");
    assert_eq!(result["code"], json!("POLICY_DENIED"), "{result:#}");
    assert_eq!(h.bridge.performed(), 0);
    assert!(h.bridge.actions().is_empty());
    h.finish().await;
}
