//! AgentThread lifecycle, mailbox, delegation, handoff and join tests (RUN-002).
//!
//! They drive the runtime's `agents` module against a real scratch PostgreSQL database
//! and assert DOMAIN.md §5.1 legality and policy, §6.3 capability narrowing, §9.2
//! `agent.*` events, mailbox exactly-once cursor resume across a pool drop, recoverable
//! and idempotent handoff, out-of-order fanout joins, generation fencing and tenant
//! isolation.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Generation, Prefix, UlidGenerator};
use quansio_events::{EventStore, RuntimeEvent};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::runtime::agents::{
    AgentDelegationPort, AgentKind, AgentStore, AgentThread, AgentThreadStatus,
    DelegationNarrowingCheck, HandoffStatus, JoinRequest, MailboxCursor, MailboxItem,
    NewAgentThread, NewDelegation, NewHandoff, NewMailboxItem,
};
use quansio_server::runtime::protocol_state::ProtocolState;
use quansio_server::runtime::state_machine::{
    Budget, DelegationRequest, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, Run,
    RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
    TurnOutcome,
};

mod common;
use common::{
    admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, scratch_url, seed_tenant,
};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const USER_B: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORKSPACE_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORK_NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const CAP_A: &str = "cap_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const CAP_B: &str = "cap_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const EVIDENCE: &str = "evd_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";

struct Fixture {
    name: String,
    url: String,
    pool: PgPool,
    agents: AgentStore,
    agents_b: AgentStore,
    engine: RuntimeEngine,
}

fn identity(tenant: &str) -> RuntimeIdentity {
    RuntimeIdentity::system(
        tenant,
        "agents-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

fn agent_store(pool: &PgPool, tenant: &str) -> AgentStore {
    AgentStore::new(pool.clone(), identity(tenant)).expect("agent store")
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    seed_tenant(&pool, TENANT_B, USER_B, WORKSPACE_B, WORK_NODE_B).await;
    let engine = RuntimeEngine::new(pool.clone(), identity(TENANT)).expect("runtime engine");
    Some(Fixture {
        url: scratch_url(&name),
        name,
        agents: agent_store(&pool, TENANT),
        agents_b: agent_store(&pool, TENANT_B),
        engine,
        pool,
    })
}

fn node_id() -> CanonicalId {
    CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("work node id")
}

fn cap(value: &str) -> CanonicalId {
    CanonicalId::parse_typed(value, Prefix::CapabilityProjection).expect("capability id")
}

async fn create_thread(
    agents: &AgentStore,
    kind: AgentKind,
    workspace: &str,
    parent: Option<CanonicalId>,
    capability: Option<CanonicalId>,
) -> AgentThread {
    let mut thread = NewAgentThread::new(workspace, kind).with_work_node(node_id());
    thread.parent_id = parent;
    thread.capability_projection_id = capability;
    agents.create_thread(thread).await.expect("create thread")
}

async fn active_thread(
    agents: &AgentStore,
    kind: AgentKind,
    workspace: &str,
    parent: Option<CanonicalId>,
    capability: Option<CanonicalId>,
) -> AgentThread {
    let created = create_thread(agents, kind, workspace, parent, capability).await;
    agents
        .activate(&created.id, created.generation)
        .await
        .expect("activate thread")
}

async fn events(pool: &PgPool, tenant: &str) -> Vec<RuntimeEvent> {
    EventStore::new(pool.clone())
        .read_events_after(tenant, None, 1000)
        .await
        .expect("read events")
}

async fn event_types(pool: &PgPool, tenant: &str) -> Vec<String> {
    events(pool, tenant)
        .await
        .into_iter()
        .map(|event| event.event_type.to_string())
        .collect()
}

/// Run a count with bound text parameters inside the tenant context (RLS is forced).
async fn count_of(pool: &PgPool, tenant: &str, sql: &str, binds: &[String]) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, tenant)
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

async fn run_to_running(fixture: &Fixture, agent_thread_id: CanonicalId) -> Run {
    let run = fixture
        .engine
        .create_run(
            NewRun::new(
                WORKSPACE,
                node_id(),
                agent_thread_id,
                RunTriggerKind::Manual,
            )
            .with_budget(Budget::new(8)),
        )
        .await
        .expect("create run");
    let run = fixture
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    fixture
        .engine
        .start(&run.id, run.generation)
        .await
        .expect("start")
}

#[tokio::test]
async fn lifecycle_transitions_for_teammate_and_worker_with_policy_negatives() {
    let Some(f) = prepare("run002_lifecycle").await else {
        blocked_marker();
        return;
    };

    // A persistent teammate walks every legal non-join transition.
    let teammate = create_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    assert_eq!(teammate.status, AgentThreadStatus::Provisioned);
    let teammate = f
        .agents
        .activate(&teammate.id, teammate.generation)
        .await
        .expect("activate");
    assert_eq!(teammate.status, AgentThreadStatus::Active);
    let teammate = f
        .agents
        .suspend(&teammate.id, teammate.generation, "budget")
        .await
        .expect("suspend");
    assert_eq!(teammate.status, AgentThreadStatus::Suspended);
    assert_eq!(teammate.suspended_reason.as_deref(), Some("budget"));
    let teammate = f
        .agents
        .activate(&teammate.id, teammate.generation)
        .await
        .expect("resume");
    assert_eq!(teammate.status, AgentThreadStatus::Active);
    assert!(teammate.suspended_reason.is_none());
    let teammate = f
        .agents
        .transition(
            &teammate.id,
            teammate.generation,
            AgentThreadStatus::HandingOff,
        )
        .await
        .expect("handing off");
    let teammate = f
        .agents
        .transition(
            &teammate.id,
            teammate.generation,
            AgentThreadStatus::HandedOff,
        )
        .await
        .expect("handed off");
    let teammate = f
        .agents
        .terminate(&teammate.id, teammate.generation)
        .await
        .expect("terminate");
    assert_eq!(teammate.status, AgentThreadStatus::Terminated);

    // A worker must JOIN before it can terminate normally and walks ACTIVE → JOINING → JOINED.
    let parent = active_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
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
    let worker = f
        .agents
        .activate(&delegated.child.id, delegated.child.generation)
        .await
        .expect("activate worker");
    let aggregate = worker.id.to_string();
    let before = count_of(
        &f.pool,
        TENANT,
        "SELECT count(*) FROM runtime_events WHERE aggregate_id = $1",
        std::slice::from_ref(&aggregate),
    )
    .await;
    let refused = f
        .agents
        .terminate(&worker.id, worker.generation)
        .await
        .expect_err("a worker must join before terminating");
    assert!(matches!(
        refused,
        RuntimeError::LifecyclePolicy {
            rule: quansio_server::runtime::agents::WORKER_MUST_JOIN_BEFORE_TERMINATE,
            ..
        }
    ));
    assert_eq!(
        f.agents
            .load_thread(&worker.id)
            .await
            .expect("worker")
            .status,
        AgentThreadStatus::Active
    );
    let after = count_of(
        &f.pool,
        TENANT,
        "SELECT count(*) FROM runtime_events WHERE aggregate_id = $1",
        std::slice::from_ref(&aggregate),
    )
    .await;
    assert_eq!(before, after, "a policy refusal writes nothing");

    let joined = f
        .agents
        .join_worker(JoinRequest::new(worker.id, worker.generation))
        .await
        .expect("join worker");
    assert_eq!(joined.child.status, AgentThreadStatus::Joined);
    let terminated = f
        .agents
        .terminate(&worker.id, worker.generation)
        .await
        .expect("terminate joined worker");
    assert_eq!(terminated.status, AgentThreadStatus::Terminated);

    // A persistent teammate never joins.
    let teammate_child = create_thread(
        &f.agents,
        AgentKind::Teammate,
        WORKSPACE,
        Some(parent.id),
        None,
    )
    .await;
    let teammate_child = f
        .agents
        .activate(&teammate_child.id, teammate_child.generation)
        .await
        .expect("activate teammate child");
    let refused = f
        .agents
        .join_worker(JoinRequest::new(
            teammate_child.id,
            teammate_child.generation,
        ))
        .await
        .expect_err("a teammate never joins");
    assert!(matches!(
        refused,
        RuntimeError::LifecyclePolicy {
            rule: quansio_server::runtime::agents::TEAMMATE_CANNOT_JOIN,
            ..
        }
    ));
    assert_eq!(
        f.agents
            .load_thread(&teammate_child.id)
            .await
            .expect("teammate child")
            .status,
        AgentThreadStatus::Active
    );

    // An illegal transition leaves state and the event log untouched.
    let fresh = create_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    let before_events = events(&f.pool, TENANT).await.len();
    let illegal = f
        .agents
        .transition(&fresh.id, fresh.generation, AgentThreadStatus::Suspended)
        .await
        .expect_err("PROVISIONED cannot suspend");
    assert!(matches!(illegal, RuntimeError::IllegalTransition { .. }));
    assert_eq!(
        f.agents.load_thread(&fresh.id).await.expect("fresh").status,
        AgentThreadStatus::Provisioned
    );
    assert_eq!(events(&f.pool, TENANT).await.len(), before_events);

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn capability_narrowing_accepts_only_narrowing_and_writes_nothing_on_widening() {
    let Some(f) = prepare("run002_narrow").await else {
        blocked_marker();
        return;
    };
    let parent = active_thread(
        &f.agents,
        AgentKind::Teammate,
        WORKSPACE,
        None,
        Some(cap(CAP_A)),
    )
    .await;

    // Accepted narrowing: the child records the parent's delegated capability id.
    let narrow_child = NewAgentThread::new(WORKSPACE, AgentKind::Worker)
        .with_work_node(node_id())
        .with_capability_projection(cap(CAP_A));
    let delegated = f
        .agents
        .delegate(&parent.id, NewDelegation::new(narrow_child))
        .await
        .expect("narrowing delegation");
    assert_eq!(delegated.child.capability_projection_id, Some(cap(CAP_A)));
    assert_eq!(delegated.edge.delegation_capability_id, Some(cap(CAP_A)));
    assert_eq!(delegated.edge.parent_agent_thread_id, parent.id);
    assert_eq!(delegated.edge.child_agent_thread_id, delegated.child.id);
    assert_eq!(
        delegated.child.parent_id,
        Some(parent.id),
        "the child's lineage records the parent"
    );

    let threads_before = count_of(&f.pool, TENANT, "SELECT count(*) FROM agent_threads", &[]).await;
    let edges_before = count_of(
        &f.pool,
        TENANT,
        "SELECT count(*) FROM agent_graph_edges",
        &[],
    )
    .await;
    let events_before = events(&f.pool, TENANT).await.len();

    // The structural rule refuses a child that claims a different capability.
    let widening_child = NewAgentThread::new(WORKSPACE, AgentKind::Worker)
        .with_work_node(node_id())
        .with_capability_projection(cap(CAP_B));
    let refused = f
        .agents
        .delegate(&parent.id, NewDelegation::new(widening_child))
        .await
        .expect_err("a different capability is not provably a narrowing");
    assert!(matches!(
        refused,
        RuntimeError::CapabilityWideningRejected { .. }
    ));

    // A check that reports widening is rejected before anything is written (RUN-005 seam).
    struct WideningCheck;
    impl DelegationNarrowingCheck for WideningCheck {
        fn check(
            &self,
            _parent: &AgentThread,
            _child: &NewAgentThread,
            _delegated: Option<&CanonicalId>,
        ) -> Result<(), RuntimeError> {
            Err(RuntimeError::CapabilityWideningRejected {
                detail: "the algebra says this widens".to_string(),
            })
        }
    }
    let seam_child = NewAgentThread::new(WORKSPACE, AgentKind::Worker)
        .with_work_node(node_id())
        .with_capability_projection(cap(CAP_A));
    let refused = f
        .agents
        .delegate_with(
            Arc::new(WideningCheck),
            &parent.id,
            NewDelegation::new(seam_child),
        )
        .await
        .expect_err("the widening seam is rejected");
    assert!(matches!(
        refused,
        RuntimeError::CapabilityWideningRejected { .. }
    ));

    assert_eq!(
        count_of(&f.pool, TENANT, "SELECT count(*) FROM agent_threads", &[]).await,
        threads_before
    );
    assert_eq!(
        count_of(
            &f.pool,
            TENANT,
            "SELECT count(*) FROM agent_graph_edges",
            &[]
        )
        .await,
        edges_before
    );
    assert_eq!(events(&f.pool, TENANT).await.len(), events_before);

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn handoff_is_recoverable_across_a_pool_drop_and_idempotent() {
    let Some(f) = prepare("run002_handoff").await else {
        blocked_marker();
        return;
    };
    let source = active_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    let target = create_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    let run = run_to_running(&f, source.id).await;

    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE work_nodes SET owner_agent_thread_id = $1 WHERE id = $2")
        .bind(source.id.to_string())
        .bind(WORK_NODE)
        .execute(&mut *tx)
        .await
        .expect("own work node");
    tx.commit().await.expect("commit");

    for message in ["m1", "m2", "m3"] {
        f.agents
            .deliver(&source.id, NewMailboxItem::new(message))
            .await
            .expect("deliver");
    }
    f.agents
        .advance_cursor(&source.id, source.generation, MailboxCursor::new(1))
        .await
        .expect("advance cursor");

    let handoff = f
        .agents
        .begin_handoff(
            &source.id,
            source.generation,
            NewHandoff::new(target.id)
                .with_run(run.id)
                .with_evidence(vec![EVIDENCE.to_string()])
                .with_conversation_ref("thr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE"),
        )
        .await
        .expect("begin handoff");
    assert_eq!(handoff.status, HandoffStatus::Pending);
    assert_eq!(
        f.agents
            .load_thread(&source.id)
            .await
            .expect("source")
            .status,
        AgentThreadStatus::HandingOff
    );
    assert_eq!(handoff.mailbox_cursor.as_deref(), Some("1"));
    assert_eq!(handoff.evidence_ids, serde_json::json!([EVIDENCE]));

    // Crash between begin and complete: drop the pool, reconnect to the same database.
    f.pool.close().await;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&f.url)
        .await
        .expect("reconnect");
    let agents = agent_store(&pool, TENANT);
    let engine = RuntimeEngine::new(pool.clone(), identity(TENANT)).expect("engine after restart");

    let pending = agents
        .pending_handoffs(WORKSPACE)
        .await
        .expect("pending handoffs");
    assert_eq!(pending.len(), 1, "the durable intent survives the crash");
    assert_eq!(pending[0].id, handoff.id);

    let completed = agents
        .complete_handoff(&handoff.id)
        .await
        .expect("complete recovered handoff");
    assert!(!completed.duplicate);
    assert_eq!(completed.source.status, AgentThreadStatus::HandedOff);
    assert_eq!(completed.target.status, AgentThreadStatus::Active);
    assert_eq!(completed.run_id, Some(run.id));
    assert_eq!(
        completed.target.mailbox_cursor.as_deref(),
        Some("1"),
        "the mailbox cursor moves with the work"
    );
    let moved = agents
        .poll(&completed.target.id, MailboxCursor::new(1))
        .await
        .expect("target mailbox");
    assert_eq!(
        moved.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(
        agents
            .load_thread(&source.id)
            .await
            .expect("source")
            .handoff_to_agent_thread_id,
        Some(target.id)
    );

    let run_after = engine
        .store()
        .load_run(&run.id)
        .await
        .expect("run after handoff");
    assert_eq!(run_after.agent_thread_id, completed.target.id);
    assert_eq!(
        count_of(
            &pool,
            TENANT,
            "SELECT count(*) FROM work_nodes WHERE owner_agent_thread_id = $1 AND id = $2",
            &[completed.target.id.to_string(), WORK_NODE.to_string()],
        )
        .await,
        1,
        "the work node moved to the target"
    );
    assert_eq!(
        count_of(
            &pool,
            TENANT,
            "SELECT count(*) FROM runtime_events WHERE type = 'agent.handoff_completed'",
            &[],
        )
        .await,
        1
    );

    // Completing twice is idempotent: no duplicate event, no duplicate lineage.
    let again = agents
        .complete_handoff(&handoff.id)
        .await
        .expect("idempotent completion");
    assert!(again.duplicate);
    assert_eq!(again.handoff.status, HandoffStatus::Completed);
    assert_eq!(
        count_of(
            &pool,
            TENANT,
            "SELECT count(*) FROM agent_handoffs WHERE id = $1",
            &[handoff.id.clone()],
        )
        .await,
        1
    );
    assert_eq!(
        count_of(
            &pool,
            TENANT,
            "SELECT count(*) FROM runtime_events WHERE type = 'agent.handoff_completed'",
            &[],
        )
        .await,
        1
    );

    drop_pool(&pool, &f.name).await;
}

#[tokio::test]
async fn fanout_join_out_of_order_parks_parent_until_the_last_child_and_is_idempotent() {
    let Some(f) = prepare("run002_fanout").await else {
        blocked_marker();
        return;
    };
    let parent = active_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    let run = run_to_running(&f, parent.id).await;

    let mut children = Vec::new();
    for _ in 0..2 {
        let child = NewAgentThread::new(WORKSPACE, AgentKind::Worker).with_work_node(node_id());
        let delegated = f
            .agents
            .delegate(&parent.id, NewDelegation::new(child))
            .await
            .expect("delegate child");
        let child = f
            .agents
            .activate(&delegated.child.id, delegated.child.generation)
            .await
            .expect("activate child");
        children.push(child);
    }

    let mut state = ProtocolState::new(run.id.to_string(), run.generation.get() as i64);
    state.child_agent_threads = children.iter().map(|c| c.id.to_string()).collect();
    f.engine
        .store()
        .store_protocol_state(&state)
        .await
        .expect("park state");
    f.engine
        .store()
        .transition_run(&run.id, run.generation, RunStatus::WaitingChild, None)
        .await
        .expect("park parent");

    // Out of order: the second child joins first and the run stays parked.
    let first = children[1].clone();
    let joined = f
        .agents
        .join_worker(
            JoinRequest::new(first.id, first.generation)
                .with_lineage(
                    vec![EVIDENCE.to_string()],
                    vec!["art_01J8Z3K6F1N8VQ2X5W9Y0EEEEE".to_string()],
                    serde_json::json!({ "summary": "child two" }),
                )
                .with_parent_run(run.id, run.generation),
        )
        .await
        .expect("join second child");
    assert!(!joined.parent_run_resumed);
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::WaitingChild
    );

    // A duplicate join of the same child does not duplicate the merge.
    let duplicate = f
        .agents
        .join_worker(JoinRequest::new(first.id, first.generation))
        .await
        .expect("duplicate join");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.merge.child_status, "SUCCEEDED");

    let second = children[0].clone();
    let joined = f
        .agents
        .join_worker(
            JoinRequest::new(second.id, second.generation).with_parent_run(run.id, run.generation),
        )
        .await
        .expect("join first child");
    assert!(joined.parent_run_resumed, "the last join resumes the run");
    assert_eq!(
        f.engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Running
    );
    let resumed_state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("state")
        .expect("state present");
    assert!(resumed_state.child_agent_threads.is_empty());

    let _ = f
        .agents
        .join_worker(JoinRequest::new(second.id, second.generation))
        .await
        .expect("idempotent second join");
    assert_eq!(
        count_of(
            &f.pool,
            TENANT,
            "SELECT count(*) FROM agent_joins WHERE child_agent_thread_id = $1",
            &[second.id.to_string()],
        )
        .await,
        1
    );
    let joined_events = event_types(&f.pool, TENANT)
        .await
        .into_iter()
        .filter(|event| event == "agent.joined")
        .count();
    assert_eq!(
        joined_events, 2,
        "one agent.joined per worker, never a replay"
    );

    // A teammate is refused by the join policy.
    let teammate = active_thread(
        &f.agents,
        AgentKind::Teammate,
        WORKSPACE,
        Some(parent.id),
        None,
    )
    .await;
    let refused = f
        .agents
        .join_worker(JoinRequest::new(teammate.id, teammate.generation))
        .await
        .expect_err("a teammate never joins");
    assert!(matches!(refused, RuntimeError::LifecyclePolicy { .. }));

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn mailbox_cursor_resumes_exactly_once_and_isolates_tenants_and_threads() {
    let Some(f) = prepare("run002_mailbox").await else {
        blocked_marker();
        return;
    };
    let thread = active_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    let other = active_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;

    for message in ["m1", "m2", "m3"] {
        f.agents
            .deliver(
                &thread.id,
                NewMailboxItem::new(message).with_payload(serde_json::json!({ "body": message })),
            )
            .await
            .expect("deliver");
    }
    // Redelivering the same message id is idempotent.
    let redelivered = f
        .agents
        .deliver(&thread.id, NewMailboxItem::new("m1"))
        .await
        .expect("redeliver");
    assert_eq!(redelivered.seq, 1);
    assert_eq!(
        count_of(
            &f.pool,
            TENANT,
            "SELECT count(*) FROM agent_mailbox_items WHERE message_id = 'm1'",
            &[],
        )
        .await,
        1
    );

    let polled = f
        .agents
        .poll(&thread.id, MailboxCursor::INITIAL)
        .await
        .expect("poll");
    assert_eq!(
        polled.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    f.agents
        .advance_cursor(&thread.id, thread.generation, MailboxCursor::new(1))
        .await
        .expect("advance");
    let refused = f
        .agents
        .advance_cursor(&thread.id, thread.generation, MailboxCursor::new(99))
        .await
        .expect_err("cursor past the last delivery is refused");
    assert!(matches!(refused, RuntimeError::InvalidArgument(_)));

    // Crash after acknowledging item 1 but before acknowledging the rest.
    f.pool.close().await;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&f.url)
        .await
        .expect("reconnect");
    let agents = agent_store(&pool, TENANT);

    let resumed: Vec<MailboxItem> = agents
        .poll(&thread.id, MailboxCursor::new(1))
        .await
        .expect("resumed poll");
    assert_eq!(
        resumed.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![2, 3],
        "a re-poll returns exactly the unacknowledged items"
    );
    let repeated = agents
        .poll(&thread.id, MailboxCursor::new(1))
        .await
        .expect("re-poll");
    assert_eq!(repeated.len(), resumed.len(), "poll never duplicates");
    agents
        .advance_cursor(&thread.id, thread.generation, MailboxCursor::new(3))
        .await
        .expect("advance to three");
    assert!(agents
        .poll(&thread.id, MailboxCursor::new(3))
        .await
        .expect("empty poll")
        .is_empty());
    let before_events = events(&pool, TENANT).await.len();
    let unchanged = agents
        .advance_cursor(&thread.id, thread.generation, MailboxCursor::new(3))
        .await
        .expect("idempotent advance");
    assert_eq!(unchanged.mailbox_cursor.as_deref(), Some("3"));
    assert_eq!(events(&pool, TENANT).await.len(), before_events);

    // Isolation across agent threads and tenants.
    let agents_b = agent_store(&pool, TENANT_B);
    assert!(agents
        .poll(&other.id, MailboxCursor::INITIAL)
        .await
        .expect("other thread")
        .is_empty());
    assert!(matches!(
        agents_b.load_thread(&thread.id).await,
        Err(RuntimeError::NotFound { .. })
    ));
    assert!(matches!(
        agents_b.deliver(&thread.id, NewMailboxItem::new("x")).await,
        Err(RuntimeError::NotFound { .. })
    ));

    drop_pool(&pool, &f.name).await;
}

#[tokio::test]
async fn generation_fencing_and_tenant_isolation_for_agent_mutations() {
    let Some(f) = prepare("run002_fencing").await else {
        blocked_marker();
        return;
    };
    let thread = active_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    let target = create_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;

    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE agent_threads SET generation = 5 WHERE id = $1 AND tenant_id = $2")
        .bind(thread.id.to_string())
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("bump generation");
    tx.commit().await.expect("commit");

    let before_events = events(&f.pool, TENANT).await.len();
    let stale = f
        .agents
        .suspend(&thread.id, Generation::new(1).expect("generation"), "stale")
        .await
        .expect_err("stale generation is fenced");
    assert!(matches!(stale, RuntimeError::FencedStaleGeneration { .. }));
    let stale_handoff = f
        .agents
        .begin_handoff(
            &thread.id,
            Generation::new(1).expect("generation"),
            NewHandoff::new(target.id),
        )
        .await
        .expect_err("stale handoff is fenced");
    assert!(matches!(
        stale_handoff,
        RuntimeError::FencedStaleGeneration { .. }
    ));
    assert_eq!(
        f.agents
            .load_thread(&thread.id)
            .await
            .expect("thread")
            .status,
        AgentThreadStatus::Active
    );
    assert_eq!(events(&f.pool, TENANT).await.len(), before_events);

    // Tenant B cannot see or mutate tenant A's agent threads.
    assert!(matches!(
        f.agents_b.load_thread(&thread.id).await,
        Err(RuntimeError::NotFound { .. })
    ));
    assert!(f
        .agents_b
        .list_threads(WORKSPACE)
        .await
        .expect("tenant b threads")
        .is_empty());
    assert!(matches!(
        f.agents_b
            .suspend(&thread.id, Generation::new(5).expect("generation"), "nope")
            .await,
        Err(RuntimeError::NotFound { .. })
    ));

    drop_pool(&f.pool, &f.name).await;
}

// ------------------------------------------------------------------ turn-loop wiring

struct ScriptedModel {
    proposals: Mutex<VecDeque<ModelProposal>>,
    calls: AtomicUsize,
}

impl ScriptedModel {
    fn new(proposals: Vec<ModelProposal>) -> Arc<Self> {
        Arc::new(Self {
            proposals: Mutex::new(proposals.into()),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl ModelProposalSource for ScriptedModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .proposals
            .lock()
            .expect("proposals")
            .pop_front()
            .unwrap_or_default())
    }
}

#[tokio::test]
async fn delegating_proposal_creates_a_real_child_and_parks_the_run() {
    let Some(f) = prepare("run002_port").await else {
        blocked_marker();
        return;
    };
    let parent = active_thread(&f.agents, AgentKind::Teammate, WORKSPACE, None, None).await;
    let run = run_to_running(&f, parent.id).await;

    let run_identity = identity(TENANT);
    let port = AgentDelegationPort::with_structural_check(f.pool.clone(), run_identity.clone())
        .expect("delegation port");
    let engine = RuntimeEngine::new(f.pool.clone(), run_identity)
        .expect("engine")
        .with_delegation(Arc::new(port))
        .with_model_source(ScriptedModel::new(vec![ModelProposal {
            delegate_requests: vec![DelegationRequest {
                request_id: "req_1".to_string(),
                work_node_id: None,
                instruction: "summarise the report".to_string(),
            }],
            ..ModelProposal::default()
        }]));

    let outcome = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Message),
        )
        .await
        .expect("turn with delegation");
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("a delegation must park the run, got {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingChild);

    let child_id = CanonicalId::parse_typed(&wait_key, Prefix::AgentThread).expect("child id");
    let child = f.agents.load_thread(&child_id).await.expect("child thread");
    assert_eq!(child.agent_kind, AgentKind::Worker);
    assert_eq!(child.parent_id, Some(parent.id));
    assert_eq!(child.work_node_id, Some(node_id()));
    assert_eq!(child.status, AgentThreadStatus::Active);
    let mailbox = f
        .agents
        .poll(&child.id, MailboxCursor::INITIAL)
        .await
        .expect("child mailbox");
    assert_eq!(mailbox.len(), 1);
    assert_eq!(
        mailbox[0].payload,
        serde_json::json!({ "instruction": "summarise the report" })
    );
    let types = event_types(&f.pool, TENANT).await;
    assert!(types.iter().any(|event| event == "agent.delegated"));
    assert!(types
        .iter()
        .any(|event| event == "agent.thread_provisioned"));
    assert!(types.iter().any(|event| event == "agent.activated"));
    assert!(types.iter().any(|event| event == "run.waiting"));
    assert!(types.iter().any(|event| event == "agent.mailbox_delivered"));

    drop_pool(&f.pool, &f.name).await;
}
