//! GraphTransaction: one transaction, one revision compare-and-set, every event (CORE-005).
//!
//! These run against real PostgreSQL: the atomicity of graph state plus RuntimeEvent plus
//! outbox, the single revision compare-and-set, and the exact event payloads are database
//! properties and are proven at that boundary.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, SEED_NODE, TENANT, WORKSPACE};
use quansio_core::{
    CanonicalId, CausationId, CommandId, CorrelationId, EventId, Prefix, Revision, TypedId,
    UlidGenerator,
};
use quansio_events::{ActorKind, EventStore, RuntimeEvent};
use quansio_graph::transaction::{GraphTransaction, GraphTransactionError, TransactionContext};
use quansio_graph::{
    AgentKind, AgentThreadStatus, GraphBatch, GraphChange, GraphStore, NewAgentThread, NewRun,
    NewTurn, NewWorkEdge, NewWorkNode, RunStatus, RunTriggerKind, WorkEdgeKind, WorkNodeKind,
    WorkNodeStatus,
};
use sqlx::PgPool;

/// The agent thread that appears as the event actor.
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

fn actor() -> serde_json::Value {
    serde_json::json!({"kind": "user", "id": "usr_seed"})
}

fn new_node(title: &str) -> NewWorkNode {
    NewWorkNode::new(WORKSPACE, WorkNodeKind::Task, title, actor())
}

fn node_id(value: &str) -> CanonicalId {
    CanonicalId::parse_typed(value, Prefix::WorkNode).expect("work node id")
}

fn seed_node() -> CanonicalId {
    node_id(SEED_NODE)
}

fn correlation() -> CorrelationId {
    CorrelationId::generate(&mut UlidGenerator::new())
}

fn causation() -> CausationId {
    CausationId::Event(
        EventId::parse("evt_01J8Z3K6F1N8VQ2X5W9Y0CCCCC").expect("causation event id"),
    )
}

fn command() -> CommandId {
    CommandId::parse("cmd_01J8Z3K6F1N8VQ2X5W9Y0CCCCC").expect("command id")
}

/// A transaction whose events carry an agent actor, a command and a direct cause.
fn transaction(store: GraphStore, pool: &PgPool, correlation: CorrelationId) -> GraphTransaction {
    GraphTransaction::new(
        store,
        EventStore::new(pool.clone()),
        TransactionContext::agent(AGENT, correlation)
            .with_command_id(command())
            .with_causation_id(causation()),
    )
}

/// Count rows in a tenant table with the tenant context set, so row-level security applies
/// exactly as it does for the store (a context-free count would always read zero).
async fn tenant_count(pool: &PgPool, sql: &str) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let count = sqlx::query_scalar(sql)
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    count
}

/// All committed events of the tenant, in sequence order.
async fn events(store: &EventStore) -> Vec<RuntimeEvent> {
    store
        .read_events_after(TENANT, None, 1000)
        .await
        .expect("read events")
}

fn find<'a>(events: &'a [RuntimeEvent], event_type: &str) -> &'a RuntimeEvent {
    events
        .iter()
        .find(|event| event.event_type.to_string() == event_type)
        .unwrap_or_else(|| panic!("no {event_type} event in {events:#?}"))
}

async fn setup(prefix: &str) -> Option<(String, PgPool, GraphStore)> {
    let (name, pool) = prepare(prefix).await?;
    let store = GraphStore::new(pool.clone(), TENANT).expect("store");
    Some((name, pool, store))
}

#[tokio::test]
async fn a_committed_transaction_writes_state_and_events_with_contiguous_sequences() {
    let Some((name, pool, store)) = setup("tx_commit").await else {
        blocked_marker();
        return;
    };
    let events_store = EventStore::new(pool.clone());
    let correlation = correlation();

    // Fixtures the batch refers to (a batch cannot name an id it is about to generate).
    let node_a = store.create_node(new_node("node a")).await.expect("node a");
    let node_b = store.create_node(new_node("node b")).await.expect("node b");
    let thread = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("thread");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE,
            node_a.id,
            thread.id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let batch = GraphBatch::new(WORKSPACE, base)
        .with(GraphChange::create_work_node(new_node(
            "created by the batch",
        )))
        .with(GraphChange::create_work_edge(NewWorkEdge::new(
            WORKSPACE,
            node_b.id,
            seed_node(),
            WorkEdgeKind::DependsOn,
        )))
        .with(GraphChange::TransitionWorkNode {
            node_id: node_a.id,
            expected: node_a.revision,
            to: WorkNodeStatus::Ready,
        })
        .with(GraphChange::create_agent_thread(NewAgentThread::new(
            WORKSPACE,
            AgentKind::Worker,
        )))
        .with(GraphChange::TransitionAgentThread {
            thread_id: thread.id,
            to: AgentThreadStatus::Active,
        })
        .with(GraphChange::create_run(NewRun::new(
            WORKSPACE,
            node_b.id,
            thread.id,
            RunTriggerKind::Message,
        )))
        .with(GraphChange::TransitionRun {
            run_id: run.id,
            to: RunStatus::Queued,
            terminal_reason: None,
        });

    let outcome = transaction(store, &pool, correlation)
        .apply(batch)
        .await
        .expect("transaction commits");
    assert_eq!(outcome.applied, 7);
    assert_eq!(outcome.revision, Revision::new(base.get() + 1));
    assert_eq!(outcome.event_ids.len(), 7, "one event per change");

    // State: every change is visible.
    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(reader.list_nodes(WORKSPACE).await.expect("nodes").len(), 4);
    assert_eq!(reader.list_edges(WORKSPACE).await.expect("edges").len(), 1);
    assert_eq!(
        reader
            .list_agent_threads(WORKSPACE)
            .await
            .expect("threads")
            .len(),
        2
    );
    assert_eq!(reader.list_runs(WORKSPACE).await.expect("runs").len(), 2);
    assert_eq!(
        reader.get_node(&node_a.id).await.expect("node a").status,
        WorkNodeStatus::Ready
    );
    assert_eq!(
        reader.get_run(&run.id).await.expect("run").status,
        RunStatus::Queued
    );
    assert_eq!(
        reader.graph_revision(WORKSPACE).await.expect("head"),
        outcome.revision,
        "the batch advances the graph revision exactly once"
    );

    // Events: one per change, in batch order, with contiguous tenant sequences.
    let committed = events(&events_store).await;
    assert_eq!(committed.len(), 7);
    let types: Vec<String> = committed
        .iter()
        .map(|event| event.event_type.to_string())
        .collect();
    assert_eq!(
        types,
        vec![
            "work.node_created",
            "work.edge_added",
            "work.node_status_changed",
            "agent.thread_provisioned",
            "agent.activated",
            "run.created",
            "run.queued",
        ]
    );
    let sequences: Vec<i64> = committed.iter().map(|event| event.sequence.get()).collect();
    assert_eq!(sequences, (1..=7).collect::<Vec<i64>>());
    let ids: Vec<EventId> = committed.iter().map(|event| event.event_id).collect();
    assert_eq!(
        ids, outcome.event_ids,
        "the outcome names the committed events"
    );

    // Every event has its outbox row, so the projection cannot diverge from the stream.
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        7
    );
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM event_outbox").await,
        7
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn events_carry_aggregate_identity_and_causal_metadata() {
    let Some((name, pool, store)) = setup("tx_payload").await else {
        blocked_marker();
        return;
    };
    let events_store = EventStore::new(pool.clone());
    let correlation = correlation();
    let causation = causation();

    let thread = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("thread");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE,
            seed_node(),
            thread.id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let base = store.graph_revision(WORKSPACE).await.expect("head");
    let batch = GraphBatch::new(WORKSPACE, base)
        .with(GraphChange::create_run(NewRun::new(
            WORKSPACE,
            seed_node(),
            thread.id,
            RunTriggerKind::Routine,
        )))
        .with(GraphChange::TransitionRun {
            run_id: run.id,
            to: RunStatus::Queued,
            terminal_reason: None,
        });

    let outcome = transaction(store, &pool, correlation)
        .apply(batch)
        .await
        .expect("transaction commits");
    let committed = events(&events_store).await;
    assert_eq!(committed.len(), 2);

    // `run.created` names the created run at version 1 and is attributed to the actor,
    // the command and the direct cause the transaction carried.
    let created = find(&committed, "run.created");
    assert_eq!(created.aggregate_type, "run");
    assert_eq!(created.aggregate_version, 1);
    assert_eq!(created.workspace_id.as_deref(), Some(WORKSPACE));
    assert_eq!(created.correlation_id, correlation);
    assert_eq!(created.causation_id.clone(), Some(causation.clone()));
    assert_eq!(created.command_id, Some(command()));
    assert_eq!(created.actor.kind, ActorKind::Agent);
    assert_eq!(created.actor.id, AGENT);
    assert_eq!(created.generation, Some(run.generation));
    assert_eq!(created.payload["trigger_kind"], "routine");
    assert_ne!(
        created.aggregate_id,
        run.id.to_string(),
        "the new run has its own identity"
    );

    // `run.queued` is the first event of the pre-existing run: version 1 for that
    // aggregate, with the exact `from`/`to` states.
    let queued = find(&committed, "run.queued");
    assert_eq!(queued.aggregate_type, "run");
    assert_eq!(queued.aggregate_id, run.id.to_string());
    assert_eq!(queued.aggregate_version, 1);
    assert_eq!(queued.correlation_id, correlation);
    assert_eq!(queued.causation_id.clone(), Some(causation.clone()));
    assert_eq!(queued.actor.id, AGENT);
    assert_eq!(queued.generation, Some(run.generation));
    assert_eq!(queued.payload["from"], "CREATED");
    assert_eq!(queued.payload["to"], "QUEUED");
    assert_eq!(queued.workspace_id.as_deref(), Some(WORKSPACE));

    // The event identities are durable and unique per event.
    assert_eq!(outcome.event_ids.len(), 2);
    assert_ne!(outcome.event_ids[0], outcome.event_ids[1]);

    // Two further transitions of the same run in one transaction advance that
    // aggregate's version monotonically: 2 then 3.
    let base = GraphStore::new(pool.clone(), TENANT)
        .expect("store")
        .graph_revision(WORKSPACE)
        .await
        .expect("head");
    let batch = GraphBatch::new(WORKSPACE, base)
        .with(GraphChange::TransitionRun {
            run_id: run.id,
            to: RunStatus::Running,
            terminal_reason: None,
        })
        .with(GraphChange::TransitionRun {
            run_id: run.id,
            to: RunStatus::Verifying,
            terminal_reason: None,
        });
    transaction(
        GraphStore::new(pool.clone(), TENANT).expect("store"),
        &pool,
        correlation,
    )
    .apply(batch)
    .await
    .expect("second transaction commits");

    let committed = events(&events_store).await;
    assert_eq!(committed.len(), 4);
    let started = find(&committed, "run.started");
    assert_eq!(started.aggregate_id, run.id.to_string());
    assert_eq!(started.aggregate_version, 2);
    assert_eq!(started.payload["from"], "QUEUED");
    assert_eq!(started.payload["to"], "RUNNING");
    let verifying = find(&committed, "run.verifying");
    assert_eq!(verifying.aggregate_id, run.id.to_string());
    assert_eq!(verifying.aggregate_version, 3);
    assert_eq!(verifying.payload["from"], "RUNNING");
    assert_eq!(verifying.payload["to"], "VERIFYING");
    assert!(
        committed
            .iter()
            .all(|event| event.correlation_id == correlation),
        "everything caused by one input shares its correlation id"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn an_invalid_last_change_rolls_back_every_row_and_event() {
    let Some((name, pool, store)) = setup("tx_rollback").await else {
        blocked_marker();
        return;
    };
    let node_a = store.create_node(new_node("existing")).await.expect("node");
    let thread = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("thread");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE,
            seed_node(),
            thread.id,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    let before = (
        tenant_count(&pool, "SELECT count(*) FROM work_nodes").await,
        tenant_count(&pool, "SELECT count(*) FROM work_edges").await,
        tenant_count(&pool, "SELECT count(*) FROM runs").await,
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        tenant_count(&pool, "SELECT count(*) FROM event_outbox").await,
    );

    // The last change is illegal (`CREATED -> RUNNING` is not in the Run state machine):
    // the node, the edge and the run created earlier in the same batch must disappear
    // with it, together with every staged event.
    let batch = GraphBatch::new(WORKSPACE, base)
        .with(GraphChange::create_work_node(new_node("rolled back node")))
        .with(GraphChange::create_work_edge(NewWorkEdge::new(
            WORKSPACE,
            node_a.id,
            seed_node(),
            WorkEdgeKind::DependsOn,
        )))
        .with(GraphChange::create_run(NewRun::new(
            WORKSPACE,
            seed_node(),
            thread.id,
            RunTriggerKind::Manual,
        )))
        .with(GraphChange::TransitionRun {
            run_id: run.id,
            to: RunStatus::Running,
            terminal_reason: None,
        });
    let error = transaction(store, &pool, correlation())
        .apply(batch)
        .await
        .expect_err("the illegal transition must reject the whole transaction");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");
    assert!(
        matches!(
            error,
            GraphTransactionError::Graph(quansio_graph::GraphError::IllegalTransition { .. })
        ),
        "{error:?}"
    );

    let after = (
        tenant_count(&pool, "SELECT count(*) FROM work_nodes").await,
        tenant_count(&pool, "SELECT count(*) FROM work_edges").await,
        tenant_count(&pool, "SELECT count(*) FROM runs").await,
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        tenant_count(&pool, "SELECT count(*) FROM event_outbox").await,
    );
    assert_eq!(before, after, "nothing survives a rolled-back transaction");
    assert_eq!(after.2, 1, "the pre-existing run is the only run");
    assert_eq!(after.3, 0, "no runtime_event");
    assert_eq!(after.4, 0, "no outbox row");

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(
        reader.graph_revision(WORKSPACE).await.expect("head"),
        base,
        "a rolled-back transaction does not advance the revision"
    );
    assert!(reader
        .list_edges(WORKSPACE)
        .await
        .expect("edges")
        .is_empty());
    assert_eq!(reader.list_nodes(WORKSPACE).await.expect("nodes").len(), 2);

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_stale_base_revision_rejects_the_whole_transaction() {
    let Some((name, pool, store)) = setup("tx_stale").await else {
        blocked_marker();
        return;
    };
    let base = store.graph_revision(WORKSPACE).await.expect("head");
    // Another writer advances the workspace graph revision between our read and our write.
    store
        .create_node(new_node("concurrent"))
        .await
        .expect("node");
    let current = store.graph_revision(WORKSPACE).await.expect("head");
    assert!(current > base);

    let events_before = tenant_count(&pool, "SELECT count(*) FROM runtime_events").await;
    let batch = GraphBatch::new(WORKSPACE, base)
        .with(GraphChange::create_work_node(new_node("must not exist")));
    let error = transaction(store, &pool, correlation())
        .apply(batch)
        .await
        .expect_err("stale base revision");
    assert_eq!(error.code(), "CONFLICT_REVISION");
    match &error {
        GraphTransactionError::Graph(quansio_graph::GraphError::RevisionConflict {
            entity,
            expected,
            current: found,
            ..
        }) => {
            assert_eq!(*entity, "graph_head");
            assert_eq!(*expected, base.get());
            assert_eq!(*found, current.get());
        }
        other => panic!("unexpected error {other:?}"),
    }

    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(reader.list_nodes(WORKSPACE).await.expect("nodes").len(), 2);
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        events_before
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_rejected_transaction_emits_no_event() {
    let Some((name, pool, store)) = setup("tx_no_event").await else {
        blocked_marker();
        return;
    };
    let base = store.graph_revision(WORKSPACE).await.expect("head");

    // Unsupported change kind: refused before the transaction opens.
    let unsupported = GraphBatch::new(WORKSPACE, base).with(GraphChange::CreateTurn {
        run_id: CanonicalId::parse_typed("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", Prefix::Run)
            .expect("run id"),
        turn: NewTurn::new("manual"),
    });
    let error = transaction(store, &pool, correlation())
        .apply(unsupported)
        .await
        .expect_err("turns have no event mapping yet");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    match &error {
        GraphTransactionError::UnsupportedChange { change, .. } => {
            assert_eq!(*change, "CreateTurn");
        }
        other => panic!("unexpected error {other:?}"),
    }

    // An unmapped status change is refused on the same terms, and the graph row it named
    // is untouched.
    let thread = GraphStore::new(pool.clone(), TENANT)
        .expect("store")
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("thread");
    let base = GraphStore::new(pool.clone(), TENANT)
        .expect("store")
        .graph_revision(WORKSPACE)
        .await
        .expect("head");
    let unmapped = GraphBatch::new(WORKSPACE, base).with(GraphChange::TransitionAgentThread {
        thread_id: thread.id,
        to: AgentThreadStatus::Joining,
    });
    let error = transaction(
        GraphStore::new(pool.clone(), TENANT).expect("store"),
        &pool,
        correlation(),
    )
    .apply(unmapped)
    .await
    .expect_err("JOINING has no DOMAIN event name");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert!(
        matches!(error, GraphTransactionError::UnsupportedChange { .. }),
        "{error:?}"
    );

    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM runtime_events").await,
        0,
        "a rejected transaction writes no event"
    );
    assert_eq!(
        tenant_count(&pool, "SELECT count(*) FROM event_outbox").await,
        0,
        "a rejected transaction writes no outbox row"
    );
    let reader = GraphStore::new(pool.clone(), TENANT).expect("reader");
    assert_eq!(
        reader
            .get_agent_thread(&thread.id)
            .await
            .expect("thread")
            .status,
        AgentThreadStatus::Provisioned,
        "the rejected transition did not move the row"
    );
    assert_eq!(reader.list_nodes(WORKSPACE).await.expect("nodes").len(), 1);

    drop_pool(&pool, &name).await;
}
