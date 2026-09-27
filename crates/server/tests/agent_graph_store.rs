//! AgentGraph store refusals, rollback, listing and replay paths (QA-001 coverage of RUN-002).
//!
//! These suites complement `tests/agents.rs`: they drive `AgentStore` against a real scratch
//! PostgreSQL database through the paths that suite leaves out — delegation refusals
//! (foreign workspace, mistyped ids, a capability invented by a parent that holds none),
//! handoff refusals and rollback, handoffs without a run, completion after the source or target
//! moved on, joins of orphan workers or workers without an edge, parent-run fencing on join, and
//! the read paths (`list_delegations`, `load_join`, `load_handoff`). Every refusal is checked to
//! write nothing.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use quansio_core::{CanonicalId, CausationId, CorrelationId, Generation, Prefix, UlidGenerator};
use quansio_events::{EventStore, RuntimeEvent};
use serde_json::json;
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::runtime::agents::{
    AgentKind, AgentStore, AgentThread, AgentThreadStatus, HandoffStatus, JoinRequest,
    MailboxCursor, NewAgentThread, NewDelegation, NewHandoff, NewMailboxItem,
};
use quansio_server::runtime::protocol_state::ProtocolState;
use quansio_server::runtime::state_machine::{
    NewRun, Run, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE_2: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGG2";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const USER_B: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const WORKSPACE_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const WORK_NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const CAP_A: &str = "cap_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const MISSING_RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const CAUSING_COMMAND: &str = "cmd_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";

struct Fixture {
    name: String,
    pool: PgPool,
    agents: AgentStore,
    agents_b: AgentStore,
    engine: RuntimeEngine,
}

fn identity(tenant: &str) -> RuntimeIdentity {
    RuntimeIdentity::system(
        tenant,
        "agent-graph-store-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    seed_tenant(&pool, TENANT_B, USER_B, WORKSPACE_B, WORK_NODE_B).await;
    // A second workspace in the same tenant, for cross-workspace refusals.
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'second')")
        .bind(WORKSPACE_2)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("insert second workspace");
    tx.commit().await.expect("commit");
    let engine = RuntimeEngine::new(pool.clone(), identity(TENANT)).expect("runtime engine");
    Some(Fixture {
        agents: AgentStore::new(pool.clone(), identity(TENANT)).expect("agent store"),
        agents_b: AgentStore::new(pool.clone(), identity(TENANT_B)).expect("agent store b"),
        engine,
        name,
        pool,
    })
}

fn node_id() -> CanonicalId {
    CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("work node id")
}

async fn create(agents: &AgentStore, kind: AgentKind, workspace: &str) -> AgentThread {
    agents
        .create_thread(NewAgentThread::new(workspace, kind))
        .await
        .expect("create thread")
}

async fn active(agents: &AgentStore, kind: AgentKind, workspace: &str) -> AgentThread {
    let created = create(agents, kind, workspace).await;
    agents
        .activate(&created.id, created.generation)
        .await
        .expect("activate")
}

async fn events(pool: &PgPool, tenant: &str) -> Vec<RuntimeEvent> {
    EventStore::new(pool.clone())
        .read_events_after(tenant, None, 10_000)
        .await
        .expect("read events")
}

/// Run a count with bound text parameters inside the tenant context (RLS is forced).
async fn count_of(pool: &PgPool, sql: &str, binds: &[String]) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let mut query = sqlx::query_scalar::<_, i64>(sql);
    for bind in binds {
        query = query.bind(bind.clone());
    }
    let value = query.fetch_one(&mut *tx).await.expect("count");
    tx.commit().await.expect("commit");
    value
}

/// Execute one statement inside the tenant context, as another holder of the row would.
async fn execute_in_tenant(pool: &PgPool, sql: &str, binds: &[String]) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(bind.clone());
    }
    let result = query.execute(&mut *tx).await.expect("execute");
    assert_eq!(result.rows_affected(), 1, "{sql} touches exactly one row");
    tx.commit().await.expect("commit");
}

async fn create_run(f: &Fixture, agent_thread_id: CanonicalId) -> Run {
    f.engine
        .create_run(NewRun::new(
            WORKSPACE,
            node_id(),
            agent_thread_id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("create run")
}

async fn running_run(f: &Fixture, agent_thread_id: CanonicalId) -> Run {
    let run = create_run(f, agent_thread_id).await;
    let run = f
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    f.engine
        .start(&run.id, run.generation)
        .await
        .expect("start")
}

async fn delegated_worker(f: &Fixture, parent: &AgentThread) -> AgentThread {
    let delegated = f
        .agents
        .delegate(
            &parent.id,
            NewDelegation::new(
                NewAgentThread::new(WORKSPACE, AgentKind::Worker).with_work_node(node_id()),
            ),
        )
        .await
        .expect("delegate worker");
    f.agents
        .activate(&delegated.child.id, delegated.child.generation)
        .await
        .expect("activate worker")
}

#[test]
fn mailbox_cursor_parses_empty_as_initial_and_refuses_a_non_decimal_cursor() {
    assert_eq!(
        MailboxCursor::parse(None).expect("none"),
        MailboxCursor::INITIAL
    );
    assert_eq!(
        MailboxCursor::parse(Some("")).expect("empty"),
        MailboxCursor::INITIAL
    );
    assert_eq!(
        MailboxCursor::parse(Some("42")).expect("decimal"),
        MailboxCursor::new(42)
    );
    assert_eq!(MailboxCursor::new(42).to_string(), "42");
    for raw in ["-1", "4x", "seq-3"] {
        assert!(
            matches!(
                MailboxCursor::parse(Some(raw)),
                Err(RuntimeError::InvalidArgument(message)) if message.contains(raw)
            ),
            "{raw:?} is not a cursor"
        );
    }
}

#[test]
fn handoff_status_round_trips_its_stored_form_and_refuses_an_unknown_value() {
    for status in [
        HandoffStatus::Pending,
        HandoffStatus::Completed,
        HandoffStatus::RolledBack,
    ] {
        assert_eq!(
            HandoffStatus::from_db_str(status.as_db_str()).expect("round trip"),
            status
        );
    }
    assert_eq!(HandoffStatus::RolledBack.as_db_str(), "ROLLED_BACK");
    assert!(matches!(
        HandoffStatus::from_db_str("ABANDONED"),
        Err(RuntimeError::UnknownState {
            entity: "agent_handoff",
            ..
        })
    ));
}

#[tokio::test]
async fn delegation_refuses_foreign_workspaces_mistyped_ids_and_capabilities_the_parent_never_held()
{
    let Some(f) = prepare("qa001_agent_deleg").await else {
        blocked_marker();
        return;
    };
    assert_eq!(f.agents.identity().tenant_id, TENANT);
    let parent = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let threads_before = count_of(&f.pool, "SELECT count(*) FROM agent_threads", &[]).await;
    let edges_before = count_of(&f.pool, "SELECT count(*) FROM agent_graph_edges", &[]).await;
    let events_before = events(&f.pool, TENANT).await.len();

    // A child in another workspace of the same tenant.
    let refused = f
        .agents
        .delegate(
            &parent.id,
            NewDelegation::new(NewAgentThread::new(WORKSPACE_2, AgentKind::Worker)),
        )
        .await
        .expect_err("a child in another workspace");
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains(WORKSPACE_2)),
        "{refused:?}"
    );

    // A parent id that is not an agent thread id.
    let refused = f
        .agents
        .delegate(
            &node_id(),
            NewDelegation::new(NewAgentThread::new(WORKSPACE, AgentKind::Worker)),
        )
        .await
        .expect_err("a work node is not a parent thread");
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("expected a")),
        "{refused:?}"
    );

    // A delegation capability that is not a capability projection id.
    let mut mistyped = NewDelegation::new(NewAgentThread::new(WORKSPACE, AgentKind::Worker));
    mistyped.delegation_capability_id = Some(node_id());
    let refused = f
        .agents
        .delegate(&parent.id, mistyped)
        .await
        .expect_err("a work node id is not a capability");
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("expected a")),
        "{refused:?}"
    );

    // The parent holds no capability, so the child cannot record one (capability only narrows).
    let invented = NewAgentThread::new(WORKSPACE, AgentKind::Worker).with_capability_projection(
        CanonicalId::parse_typed(CAP_A, Prefix::CapabilityProjection).expect("cap"),
    );
    let refused = f
        .agents
        .delegate(&parent.id, NewDelegation::new(invented))
        .await
        .expect_err("a parent without capability cannot grant one");
    assert!(
        matches!(&refused, RuntimeError::CapabilityWideningRejected { detail } if detail.contains("holds no delegation capability")),
        "{refused:?}"
    );

    assert_eq!(
        count_of(&f.pool, "SELECT count(*) FROM agent_threads", &[]).await,
        threads_before,
        "no refused delegation inserts a child"
    );
    assert_eq!(
        count_of(&f.pool, "SELECT count(*) FROM agent_graph_edges", &[]).await,
        edges_before
    );
    assert_eq!(events(&f.pool, TENANT).await.len(), events_before);

    // A delegation with an instruction delivers it to the child's mailbox in the same commit.
    let delegated = f
        .agents
        .delegate(
            &parent.id,
            NewDelegation::new(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
                .with_work_node(node_id())
                .with_instruction("summarise the ticket"),
        )
        .await
        .expect("delegate with instruction");
    assert_eq!(delegated.edge.work_node_id, Some(node_id()));
    let inbox = f
        .agents
        .poll(&delegated.child.id, MailboxCursor::INITIAL)
        .await
        .expect("child inbox");
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        inbox[0].payload,
        json!({ "instruction": "summarise the ticket" })
    );

    // Listing is scoped to the workspace of the child.
    let listed = f
        .agents
        .list_delegations(WORKSPACE)
        .await
        .expect("list delegations");
    assert_eq!(listed, vec![delegated.edge.clone()]);
    assert!(f
        .agents
        .list_delegations(WORKSPACE_2)
        .await
        .expect("list other workspace")
        .is_empty());
    assert!(
        f.agents_b
            .list_delegations(WORKSPACE)
            .await
            .expect("list from tenant b")
            .is_empty(),
        "another tenant sees none of this tenant's edges"
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn transition_refuses_states_owned_by_other_operations_and_cancel_thread_terminates() {
    let Some(f) = prepare("qa001_agent_transition").await else {
        blocked_marker();
        return;
    };
    let thread = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let events_before = events(&f.pool, TENANT).await.len();
    for to in [
        AgentThreadStatus::Joining,
        AgentThreadStatus::Joined,
        AgentThreadStatus::Provisioned,
    ] {
        let refused = f
            .agents
            .transition(&thread.id, thread.generation, to)
            .await
            .expect_err("owned by another operation");
        assert!(
            matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains(to.as_db_str())),
            "{refused:?}"
        );
    }
    assert_eq!(
        f.agents.load_thread(&thread.id).await.expect("load").status,
        AgentThreadStatus::Active
    );
    assert_eq!(events(&f.pool, TENANT).await.len(), events_before);

    // cancel_thread applies the teammate's any → TERMINATED rule.
    let cancelled = f
        .agents
        .cancel_thread(&thread.id, thread.generation)
        .await
        .expect("cancel teammate");
    assert_eq!(cancelled.status, AgentThreadStatus::Terminated);
    let last = events(&f.pool, TENANT).await.pop().expect("event");
    assert_eq!(last.event_type.to_string(), "agent.terminated");
    assert_eq!(last.aggregate_id, thread.id.to_string());
    assert_eq!(last.payload["from"], "ACTIVE");

    // A terminated thread takes no more mail.
    let refused = f
        .agents
        .deliver(&thread.id, NewMailboxItem::new("late"))
        .await
        .expect_err("terminated threads take no mail");
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { from, .. } if from == "TERMINATED"),
        "{refused:?}"
    );
    assert!(f
        .agents
        .poll(&thread.id, MailboxCursor::INITIAL)
        .await
        .expect("poll")
        .is_empty());

    // ensure_active surfaces a thread this tenant cannot see as NotFound, not as active.
    let foreign = create(&f.agents_b, AgentKind::Teammate, WORKSPACE_B).await;
    let refused = f
        .agents
        .ensure_active(foreign)
        .await
        .expect_err("invisible thread");
    assert!(
        matches!(
            refused,
            RuntimeError::NotFound {
                entity: "agent_thread",
                ..
            }
        ),
        "{refused:?}"
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn begin_handoff_refuses_invalid_targets_and_foreign_runs_and_writes_nothing() {
    let Some(f) = prepare("qa001_agent_handoff_refuse").await else {
        blocked_marker();
        return;
    };
    let source = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let other = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let foreign_workspace = create(&f.agents, AgentKind::Teammate, WORKSPACE_2).await;

    let consumed = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    f.agents
        .deliver(&consumed.id, NewMailboxItem::new("seen"))
        .await
        .expect("deliver");
    f.agents
        .advance_cursor(&consumed.id, consumed.generation, MailboxCursor::new(1))
        .await
        .expect("consume");

    let ended = create(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    f.agents
        .terminate(&ended.id, ended.generation)
        .await
        .expect("terminate");

    let others_run = create_run(&f, other.id).await;
    let missing_run = CanonicalId::parse_typed(MISSING_RUN, Prefix::Run).expect("run id");
    let events_before = events(&f.pool, TENANT).await.len();

    let attempt = |request: NewHandoff| {
        let agents = f.agents.clone();
        let source = source.clone();
        async move {
            agents
                .begin_handoff(&source.id, source.generation, request)
                .await
                .expect_err("refused handoff")
        }
    };

    let refused = attempt(NewHandoff::new(source.id)).await;
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("itself")),
        "{refused:?}"
    );
    let refused = attempt(NewHandoff::new(foreign_workspace.id)).await;
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("another workspace")),
        "{refused:?}"
    );
    let refused = attempt(NewHandoff::new(consumed.id)).await;
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("consumed mailbox")),
        "{refused:?}"
    );
    let refused = attempt(NewHandoff::new(ended.id)).await;
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { from, to, .. } if from == "TERMINATED" && to == "handoff target"),
        "{refused:?}"
    );
    let refused = attempt(NewHandoff::new(other.id).with_run(others_run.id)).await;
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("not owned by the source")),
        "{refused:?}"
    );
    let refused = attempt(NewHandoff::new(other.id).with_run(missing_run)).await;
    assert!(
        matches!(&refused, RuntimeError::NotFound { entity: "run", id, .. } if id == MISSING_RUN),
        "{refused:?}"
    );

    assert_eq!(
        f.agents
            .load_thread(&source.id)
            .await
            .expect("source")
            .status,
        AgentThreadStatus::Active,
        "every refusal leaves the source ACTIVE"
    );
    assert_eq!(
        count_of(&f.pool, "SELECT count(*) FROM agent_handoffs", &[]).await,
        0
    );
    assert_eq!(events(&f.pool, TENANT).await.len(), events_before);
    assert!(f
        .agents
        .pending_handoffs(WORKSPACE)
        .await
        .expect("pending")
        .is_empty());

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn rollback_handoff_restores_the_source_once_and_blocks_later_completion() {
    let Some(f) = prepare("qa001_agent_rollback").await else {
        blocked_marker();
        return;
    };
    let source = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let target = create(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let mut request = NewHandoff::new(target.id);
    request.work_node_id = Some(node_id());
    let handoff = f
        .agents
        .begin_handoff(&source.id, source.generation, request)
        .await
        .expect("begin handoff without a run");
    assert_eq!(handoff.status, HandoffStatus::Pending);
    assert_eq!(handoff.run_id, None);
    assert_eq!(handoff.run_generation, None);
    assert_eq!(handoff.work_node_id, Some(node_id()));
    assert_eq!(
        f.agents.load_handoff(&handoff.id).await.expect("load"),
        handoff
    );

    let rolled_back = f
        .agents
        .rollback_handoff(&handoff.id)
        .await
        .expect("rollback");
    assert_eq!(rolled_back.id, handoff.id);
    assert_eq!(rolled_back.status, HandoffStatus::RolledBack);
    let restored = f.agents.load_thread(&source.id).await.expect("source");
    assert_eq!(restored.status, AgentThreadStatus::Active);
    assert_eq!(restored.handoff_to_agent_thread_id, None);
    assert_eq!(restored.handoff_at, None);
    assert_eq!(
        f.agents
            .load_thread(&target.id)
            .await
            .expect("target")
            .status,
        AgentThreadStatus::Provisioned,
        "the target never received the work"
    );
    let last = events(&f.pool, TENANT).await.pop().expect("event");
    assert_eq!(last.event_type.to_string(), "agent.activated");
    assert_eq!(last.aggregate_id, source.id.to_string());
    assert_eq!(last.payload["rolled_back"], true);
    assert_eq!(last.payload["from"], "HANDING_OFF");
    assert_eq!(last.payload["handoff_id"], handoff.id.as_str());
    assert!(f
        .agents
        .pending_handoffs(WORKSPACE)
        .await
        .expect("pending")
        .is_empty());

    let events_before = events(&f.pool, TENANT).await.len();
    let again = f
        .agents
        .rollback_handoff(&handoff.id)
        .await
        .expect_err("rollback is once-only");
    assert!(
        matches!(&again, RuntimeError::HandoffNotRecoverable { status, .. } if status == "ROLLED_BACK"),
        "{again:?}"
    );
    let completion = f
        .agents
        .complete_handoff(&handoff.id)
        .await
        .expect_err("a rolled-back handoff never completes");
    assert!(
        matches!(&completion, RuntimeError::HandoffNotRecoverable { status, .. } if status == "ROLLED_BACK"),
        "{completion:?}"
    );
    assert_eq!(events(&f.pool, TENANT).await.len(), events_before);

    // Unknown handoff ids are NotFound on every path.
    let unknown = "ahf_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
    for refused in [
        f.agents.load_handoff(unknown).await.expect_err("load"),
        f.agents
            .rollback_handoff(unknown)
            .await
            .expect_err("rollback"),
        f.agents
            .complete_handoff(unknown)
            .await
            .map(|_| ())
            .expect_err("complete"),
    ] {
        assert!(
            matches!(&refused, RuntimeError::NotFound { entity: "agent_handoff", id, .. } if id == unknown),
            "{refused:?}"
        );
    }

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn completing_a_runless_handoff_moves_the_work_node_to_an_already_active_target() {
    let Some(f) = prepare("qa001_agent_runless").await else {
        blocked_marker();
        return;
    };
    let source = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let target = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let mut request = NewHandoff::new(target.id);
    request.work_node_id = Some(node_id());
    let handoff = f
        .agents
        .begin_handoff(&source.id, source.generation, request)
        .await
        .expect("begin");
    let activations_before = count_of(
        &f.pool,
        "SELECT count(*) FROM runtime_events WHERE type = 'agent.activated'",
        &[],
    )
    .await;
    let completed = f
        .agents
        .complete_handoff(&handoff.id)
        .await
        .expect("complete");
    assert!(!completed.duplicate);
    assert_eq!(completed.run_id, None);
    assert_eq!(completed.handoff.status, HandoffStatus::Completed);
    assert!(completed.handoff.completed_at.is_some());
    assert_eq!(completed.source.status, AgentThreadStatus::HandedOff);
    assert_eq!(completed.target.status, AgentThreadStatus::Active);
    assert_eq!(
        completed.target.mailbox_cursor, None,
        "a source that consumed nothing hands over no cursor"
    );
    assert_eq!(
        count_of(
            &f.pool,
            "SELECT count(*) FROM agent_threads WHERE status = 'ACTIVE' AND id = $1",
            &[target.id.to_string()],
        )
        .await,
        1
    );
    assert_eq!(
        count_of(
            &f.pool,
            "SELECT count(*) FROM work_nodes WHERE id = $1 AND owner_agent_thread_id = $2",
            &[WORK_NODE.to_string(), target.id.to_string()],
        )
        .await,
        1,
        "the work node moved to the target"
    );
    assert_eq!(
        count_of(
            &f.pool,
            "SELECT count(*) FROM runtime_events WHERE type = 'agent.activated'",
            &[],
        )
        .await,
        activations_before,
        "an already-active target is not activated again"
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn complete_handoff_refuses_a_moved_source_a_terminated_target_and_a_moved_run() {
    let Some(f) = prepare("qa001_agent_complete_refuse").await else {
        blocked_marker();
        return;
    };

    // The source left HANDING_OFF without the handoff record: neither completion nor rollback
    // may act on it.
    let source = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let target = create(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let handoff = f
        .agents
        .begin_handoff(&source.id, source.generation, NewHandoff::new(target.id))
        .await
        .expect("begin");
    f.agents
        .transition(&source.id, source.generation, AgentThreadStatus::HandedOff)
        .await
        .expect("source moved on");
    for refused in [
        f.agents
            .complete_handoff(&handoff.id)
            .await
            .map(|_| ())
            .expect_err("complete"),
        f.agents
            .rollback_handoff(&handoff.id)
            .await
            .map(|_| ())
            .expect_err("rollback"),
    ] {
        assert!(
            matches!(&refused, RuntimeError::HandoffNotRecoverable { status, .. } if status == "source HANDED_OFF"),
            "{refused:?}"
        );
    }
    assert_eq!(
        f.agents
            .load_handoff(&handoff.id)
            .await
            .expect("load")
            .status,
        HandoffStatus::Pending
    );
    assert_eq!(
        f.agents
            .load_thread(&target.id)
            .await
            .expect("target")
            .status,
        AgentThreadStatus::Provisioned
    );

    // The target terminated between begin and complete: completion is refused and the handoff
    // can still be rolled back.
    let source = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let target = create(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let handoff = f
        .agents
        .begin_handoff(&source.id, source.generation, NewHandoff::new(target.id))
        .await
        .expect("begin");
    f.agents
        .terminate(&target.id, target.generation)
        .await
        .expect("target ends");
    let refused = f
        .agents
        .complete_handoff(&handoff.id)
        .await
        .map(|_| ())
        .expect_err("a terminated target takes no work");
    assert!(
        matches!(&refused, RuntimeError::IllegalTransition { from, to, .. } if from == "TERMINATED" && to == "handoff target"),
        "{refused:?}"
    );
    assert_eq!(
        f.agents
            .load_thread(&source.id)
            .await
            .expect("source")
            .status,
        AgentThreadStatus::HandingOff
    );
    f.agents
        .rollback_handoff(&handoff.id)
        .await
        .expect("the handoff is still recoverable");

    // The run moved to a newer generation after the handoff was recorded: completion is fenced.
    let source = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let target = create(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let run = running_run(&f, source.id).await;
    let handoff = f
        .agents
        .begin_handoff(
            &source.id,
            source.generation,
            NewHandoff::new(target.id).with_run(run.id),
        )
        .await
        .expect("begin with run");
    assert_eq!(handoff.run_generation, Some(run.generation));
    execute_in_tenant(
        &f.pool,
        "UPDATE runs SET generation = 2 WHERE id = $1",
        &[run.id.to_string()],
    )
    .await;
    let refused = f
        .agents
        .complete_handoff(&handoff.id)
        .await
        .map(|_| ())
        .expect_err("the run moved on");
    assert!(
        matches!(
            refused,
            RuntimeError::FencedStaleGeneration {
                received: 1,
                current: 2
            }
        ),
        "{refused:?}"
    );
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .agent_thread_id,
        source.id,
        "a fenced completion leaves the run with the source"
    );
    f.agents
        .rollback_handoff(&handoff.id)
        .await
        .expect("roll back the fenced handoff");

    // The run was reassigned to another thread after the handoff was recorded.
    let source = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let target = create(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let bystander = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let run = running_run(&f, source.id).await;
    let handoff = f
        .agents
        .begin_handoff(
            &source.id,
            source.generation,
            NewHandoff::new(target.id).with_run(run.id),
        )
        .await
        .expect("begin with run");
    execute_in_tenant(
        &f.pool,
        "UPDATE runs SET agent_thread_id = $1 WHERE id = $2",
        &[bystander.id.to_string(), run.id.to_string()],
    )
    .await;
    let refused = f
        .agents
        .complete_handoff(&handoff.id)
        .await
        .map(|_| ())
        .expect_err("the run belongs to someone else now");
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("no longer owned")),
        "{refused:?}"
    );
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .agent_thread_id,
        bystander.id
    );
    assert_eq!(
        f.agents
            .load_handoff(&handoff.id)
            .await
            .expect("load")
            .status,
        HandoffStatus::Pending
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn join_worker_refuses_orphans_and_workers_without_a_delegation_edge() {
    let Some(f) = prepare("qa001_agent_join_orphan").await else {
        blocked_marker();
        return;
    };
    let orphan = active(&f.agents, AgentKind::Worker, WORKSPACE).await;
    let refused = f
        .agents
        .join_worker(JoinRequest::new(orphan.id, orphan.generation))
        .await
        .expect_err("a worker without a parent cannot join");
    assert!(
        matches!(&refused, RuntimeError::InvalidArgument(message) if message.contains("without a parent")),
        "{refused:?}"
    );
    assert_eq!(
        f.agents
            .load_thread(&orphan.id)
            .await
            .expect("orphan")
            .status,
        AgentThreadStatus::Active
    );

    // A worker that names a parent but was never delegated through an edge.
    let parent = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let mut edgeless = NewAgentThread::new(WORKSPACE, AgentKind::Worker);
    edgeless.parent_id = Some(parent.id);
    let edgeless = f.agents.create_thread(edgeless).await.expect("create");
    let edgeless = f
        .agents
        .activate(&edgeless.id, edgeless.generation)
        .await
        .expect("activate");
    let refused = f
        .agents
        .join_worker(JoinRequest::new(edgeless.id, edgeless.generation))
        .await
        .expect_err("no delegation edge");
    assert!(
        matches!(&refused, RuntimeError::NotFound { entity: "delegation", id, .. } if *id == edgeless.id.to_string()),
        "{refused:?}"
    );
    assert_eq!(
        f.agents
            .load_thread(&edgeless.id)
            .await
            .expect("edgeless")
            .status,
        AgentThreadStatus::Active,
        "the JOINING step rolled back with the refusal"
    );
    let refused = f
        .agents
        .load_join(&edgeless.id)
        .await
        .expect_err("no merge");
    assert!(
        matches!(
            refused,
            RuntimeError::NotFound {
                entity: "agent_join",
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(
        count_of(&f.pool, "SELECT count(*) FROM agent_joins", &[]).await,
        0
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn join_worker_fences_the_parent_run_and_leaves_a_running_parent_alone() {
    let Some(f) = prepare("qa001_agent_join_parent").await else {
        blocked_marker();
        return;
    };
    let parent = active(&f.agents, AgentKind::Teammate, WORKSPACE).await;
    let run = running_run(&f, parent.id).await;

    // A parent run that is not parked is not resumed, but the join still commits.
    let worker = delegated_worker(&f, &parent).await;
    let joined = f
        .agents
        .join_worker(
            JoinRequest::new(worker.id, worker.generation)
                .with_lineage(
                    vec!["evd_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string()],
                    Vec::new(),
                    json!({ "summary": "done" }),
                )
                .with_parent_run(run.id, run.generation),
        )
        .await
        .expect("join");
    assert!(!joined.parent_run_resumed);
    assert!(!joined.duplicate);
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Running
    );
    let merge = f.agents.load_join(&worker.id).await.expect("load join");
    assert_eq!(merge, joined.merge);
    assert_eq!(merge.parent_agent_thread_id, parent.id);
    assert_eq!(
        merge.evidence_ids,
        json!(["evd_01J8Z3K6F1N8VQ2X5W9Y0GGGGG"])
    );
    assert_eq!(merge.outcome, json!({ "summary": "done" }));

    // A parent run id that does not exist refuses the whole join.
    let missing = CanonicalId::parse_typed(MISSING_RUN, Prefix::Run).expect("run id");
    let worker = delegated_worker(&f, &parent).await;
    let refused = f
        .agents
        .join_worker(
            JoinRequest::new(worker.id, worker.generation)
                .with_parent_run(missing, Generation::INITIAL),
        )
        .await
        .expect_err("unknown parent run");
    assert!(
        matches!(refused, RuntimeError::NotFound { entity: "run", .. }),
        "{refused:?}"
    );
    assert_eq!(
        f.agents
            .load_thread(&worker.id)
            .await
            .expect("worker")
            .status,
        AgentThreadStatus::Active,
        "a refused release rolls back the join"
    );

    // The parent run moved to a newer generation: a join carrying the old one is fenced.
    execute_in_tenant(
        &f.pool,
        "UPDATE runs SET generation = 3 WHERE id = $1",
        &[run.id.to_string()],
    )
    .await;
    let refused = f
        .agents
        .join_worker(
            JoinRequest::new(worker.id, worker.generation).with_parent_run(run.id, run.generation),
        )
        .await
        .expect_err("stale parent run generation");
    assert!(
        matches!(
            refused,
            RuntimeError::FencedStaleGeneration {
                received: 1,
                current: 3
            }
        ),
        "{refused:?}"
    );
    assert!(matches!(
        f.agents.load_join(&worker.id).await,
        Err(RuntimeError::NotFound { .. })
    ));

    // A parked parent run without protocol state cannot be released, so the join is refused.
    f.engine
        .store()
        .transition_run(
            &run.id,
            Generation::new(3).expect("generation"),
            RunStatus::WaitingChild,
            None,
        )
        .await
        .expect("park the parent run");
    let refused = f
        .agents
        .join_worker(
            JoinRequest::new(worker.id, worker.generation)
                .with_parent_run(run.id, Generation::new(3).expect("generation")),
        )
        .await
        .expect_err("no protocol state to release");
    assert!(
        matches!(&refused, RuntimeError::StateConflict { entity: "run", id } if *id == run.id.to_string()),
        "{refused:?}"
    );
    assert_eq!(
        f.agents
            .load_thread(&worker.id)
            .await
            .expect("worker")
            .status,
        AgentThreadStatus::Active
    );

    // With the protocol state present the same join releases the run.
    let mut state = ProtocolState::new(run.id.to_string(), 3);
    state.child_agent_threads = vec![worker.id.to_string()];
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("store state");
    let joined = f
        .agents
        .join_worker(
            JoinRequest::new(worker.id, worker.generation)
                .with_parent_run(run.id, Generation::new(3).expect("generation")),
        )
        .await
        .expect("join releases the run");
    assert!(joined.parent_run_resumed);
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Running
    );

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn agent_events_carry_the_causation_of_the_identity_that_caused_them() {
    let Some(f) = prepare("qa001_agent_causation").await else {
        blocked_marker();
        return;
    };
    let causation: CausationId = CAUSING_COMMAND.parse().expect("causation id");
    let agents = AgentStore::new(
        f.agents.pool().clone(),
        identity(TENANT).with_causation_id(causation.clone()),
    )
    .expect("agent store");
    let thread = create(&agents, AgentKind::Teammate, WORKSPACE).await;
    let provisioned = events(&f.pool, TENANT)
        .await
        .into_iter()
        .find(|event| event.aggregate_id == thread.id.to_string())
        .expect("provisioned event");
    assert_eq!(
        provisioned.event_type.to_string(),
        "agent.thread_provisioned"
    );
    assert_eq!(provisioned.causation_id, Some(causation));

    // A store bound to a malformed tenant id is refused at construction.
    assert!(AgentStore::new(f.pool.clone(), identity("not-a-tenant")).is_err());

    drop_pool(&f.pool, &f.name).await;
}
