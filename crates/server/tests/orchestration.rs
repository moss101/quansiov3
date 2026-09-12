//! RUN-004 acceptance: bounded-concurrency orchestration over the canonical runtime.
//!
//! The WorkGraph is a conformance double here (its production implementation applies
//! transitions through `crates/graph`'s transaction, which `crates/graph/tests` proves), but
//! everything else is real: real `runs` rows, the real Run state machine for dispatch and
//! cancellation, real ProtocolState writes and real RuntimeEvents.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Generation, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::orchestration::{
    cancel::CancelRequest, is_terminal_node_status, CapacityGate, DependencyEdge, GraphSnapshot,
    OrchestrationError, Orchestrator, ReleaseBatch, ReleaseOutcome, RunRef, WorkGraphPort,
    WorkNodeView,
};
use quansio_server::runtime::state_machine::{
    NewRun, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity,
};
use quansio_server::scheduler::{
    DispatchOutcome, RoutineFireOutcome, RoutineFireRequest, RunDispatch, RunDispatchRequest,
    RunResumeRequest, SchedulerError,
};
use serde_json::json;
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0RR004";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0RR004";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0RR004";
const NODE_ROOT: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0RR401";

// ---------------------------------------------------------------------------------------
// Conformance seams
// ---------------------------------------------------------------------------------------

/// A WorkGraph double that compare-and-sets the same way the graph transaction does, and can
/// be told to lose the revision race a fixed number of times.
#[derive(Clone, Default)]
struct FakeGraph {
    inner: Arc<Mutex<FakeInner>>,
}

struct FakeInner {
    snapshot: GraphSnapshot,
    batches: Vec<ReleaseBatch>,
    injected_conflicts: usize,
}

impl Default for FakeInner {
    fn default() -> Self {
        Self {
            snapshot: GraphSnapshot {
                workspace_id: WORKSPACE.to_string(),
                revision: 0,
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            batches: Vec::new(),
            injected_conflicts: 0,
        }
    }
}

impl FakeGraph {
    fn new(snapshot: GraphSnapshot) -> Self {
        let graph = Self::default();
        graph.inner.lock().expect("graph").snapshot = snapshot;
        graph
    }

    fn with_conflicts(self, conflicts: usize) -> Self {
        self.inner.lock().expect("graph").injected_conflicts = conflicts;
        self
    }

    fn snapshot(&self) -> GraphSnapshot {
        self.inner.lock().expect("graph").snapshot.clone()
    }

    fn batches(&self) -> Vec<ReleaseBatch> {
        self.inner.lock().expect("graph").batches.clone()
    }

    /// Refresh every node's run view from the durable `runs` rows, the way a real snapshot
    /// would read them. Capacity is counted from this durable state, so a double that kept
    /// stale `QUEUED` views would under-count in-flight work.
    async fn sync_runs(&self, pool: &PgPool) {
        let mut tx = pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant context");
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT id, status FROM runs WHERE tenant_id = $1",
        )
        .bind(TENANT)
        .fetch_all(&mut *tx)
        .await
        .expect("runs");
        tx.commit().await.expect("commit");
        let statuses: std::collections::HashMap<String, String> = rows.into_iter().collect();
        let mut inner = self.inner.lock().expect("graph");
        for node in &mut inner.snapshot.nodes {
            if let Some(run) = &mut node.run {
                if let Some(status) = statuses.get(&run.run_id) {
                    run.status = status.clone();
                }
            }
        }
    }

    fn clear_run(&self, node_id: &str) {
        let mut inner = self.inner.lock().expect("graph");
        if let Some(node) = inner
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
        {
            node.run = None;
            node.revision += 1;
        }
    }

    fn set_status(&self, node_id: &str, status: &str) {
        let mut inner = self.inner.lock().expect("graph");
        if let Some(node) = inner
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
        {
            node.status = status.to_string();
            node.revision += 1;
        }
    }
}

#[async_trait]
impl WorkGraphPort for FakeGraph {
    async fn snapshot(&self, _workspace_id: &str) -> Result<GraphSnapshot, OrchestrationError> {
        Ok(self.snapshot())
    }

    async fn apply(&self, batch: ReleaseBatch) -> Result<ReleaseOutcome, OrchestrationError> {
        let mut inner = self.inner.lock().expect("graph");
        inner.batches.push(batch.clone());
        if inner.injected_conflicts > 0 {
            inner.injected_conflicts -= 1;
            inner.snapshot.revision += 1;
            let current = inner.snapshot.revision;
            return Err(OrchestrationError::RevisionConflict {
                workspace_id: batch.workspace_id,
                expected: batch.base_revision,
                current,
            });
        }
        if batch.base_revision != inner.snapshot.revision {
            return Err(OrchestrationError::RevisionConflict {
                workspace_id: batch.workspace_id,
                expected: batch.base_revision,
                current: inner.snapshot.revision,
            });
        }
        for transition in &batch.transitions {
            let Some(index) = inner
                .snapshot
                .nodes
                .iter()
                .position(|node| node.id == transition.node_id)
            else {
                return Err(OrchestrationError::NotFound {
                    entity: "work_node",
                    id: transition.node_id.clone(),
                });
            };
            let node = &mut inner.snapshot.nodes[index];
            if node.revision != transition.expected_revision {
                return Err(OrchestrationError::RevisionConflict {
                    workspace_id: batch.workspace_id,
                    expected: transition.expected_revision,
                    current: node.revision,
                });
            }
            // The graph refuses to move a terminal node; the double mirrors that rule so an
            // illegal orchestration transition fails here too.
            if is_terminal_node_status(&node.status) {
                return Err(OrchestrationError::IllegalNodeTransition {
                    node_id: node.id.clone(),
                    from: node.status.clone(),
                    to: transition.to.clone(),
                });
            }
            node.status = transition.to.clone();
            node.revision += 1;
        }
        inner.snapshot.revision += 1;
        Ok(ReleaseOutcome {
            revision: inner.snapshot.revision,
            applied: batch.transitions.len(),
        })
    }

    async fn revision(&self, _workspace_id: &str) -> Result<u64, OrchestrationError> {
        Ok(self.inner.lock().expect("graph").snapshot.revision)
    }
}

/// Dispatch queued runs through the canonical Run state machine.
#[derive(Clone)]
struct StoreDispatch {
    engine: RuntimeEngine,
    dispatches: Arc<Mutex<Vec<String>>>,
}

impl StoreDispatch {
    fn new(engine: RuntimeEngine) -> Self {
        Self {
            engine,
            dispatches: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn dispatch_count(&self) -> usize {
        self.dispatches.lock().expect("dispatches").len()
    }
}

#[async_trait]
impl RunDispatch for StoreDispatch {
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        let run_id = CanonicalId::parse_typed(&request.run_id, Prefix::Run).map_err(|error| {
            SchedulerError::RunDispatch {
                run_id: request.run_id.clone(),
                reason: error.to_string(),
            }
        })?;
        let generation =
            Generation::new(request.generation).map_err(|error| SchedulerError::RunDispatch {
                run_id: request.run_id.clone(),
                reason: error.to_string(),
            })?;
        let current = self
            .engine
            .store()
            .load_run(&run_id)
            .await
            .map_err(|error| SchedulerError::RunDispatch {
                run_id: request.run_id.clone(),
                reason: error.to_string(),
            })?;
        if current.status == RunStatus::Running {
            return Ok(DispatchOutcome::AlreadyRunning);
        }
        match self.engine.store().start(&run_id, generation).await {
            Ok(run) if run.status == RunStatus::Running => {
                self.dispatches
                    .lock()
                    .expect("dispatches")
                    .push(request.run_id.clone());
                Ok(DispatchOutcome::Dispatched)
            }
            Ok(_) => Ok(DispatchOutcome::NotDispatchable),
            Err(RuntimeError::FencedStaleGeneration { .. }) => Ok(DispatchOutcome::Fenced),
            Err(RuntimeError::IllegalTransition { .. }) => Ok(DispatchOutcome::NotDispatchable),
            Err(error) => Err(SchedulerError::RunDispatch {
                run_id: request.run_id,
                reason: error.to_string(),
            }),
        }
    }

    async fn resume_timer_wait(
        &self,
        _request: RunResumeRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        Ok(DispatchOutcome::NotDispatchable)
    }

    async fn fire_routine(
        &self,
        _request: RoutineFireRequest,
    ) -> Result<RoutineFireOutcome, SchedulerError> {
        Err(SchedulerError::RunDispatch {
            run_id: "routine".to_string(),
            reason: "the orchestration suite does not fire routines".to_string(),
        })
    }
}

// ---------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
    engine: RuntimeEngine,
    agent_thread: CanonicalId,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE_ROOT).await;

    let mut generator = UlidGenerator::new();
    let agent_thread = CanonicalId::generate(Prefix::AgentThread, &mut generator);
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
    tx.commit().await.expect("commit");

    let identity = RuntimeIdentity::system(
        TENANT,
        "orchestration-test",
        CorrelationId::generate(&mut generator),
    );
    let engine = RuntimeEngine::new(pool.clone(), identity.clone()).expect("engine");
    Some(Fixture {
        name,
        pool,
        identity,
        engine,
        agent_thread,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

/// Create a queued Run for a node, the way the command path does before orchestration runs.
async fn queued_run(fixture: &Fixture, title: &str) -> RunRef {
    let mut new_run = NewRun::new(
        WORKSPACE,
        CanonicalId::parse_typed(NODE_ROOT, Prefix::WorkNode).expect("node"),
        fixture.agent_thread,
        RunTriggerKind::Manual,
    );
    new_run.trigger_ref = Some(title.to_string());
    let run = fixture
        .engine
        .create_run(new_run)
        .await
        .expect("create run");
    let run = fixture
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    RunRef {
        run_id: run.id.to_string(),
        generation: run.generation.get(),
        status: "QUEUED".to_string(),
    }
}

fn node(id: &str, status: &str, run: Option<RunRef>) -> WorkNodeView {
    WorkNodeView {
        id: id.to_string(),
        kind: "task".to_string(),
        status: status.to_string(),
        parent_id: None,
        priority: 1,
        revision: 1,
        run,
    }
}

fn depends_on(from: &str, to: &str) -> DependencyEdge {
    DependencyEdge {
        from_node_id: from.to_string(),
        to_node_id: to.to_string(),
        kind: "depends_on".to_string(),
    }
}

fn snapshot(nodes: Vec<WorkNodeView>, edges: Vec<DependencyEdge>) -> GraphSnapshot {
    GraphSnapshot {
        workspace_id: WORKSPACE.to_string(),
        revision: 10,
        nodes,
        edges,
    }
}

async fn events_of_type(pool: &PgPool, event_type: &str) -> Vec<serde_json::Value> {
    let store = quansio_events::EventStore::new(pool.clone());
    store
        .read_events_after(TENANT, None, 2_000)
        .await
        .expect("events")
        .into_iter()
        .filter(|event| event.event_type.to_string() == event_type)
        .map(|event| event.payload)
        .collect()
}

fn orchestrator(
    fixture: &Fixture,
    graph: FakeGraph,
    dispatch: StoreDispatch,
    limit: usize,
) -> Orchestrator {
    Orchestrator::new(
        fixture.pool.clone(),
        fixture.identity.clone(),
        Arc::new(graph),
        Arc::new(dispatch),
        CapacityGate::new(limit),
    )
    .expect("orchestrator")
}

// ---------------------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_parallel_dag_releases_and_dispatches_in_dependency_order() {
    let Some(fixture) = prepare("run004_dag").await else {
        blocked_marker();
        return;
    };
    // A → {B, C} → D: the D that depends on both B and C must wait for both.
    let run_b = queued_run(&fixture, "b").await;
    let run_c = queued_run(&fixture, "c").await;
    let run_d = queued_run(&fixture, "d").await;

    let mut high = node("wn_b", "ready", Some(run_b.clone()));
    high.priority = 3;
    let mut low = node("wn_c", "ready", Some(run_c.clone()));
    low.priority = 0;
    let graph = FakeGraph::new(snapshot(
        vec![
            node("wn_a", "done", None),
            high,
            low,
            node("wn_d", "draft", Some(run_d.clone())),
        ],
        vec![
            depends_on("wn_b", "wn_a"),
            depends_on("wn_c", "wn_a"),
            depends_on("wn_d", "wn_b"),
            depends_on("wn_d", "wn_c"),
        ],
    ));
    let dispatch = StoreDispatch::new(fixture.engine.clone());
    let orchestrator = orchestrator(&fixture, graph.clone(), dispatch.clone(), 8);

    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.released, 0, "wn_d is not releasable yet");
    assert_eq!(report.dispatched, 2, "B and C may run together");
    assert!(
        graph.batches().is_empty(),
        "B and C were already ready, so nothing needed a release"
    );

    // Finish the first dependent; D must still wait for the second.
    graph.set_status("wn_b", "done");
    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.released, 0, "D still waits on C");
    graph.set_status("wn_c", "done");
    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(
        report.released, 1,
        "D becomes ready only once every prerequisite is done"
    );
    assert_eq!(report.dispatched, 1, "D starts after both prerequisites");
    assert_eq!(
        graph.snapshot().node("wn_d").expect("d").status,
        "ready",
        "the release was committed to the graph"
    );
    assert_eq!(dispatch.dispatch_count(), 3, "A's dependents plus D");

    // Every dispatch is one run transition, so a repeat tick changes nothing.
    graph.sync_runs(&fixture.pool).await;
    let quiet = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert!(
        quiet.is_quiet(),
        "a second tick over the same graph is a no-op"
    );
    assert_eq!(dispatch.dispatch_count(), 3);
    assert_eq!(
        events_of_type(&fixture.pool, "run.started").await.len(),
        3,
        "exactly one run.started per dispatched run"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_dependent_with_a_failed_prerequisite_is_blocked_never_dispatched() {
    let Some(fixture) = prepare("run004_block").await else {
        blocked_marker();
        return;
    };
    let run_b = queued_run(&fixture, "b").await;
    let graph = FakeGraph::new(snapshot(
        vec![
            node("wn_a", "failed", None),
            node("wn_b", "draft", Some(run_b.clone())),
        ],
        vec![depends_on("wn_b", "wn_a")],
    ));
    let dispatch = StoreDispatch::new(fixture.engine.clone());
    let orchestrator = orchestrator(&fixture, graph.clone(), dispatch.clone(), 4);

    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.dispatched, 0, "a failed prerequisite starts nothing");
    assert_eq!(report.released, 1, "the dependent is marked blocked");
    assert_eq!(graph.snapshot().node("wn_b").expect("b").status, "blocked");
    assert_eq!(dispatch.dispatch_count(), 0);
    finish(fixture).await;
}

#[tokio::test]
async fn capacity_bounds_concurrency_and_frees_slots_as_runs_finish() {
    let Some(fixture) = prepare("run004_capacity").await else {
        blocked_marker();
        return;
    };
    let run_a = queued_run(&fixture, "a").await;
    let run_b = queued_run(&fixture, "b").await;
    let run_c = queued_run(&fixture, "c").await;
    let graph = FakeGraph::new(snapshot(
        vec![
            node("wn_a", "ready", Some(run_a.clone())),
            node("wn_b", "ready", Some(run_b.clone())),
            node("wn_c", "ready", Some(run_c.clone())),
        ],
        vec![],
    ));
    let dispatch = StoreDispatch::new(fixture.engine.clone());
    let orchestrator = orchestrator(&fixture, graph.clone(), dispatch.clone(), 2);

    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.dispatched, 2, "the limit admits two");
    assert_eq!(report.deferred, vec!["wn_c".to_string()]);
    let refusal = report.capacity.expect("a typed refusal is reported");
    assert_eq!(refusal.limit, 2);
    assert_eq!(
        refusal.into_error().code(),
        "BUDGET_EXHAUSTED",
        "a saturated limit is a typed refusal, not a silent drop"
    );

    // The two dispatched runs now hold their slots (durable state), so the third still waits.
    graph.sync_runs(&fixture.pool).await;
    graph.set_status("wn_a", "in_progress");
    graph.set_status("wn_b", "in_progress");
    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.dispatched, 0);
    assert!(report.capacity.is_some(), "still saturated");

    // One run finishes: the slot is durable state, so the next tick admits the third node.
    graph.set_status("wn_a", "done");
    graph.clear_run("wn_a");
    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.dispatched, 1, "the freed slot admits wn_c");
    assert!(report.capacity.is_none());
    finish(fixture).await;
}

#[tokio::test]
async fn a_cancel_storm_cancels_each_run_once_and_blocks_dependents() {
    let Some(fixture) = prepare("run004_cancel").await else {
        blocked_marker();
        return;
    };
    let run_root = queued_run(&fixture, "root").await;
    let run_child = queued_run(&fixture, "child").await;
    let run_unstarted = queued_run(&fixture, "unstarted child").await;
    let run_dependent = queued_run(&fixture, "dependent").await;

    // Start root and child so there is live work to cancel; the dependent stays queued.
    let mut running_root = run_root.clone();
    let mut running_child = run_child.clone();
    for run in [&mut running_root, &mut running_child] {
        let run_id = CanonicalId::parse_typed(&run.run_id, Prefix::Run).expect("run");
        let started = fixture
            .engine
            .store()
            .start(
                &run_id,
                Generation::new(run.generation).expect("generation"),
            )
            .await
            .expect("start run");
        run.status = started.status.as_db_str().to_string();
    }
    let run_root = running_root;
    let run_child = running_child;
    let dispatch = StoreDispatch::new(fixture.engine.clone());
    let mut child = node("wn_child", "in_progress", Some(run_child.clone()));
    child.parent_id = Some("wn_root".to_string());
    // A child of the subtree whose run was never started: the Run state machine has no
    // `QUEUED -> CANCELLED` edge, so the orchestrator must report it rather than cancel it.
    let mut unstarted_child = node("wn_unstarted", "ready", Some(run_unstarted.clone()));
    unstarted_child.parent_id = Some("wn_root".to_string());
    let graph = FakeGraph::new(snapshot(
        vec![
            node("wn_root", "in_progress", Some(run_root.clone())),
            child,
            unstarted_child,
            node("wn_dependent", "ready", Some(run_dependent.clone())),
        ],
        vec![depends_on("wn_dependent", "wn_child")],
    ));
    let orchestrator = orchestrator(&fixture, graph.clone(), dispatch.clone(), 8);

    // A storm: eight concurrent cancellations of the same subtree.
    let request = CancelRequest {
        workspace_id: WORKSPACE.to_string(),
        node_id: "wn_root".to_string(),
        reason: "operator cancelled".to_string(),
    };
    let mut handles = Vec::new();
    for _ in 0..8 {
        let orchestrator = orchestrator.clone();
        let request = request.clone();
        handles.push(tokio::spawn(async move {
            orchestrator.cancel_subtree(request).await
        }));
    }
    let mut cancelled_runs = 0;
    let mut left_unstarted: Vec<String> = Vec::new();
    let mut cancelled_nodes: Vec<String> = Vec::new();
    for handle in handles {
        let outcome = handle.await.expect("join").expect("cancel");
        cancelled_runs += outcome.runs_cancelled.len();
        left_unstarted.extend(outcome.runs_left_unstarted);
        cancelled_nodes.extend(
            outcome
                .cancelled
                .into_iter()
                .filter(|node| node.applied)
                .map(|node| node.node_id),
        );
    }
    assert!(
        left_unstarted.iter().any(|run| run == &run_unstarted.run_id),
        "an unstarted run inside the subtree is reported, never silently dropped: {left_unstarted:?}"
    );

    assert_eq!(
        cancelled_runs, 2,
        "exactly the two live runs are cancelled, once each"
    );
    assert_eq!(
        events_of_type(&fixture.pool, "run.cancelled").await.len(),
        2,
        "one run.cancelled per run, never one per cancel request"
    );
    cancelled_nodes.sort();
    cancelled_nodes.dedup();
    assert!(
        cancelled_nodes.contains(&"wn_root".to_string()),
        "the subtree root was cancelled: {cancelled_nodes:?}"
    );

    // The subtree is cancelled and the dependent outside it is blocked, not started.
    let final_snapshot = graph.snapshot();
    assert_eq!(
        final_snapshot.node("wn_root").expect("root").status,
        "cancelled"
    );
    assert_eq!(
        final_snapshot.node("wn_child").expect("child").status,
        "cancelled"
    );
    assert_eq!(
        final_snapshot
            .node("wn_unstarted")
            .expect("unstarted")
            .status,
        "cancelled",
        "the node is cancelled even though its queued run had no cancellation edge"
    );
    assert_eq!(
        final_snapshot
            .node("wn_dependent")
            .expect("dependent")
            .status,
        "blocked"
    );

    // Cancelling again is a no-op, and no effect is dispatched or re-dispatched.
    let again = orchestrator
        .cancel_subtree(request)
        .await
        .expect("second cancel");
    assert!(again.already_cancelled || again.runs_cancelled.is_empty());
    assert_eq!(
        dispatch.dispatch_count(),
        0,
        "cancellation dispatches nothing"
    );
    assert_eq!(
        events_of_type(&fixture.pool, "run.cancelled").await.len(),
        2,
        "no further cancellation events"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_lost_graph_race_is_recomputed_against_the_new_revision() {
    let Some(fixture) = prepare("run004_race").await else {
        blocked_marker();
        return;
    };
    let run_a = queued_run(&fixture, "a").await;
    let graph = FakeGraph::new(snapshot(
        vec![
            node("wn_done", "done", None),
            node("wn_a", "draft", Some(run_a.clone())),
        ],
        vec![depends_on("wn_a", "wn_done")],
    ))
    .with_conflicts(1);
    let dispatch = StoreDispatch::new(fixture.engine.clone());
    let orchestrator = orchestrator(&fixture, graph.clone(), dispatch.clone(), 4);

    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.released, 1, "the retry released the node");
    assert_eq!(report.dispatched, 1);
    assert!(
        graph.batches().len() >= 2,
        "the losing batch was recomputed rather than applied twice"
    );
    assert_eq!(
        graph.snapshot().node("wn_a").expect("a").status,
        "ready",
        "exactly one release landed"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_waiting_parent_joins_once_its_last_child_completes() {
    let Some(fixture) = prepare("run004_join").await else {
        blocked_marker();
        return;
    };
    let mut parent = node("wn_parent", "waiting", None);
    parent.kind = "objective".to_string();
    let mut child_a = node("wn_child_a", "done", None);
    child_a.parent_id = Some("wn_parent".to_string());
    let mut child_b = node("wn_child_b", "in_progress", None);
    child_b.parent_id = Some("wn_parent".to_string());
    let graph = FakeGraph::new(snapshot(vec![parent, child_a, child_b], vec![]));
    let dispatch = StoreDispatch::new(fixture.engine.clone());
    let orchestrator = orchestrator(&fixture, graph.clone(), dispatch.clone(), 4);

    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.joined, 0, "the last child has not finished");
    graph.set_status("wn_child_b", "done");
    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.joined, 1, "the parent joins once");
    assert_eq!(
        graph.snapshot().node("wn_parent").expect("parent").status,
        "in_progress"
    );
    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.joined, 0, "the join does not repeat");
    finish(fixture).await;
}

#[tokio::test]
async fn a_cycle_in_the_graph_is_refused_before_anything_is_dispatched() {
    let Some(fixture) = prepare("run004_cycle").await else {
        blocked_marker();
        return;
    };
    let run_a = queued_run(&fixture, "a").await;
    let graph = FakeGraph::new(snapshot(
        vec![
            node("wn_a", "ready", Some(run_a)),
            node("wn_b", "ready", None),
        ],
        vec![depends_on("wn_a", "wn_b"), depends_on("wn_b", "wn_a")],
    ));
    let dispatch = StoreDispatch::new(fixture.engine.clone());
    let orchestrator = orchestrator(&fixture, graph.clone(), dispatch.clone(), 4);

    let error = orchestrator.tick(WORKSPACE).await.expect_err("cycle");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");
    assert_eq!(dispatch.dispatch_count(), 0);
    assert!(graph.batches().is_empty(), "no batch was applied");
    let _ = json!({});
    finish(fixture).await;
}
