//! Orchestration retry exhaustion, cancellation races and graph refusals (QA-001 coverage of
//! RUN-004).
//!
//! These suites complement `tests/orchestration.rs` and `tests/graph_orchestration.rs`. The pure
//! tests pin the snapshot helpers, the cancellation plan, the join plan, the cycle check, the run
//! status predicates and the error codes. The database tests drive the real `Orchestrator` (real
//! `runs` rows and the real Run state machine for cancellation) over a WorkGraph conformance
//! double that can lose the revision race, lose it to a concurrent canceller, or refuse a batch,
//! and the production `GraphWorkGraph` over real graph rows for its refusals.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::orchestration::cancel::cancel_plan;
use quansio_server::runtime::orchestration::{
    ensure_acyclic, is_active_run_status, is_terminal_run_status, join_plan, node_ids,
    CancelRequest, CancellationOutcome, CapacityGate, DependencyEdge, GraphSnapshot,
    GraphWorkGraph, NodeTransition, OrchestrationError, Orchestrator, ReleaseBatch, ReleaseOutcome,
    RunRef, UnavailableWorkGraph, WorkGraphPort, WorkNodeView, GRAPH_TRANSACTION_OWNER,
    MAX_CANCEL_ATTEMPTS, MAX_REVISION_ATTEMPTS,
};
use quansio_server::runtime::state_machine::{
    NewRun, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity,
};
use quansio_server::scheduler::{
    DispatchOutcome, RoutineFireOutcome, RoutineFireRequest, RunDispatch, RunDispatchRequest,
    RunResumeRequest, SchedulerError,
};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const MISSING_RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";

// ------------------------------------------------------------------------------ graph builders

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

fn child(id: &str, status: &str, parent: &str) -> WorkNodeView {
    let mut view = node(id, status, None);
    view.parent_id = Some(parent.to_string());
    view
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

fn run_ref(run_id: &str, status: &str) -> RunRef {
    RunRef {
        run_id: run_id.to_string(),
        generation: 1,
        status: status.to_string(),
    }
}

fn cancel_request(node_id: &str) -> CancelRequest {
    CancelRequest {
        workspace_id: WORKSPACE.to_string(),
        node_id: node_id.to_string(),
        reason: "operator cancelled".to_string(),
    }
}

// ---------------------------------------------------------------------------------- pure suites

#[test]
fn run_status_predicates_split_terminal_from_active_runs() {
    for terminal in ["SUCCEEDED", "FAILED", "CANCELLED", "BLOCKED_UNRECOVERABLE"] {
        assert!(is_terminal_run_status(terminal), "{terminal}");
        assert!(!is_active_run_status(terminal), "{terminal}");
    }
    for active in ["CREATED", "QUEUED", "RUNNING", "WAITING_CHILD", "SUSPENDED"] {
        assert!(!is_terminal_run_status(active), "{active}");
        assert!(is_active_run_status(active), "{active}");
    }
}

#[test]
fn orchestration_errors_map_to_their_domain_codes() {
    let cases: Vec<(OrchestrationError, &str)> = vec![
        (
            OrchestrationError::PortUnavailable {
                owner: GRAPH_TRANSACTION_OWNER,
                detail: "x".to_string(),
            },
            "INTERNAL",
        ),
        (
            OrchestrationError::CapacitySaturated {
                in_flight: 2,
                limit: 2,
            },
            "BUDGET_EXHAUSTED",
        ),
        (
            OrchestrationError::PrerequisiteUnsatisfied {
                node_id: "a".to_string(),
                prerequisite_id: "b".to_string(),
                status: "failed".to_string(),
            },
            "CONFLICT_STATE",
        ),
        (
            OrchestrationError::IllegalNodeTransition {
                node_id: "a".to_string(),
                from: "done".to_string(),
                to: "ready".to_string(),
            },
            "RUNTIME_ILLEGAL_TRANSITION",
        ),
        (
            OrchestrationError::DependencyCycle {
                node_id: "a".to_string(),
            },
            "VALIDATION_SCHEMA",
        ),
        (
            OrchestrationError::NotDispatchable {
                run_id: "r".to_string(),
                reason: "gone".to_string(),
            },
            "CONFLICT_STATE",
        ),
        (
            OrchestrationError::Runtime(RuntimeError::FencedStaleGeneration {
                received: 1,
                current: 2,
            }),
            RuntimeError::FencedStaleGeneration {
                received: 1,
                current: 2,
            }
            .code(),
        ),
        (
            OrchestrationError::Core(quansio_core::CoreError::InvalidUlid("x".to_string())),
            "VALIDATION_SCHEMA",
        ),
        (
            OrchestrationError::Scheduler(SchedulerError::RunDispatch {
                run_id: "r".to_string(),
                reason: "down".to_string(),
            }),
            "INTERNAL",
        ),
        (
            OrchestrationError::Database(sqlx::Error::RowNotFound),
            "INTERNAL",
        ),
        (
            OrchestrationError::Event(quansio_events::EventError::Database(
                sqlx::Error::RowNotFound,
            )),
            "INTERNAL",
        ),
    ];
    for (error, code) in cases {
        assert_eq!(error.code(), code, "{error}");
    }
    assert_ne!(
        OrchestrationError::Runtime(RuntimeError::FencedStaleGeneration {
            received: 1,
            current: 2
        })
        .code(),
        "INTERNAL",
        "a fenced runtime refusal keeps its own code"
    );
}

#[test]
fn prerequisites_are_satisfied_only_when_every_one_exists_and_is_done() {
    let graph = snapshot(
        vec![
            node("wn_done", "done", None),
            node("wn_running", "in_progress", None),
            node("wn_a", "draft", None),
            node("wn_b", "draft", None),
            node("wn_c", "draft", None),
        ],
        vec![
            depends_on("wn_a", "wn_done"),
            depends_on("wn_b", "wn_done"),
            depends_on("wn_b", "wn_running"),
            depends_on("wn_c", "wn_ghost"),
        ],
    );
    assert!(graph.prerequisites_satisfied("wn_a"));
    assert_eq!(graph.unsatisfied_prerequisite("wn_a"), None);
    assert!(!graph.prerequisites_satisfied("wn_b"));
    assert_eq!(
        graph.unsatisfied_prerequisite("wn_b"),
        Some(("wn_running".to_string(), "in_progress".to_string()))
    );
    assert!(
        !graph.prerequisites_satisfied("wn_c"),
        "a prerequisite the snapshot cannot see is never satisfied"
    );
    assert_eq!(
        graph.unsatisfied_prerequisite("wn_c"),
        Some(("wn_ghost".to_string(), "missing".to_string()))
    );
    assert!(graph.prerequisites_satisfied("wn_done"), "no prerequisites");
    assert_eq!(
        node_ids(&graph).into_iter().collect::<Vec<_>>(),
        vec!["wn_a", "wn_b", "wn_c", "wn_done", "wn_running"]
    );
}

#[tokio::test]
async fn the_unwired_work_graph_fails_closed_naming_its_owner() {
    let graph = UnavailableWorkGraph;
    let refusals = [
        graph.snapshot(WORKSPACE).await.map(|_| ()),
        graph
            .apply(ReleaseBatch {
                workspace_id: WORKSPACE.to_string(),
                base_revision: 1,
                transitions: Vec::new(),
            })
            .await
            .map(|_| ()),
        graph.revision(WORKSPACE).await.map(|_| ()),
    ];
    for refusal in refusals {
        assert!(
            matches!(
                refusal,
                Err(OrchestrationError::PortUnavailable {
                    owner: GRAPH_TRANSACTION_OWNER,
                    ..
                })
            ),
            "{refusal:?}"
        );
    }
}

#[test]
fn a_cancel_plan_refuses_an_unknown_root_and_blocks_each_outside_dependent_once() {
    let graph = snapshot(
        vec![
            node(
                "wn_root",
                "in_progress",
                Some(run_ref("run_done", "SUCCEEDED")),
            ),
            child("wn_kid", "in_progress", "wn_root"),
            child("wn_sibling", "ready", "wn_root"),
            child("wn_finished", "done", "wn_root"),
            node("wn_outside", "ready", None),
            node("wn_twice", "ready", None),
            node("wn_already_blocked", "blocked", None),
        ],
        vec![
            // Inside the subtree: the sibling is cancelled, not blocked.
            depends_on("wn_sibling", "wn_kid"),
            depends_on("wn_outside", "wn_kid"),
            depends_on("wn_twice", "wn_root"),
            depends_on("wn_twice", "wn_kid"),
            depends_on("wn_already_blocked", "wn_root"),
            // An edge from a node the snapshot does not hold.
            depends_on("wn_ghost", "wn_root"),
        ],
    );
    let refused = cancel_plan(&graph, &cancel_request("wn_missing")).expect_err("unknown root");
    assert!(
        matches!(&refused, OrchestrationError::NotFound { entity: "work_node", id } if id == "wn_missing"),
        "{refused:?}"
    );

    let plan = cancel_plan(&graph, &cancel_request("wn_root")).expect("plan");
    let cancelled: Vec<&str> = plan
        .cancelled_nodes
        .iter()
        .map(|transition| transition.node_id.as_str())
        .collect();
    assert_eq!(cancelled, vec!["wn_kid", "wn_root", "wn_sibling"]);
    let blocked: Vec<&str> = plan
        .blocked_dependents
        .iter()
        .map(|transition| transition.node_id.as_str())
        .collect();
    assert_eq!(blocked, vec!["wn_outside", "wn_twice"]);
    assert!(
        plan.runs.is_empty() && plan.unstarted_runs.is_empty(),
        "a finished run is neither cancelled nor reported unstarted"
    );
    assert_eq!(plan.transitions().len(), 5);

    let done = CancellationOutcome::already_done();
    assert!(done.already_cancelled);
    assert!(done.cancelled.is_empty() && done.blocked.is_empty());
    assert!(done.runs_cancelled.is_empty() && done.runs_already_cancelled.is_empty());
    assert!(done.runs_left_unstarted.is_empty());
}

#[test]
fn join_plan_ignores_a_waiting_node_without_children_and_the_cycle_check_accepts_any_order() {
    let graph = snapshot(
        vec![
            node("wn_waiting", "waiting", None),
            node("wn_dependent", "draft", None),
            node("wn_prerequisite", "done", None),
        ],
        // The dependent is listed before its prerequisite, so the walk from it colours the
        // prerequisite before the prerequisite's own turn comes.
        vec![depends_on("wn_dependent", "wn_prerequisite")],
    );
    let plan = join_plan(&graph);
    assert!(plan.is_empty(), "{plan:?}");
    ensure_acyclic(&graph).expect("acyclic");

    let cyclic = snapshot(
        vec![node("wn_x", "draft", None), node("wn_y", "draft", None)],
        vec![depends_on("wn_x", "wn_y"), depends_on("wn_y", "wn_x")],
    );
    assert!(matches!(
        ensure_acyclic(&cyclic),
        Err(OrchestrationError::DependencyCycle { .. })
    ));
}

// ---------------------------------------------------------------------------- conformance seams

/// What the scripted graph does with the next batch.
#[derive(Debug, Clone)]
enum ApplyScript {
    /// Another writer moved the graph: refuse with a revision conflict.
    LoseRace,
    /// A concurrent canceller applied this very batch (and its runs are cancelled) first.
    LoseRaceToCanceller,
    /// Refuse for a reason other than a race.
    Refuse,
}

#[derive(Clone)]
struct ScriptedGraph {
    inner: Arc<Mutex<ScriptedInner>>,
}

struct ScriptedInner {
    snapshot: GraphSnapshot,
    script: VecDeque<ApplyScript>,
    applies: usize,
}

impl ScriptedGraph {
    fn new(snapshot: GraphSnapshot, script: Vec<ApplyScript>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ScriptedInner {
                snapshot,
                script: script.into(),
                applies: 0,
            })),
        }
    }

    fn applies(&self) -> usize {
        self.inner.lock().expect("graph").applies
    }

    fn status(&self, node_id: &str) -> String {
        self.inner
            .lock()
            .expect("graph")
            .snapshot
            .node(node_id)
            .expect("node")
            .status
            .clone()
    }
}

fn apply_transitions(snapshot: &mut GraphSnapshot, transitions: &[NodeTransition]) {
    for transition in transitions {
        if let Some(node) = snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == transition.node_id)
        {
            node.status = transition.to.clone();
            node.revision += 1;
            if transition.to == "cancelled" {
                if let Some(run) = &mut node.run {
                    run.status = "CANCELLED".to_string();
                }
            }
        }
    }
    snapshot.revision += 1;
}

#[async_trait]
impl WorkGraphPort for ScriptedGraph {
    async fn snapshot(&self, _workspace_id: &str) -> Result<GraphSnapshot, OrchestrationError> {
        Ok(self.inner.lock().expect("graph").snapshot.clone())
    }

    async fn apply(&self, batch: ReleaseBatch) -> Result<ReleaseOutcome, OrchestrationError> {
        let mut inner = self.inner.lock().expect("graph");
        inner.applies += 1;
        let conflict = |inner: &ScriptedInner| OrchestrationError::RevisionConflict {
            workspace_id: batch.workspace_id.clone(),
            expected: batch.base_revision,
            current: inner.snapshot.revision,
        };
        match inner.script.pop_front() {
            Some(ApplyScript::LoseRace) => {
                inner.snapshot.revision += 1;
                Err(conflict(&inner))
            }
            Some(ApplyScript::LoseRaceToCanceller) => {
                apply_transitions(&mut inner.snapshot, &batch.transitions);
                Err(conflict(&inner))
            }
            Some(ApplyScript::Refuse) => Err(OrchestrationError::IllegalNodeTransition {
                node_id: batch
                    .transitions
                    .first()
                    .map(|transition| transition.node_id.clone())
                    .unwrap_or_default(),
                from: "cancelled".to_string(),
                to: "ready".to_string(),
            }),
            None => {
                if batch.base_revision != inner.snapshot.revision {
                    return Err(conflict(&inner));
                }
                apply_transitions(&mut inner.snapshot, &batch.transitions);
                Ok(ReleaseOutcome {
                    revision: inner.snapshot.revision,
                    applied: batch.transitions.len(),
                })
            }
        }
    }

    async fn revision(&self, _workspace_id: &str) -> Result<u64, OrchestrationError> {
        Ok(self.inner.lock().expect("graph").snapshot.revision)
    }
}

/// A dispatcher whose answer per run is scripted.
#[derive(Clone, Default)]
struct ScriptedDispatch {
    outcomes: Arc<HashMap<String, DispatchOutcome>>,
    asked: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl RunDispatch for ScriptedDispatch {
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        self.asked
            .lock()
            .expect("asked")
            .push(request.run_id.clone());
        Ok(self
            .outcomes
            .get(&request.run_id)
            .copied()
            .unwrap_or(DispatchOutcome::Dispatched))
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
            reason: "this suite does not fire routines".to_string(),
        })
    }
}

// ------------------------------------------------------------------------------------- fixture

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
    engine: RuntimeEngine,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(AGENT)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    tx.commit().await.expect("commit");
    let identity = RuntimeIdentity::system(
        TENANT,
        "orchestration-paths-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let engine = RuntimeEngine::new(pool.clone(), identity.clone()).expect("engine");
    Some(Fixture {
        name,
        pool,
        identity,
        engine,
    })
}

fn orchestrator(f: &Fixture, graph: ScriptedGraph, dispatch: ScriptedDispatch) -> Orchestrator {
    Orchestrator::new(
        f.pool.clone(),
        f.identity.clone(),
        Arc::new(graph),
        Arc::new(dispatch),
        CapacityGate::new(8),
    )
    .expect("orchestrator")
}

async fn running_run(f: &Fixture) -> RunRef {
    let run = f
        .engine
        .create_run(NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
            CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("create run");
    let run = f
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    let run = f
        .engine
        .start(&run.id, run.generation)
        .await
        .expect("start");
    run_ref(&run.id.to_string(), "RUNNING")
}

async fn run_status(f: &Fixture, run: &RunRef) -> RunStatus {
    f.engine
        .store()
        .load_run(&CanonicalId::parse_typed(&run.run_id, Prefix::Run).expect("run"))
        .await
        .expect("run")
        .status
}

async fn cancelled_events(f: &Fixture) -> usize {
    quansio_events::EventStore::new(f.pool.clone())
        .read_events_after(TENANT, None, 5_000)
        .await
        .expect("events")
        .into_iter()
        .filter(|event| event.event_type.to_string() == "run.cancelled")
        .count()
}

// ------------------------------------------------------------------------------------ DB suites

#[tokio::test]
async fn a_tick_counts_fenced_and_undispatchable_runs_apart_from_dispatched_ones() {
    let Some(f) = prepare("qa001_orch_tick_outcomes").await else {
        blocked_marker();
        return;
    };
    let graph = ScriptedGraph::new(
        snapshot(
            vec![
                node("wn_a", "ready", Some(run_ref("run_a", "QUEUED"))),
                node("wn_b", "ready", Some(run_ref("run_b", "QUEUED"))),
                node("wn_c", "ready", Some(run_ref("run_c", "QUEUED"))),
                node("wn_d", "ready", Some(run_ref("run_d", "QUEUED"))),
            ],
            Vec::new(),
        ),
        Vec::new(),
    );
    let dispatch = ScriptedDispatch {
        outcomes: Arc::new(HashMap::from([
            ("run_a".to_string(), DispatchOutcome::Fenced),
            ("run_b".to_string(), DispatchOutcome::NotDispatchable),
            ("run_c".to_string(), DispatchOutcome::AlreadyRunning),
        ])),
        ..ScriptedDispatch::default()
    };
    let orchestrator = orchestrator(&f, graph.clone(), dispatch.clone());
    assert_eq!(orchestrator.identity().tenant_id, TENANT);
    assert_eq!(orchestrator.capacity(), CapacityGate::new(8));
    let report = orchestrator.tick(WORKSPACE).await.expect("tick");
    assert_eq!(report.fenced, 1);
    assert_eq!(report.not_dispatchable, 1);
    assert_eq!(report.already_running, 1);
    assert_eq!(report.dispatched, 1);
    assert_eq!(
        report.graph_revision, 10,
        "nothing to release, no graph write"
    );
    assert_eq!(graph.applies(), 0);
    assert_eq!(dispatch.asked.lock().expect("asked").len(), 4);

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_tick_gives_up_after_losing_every_revision_race_and_surfaces_other_refusals() {
    let Some(f) = prepare("qa001_orch_tick_races").await else {
        blocked_marker();
        return;
    };
    let releasable = || {
        snapshot(
            vec![
                node("wn_done", "done", None),
                node("wn_a", "draft", Some(run_ref("run_a", "QUEUED"))),
            ],
            vec![depends_on("wn_a", "wn_done")],
        )
    };

    let graph = ScriptedGraph::new(
        releasable(),
        vec![ApplyScript::LoseRace; MAX_REVISION_ATTEMPTS],
    );
    let dispatch = ScriptedDispatch::default();
    let refused = orchestrator(&f, graph.clone(), dispatch.clone())
        .tick(WORKSPACE)
        .await
        .expect_err("every race lost");
    assert!(
        matches!(
            &refused,
            OrchestrationError::RevisionConflict { current, .. }
                if *current == 10 + MAX_REVISION_ATTEMPTS as u64
        ),
        "{refused:?}"
    );
    assert_eq!(graph.applies(), MAX_REVISION_ATTEMPTS);
    assert_eq!(graph.status("wn_a"), "draft", "no release landed");
    assert!(
        dispatch.asked.lock().expect("asked").is_empty(),
        "nothing is dispatched from a graph the tick could not commit"
    );

    let graph = ScriptedGraph::new(releasable(), vec![ApplyScript::Refuse]);
    let refused = orchestrator(&f, graph.clone(), dispatch.clone())
        .tick(WORKSPACE)
        .await
        .expect_err("refused batch");
    assert!(
        matches!(refused, OrchestrationError::IllegalNodeTransition { .. }),
        "{refused:?}"
    );
    assert_eq!(
        graph.applies(),
        1,
        "a refusal that is not a race is not retried"
    );
    assert!(dispatch.asked.lock().expect("asked").is_empty());

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_cancellation_that_loses_the_graph_race_to_another_canceller_still_reports_its_runs() {
    let Some(f) = prepare("qa001_orch_cancel_race").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f).await;
    let graph = ScriptedGraph::new(
        snapshot(
            vec![node("wn_root", "in_progress", Some(run.clone()))],
            Vec::new(),
        ),
        vec![ApplyScript::LoseRaceToCanceller],
    );
    let outcome = orchestrator(&f, graph.clone(), ScriptedDispatch::default())
        .cancel_subtree(cancel_request("wn_root"))
        .await
        .expect("cancel");
    assert!(
        outcome.already_cancelled,
        "the graph work was done by the other canceller"
    );
    assert_eq!(
        outcome.runs_cancelled,
        vec![run.run_id.clone()],
        "the run this call stopped is still reported"
    );
    assert!(outcome.cancelled.is_empty());
    assert_eq!(run_status(&f, &run).await, RunStatus::Cancelled);
    assert_eq!(cancelled_events(&f).await, 1);
    assert_eq!(graph.status("wn_root"), "cancelled");

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn cancellation_out_of_retries_is_a_conflict_only_while_work_is_still_pending() {
    let Some(f) = prepare("qa001_orch_cancel_retries").await else {
        blocked_marker();
        return;
    };
    let subtree = || {
        snapshot(
            vec![
                node("wn_root", "in_progress", None),
                child("wn_kid", "ready", "wn_root"),
            ],
            Vec::new(),
        )
    };

    // Every attempt loses to a writer that changed something else: the change is still pending.
    let graph = ScriptedGraph::new(subtree(), vec![ApplyScript::LoseRace; MAX_CANCEL_ATTEMPTS]);
    let refused = orchestrator(&f, graph.clone(), ScriptedDispatch::default())
        .cancel_subtree(cancel_request("wn_root"))
        .await
        .expect_err("still pending after every retry");
    assert!(
        matches!(
            &refused,
            OrchestrationError::RevisionConflict { workspace_id, .. } if workspace_id == WORKSPACE
        ),
        "{refused:?}"
    );
    assert_eq!(graph.applies(), MAX_CANCEL_ATTEMPTS);
    assert_eq!(graph.status("wn_root"), "in_progress");

    // The last lost race was to a canceller that finished the same work: that is success.
    let mut script = vec![ApplyScript::LoseRace; MAX_CANCEL_ATTEMPTS - 1];
    script.push(ApplyScript::LoseRaceToCanceller);
    let graph = ScriptedGraph::new(subtree(), script);
    let outcome = orchestrator(&f, graph.clone(), ScriptedDispatch::default())
        .cancel_subtree(cancel_request("wn_root"))
        .await
        .expect("finished by another canceller");
    assert!(outcome.already_cancelled);
    assert!(outcome.runs_cancelled.is_empty());
    assert_eq!(graph.applies(), MAX_CANCEL_ATTEMPTS);
    assert_eq!(graph.status("wn_kid"), "cancelled");

    // A graph refusal that is not a race ends the cancellation with that refusal.
    let graph = ScriptedGraph::new(subtree(), vec![ApplyScript::Refuse]);
    let refused = orchestrator(&f, graph.clone(), ScriptedDispatch::default())
        .cancel_subtree(cancel_request("wn_root"))
        .await
        .expect_err("refused");
    assert!(
        matches!(refused, OrchestrationError::IllegalNodeTransition { .. }),
        "{refused:?}"
    );
    assert_eq!(graph.applies(), 1);

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_live_run_the_runtime_cannot_find_stops_the_cancellation_before_the_graph_moves() {
    let Some(f) = prepare("qa001_orch_cancel_missing").await else {
        blocked_marker();
        return;
    };
    let graph = ScriptedGraph::new(
        snapshot(
            vec![node(
                "wn_root",
                "in_progress",
                Some(run_ref(MISSING_RUN, "RUNNING")),
            )],
            Vec::new(),
        ),
        Vec::new(),
    );
    let refused = orchestrator(&f, graph.clone(), ScriptedDispatch::default())
        .cancel_subtree(cancel_request("wn_root"))
        .await
        .expect_err("unknown run");
    assert!(
        matches!(
            &refused,
            OrchestrationError::Runtime(RuntimeError::NotFound { entity: "run", id, .. })
                if id == MISSING_RUN
        ),
        "{refused:?}"
    );
    assert_eq!(
        graph.applies(),
        0,
        "the node is not cancelled over a run left running"
    );
    assert_eq!(graph.status("wn_root"), "in_progress");

    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn the_graph_port_refuses_an_unknown_status_and_a_malformed_tenant() {
    let Some(f) = prepare("qa001_orch_graph_port").await else {
        blocked_marker();
        return;
    };
    let port = GraphWorkGraph::new(f.pool.clone(), TENANT);
    let before = port.revision(WORKSPACE).await.expect("revision");
    let snapshot = port.snapshot(WORKSPACE).await.expect("snapshot");
    assert_eq!(snapshot.revision, before);
    let seeded = snapshot.node(WORK_NODE).expect("seeded node");

    let refused = port
        .apply(ReleaseBatch {
            workspace_id: WORKSPACE.to_string(),
            base_revision: before,
            transitions: vec![NodeTransition {
                node_id: WORK_NODE.to_string(),
                expected_revision: seeded.revision,
                to: "teleported".to_string(),
                reason: "test".to_string(),
            }],
        })
        .await
        .expect_err("unknown status");
    assert!(
        matches!(&refused, OrchestrationError::IllegalNodeTransition { node_id, from, .. }
            if node_id == WORK_NODE && from == "teleported"),
        "{refused:?}"
    );
    assert_eq!(port.revision(WORKSPACE).await.expect("revision"), before);
    assert_eq!(
        port.snapshot(WORKSPACE)
            .await
            .expect("snapshot")
            .node(WORK_NODE)
            .expect("node")
            .status,
        seeded.status,
        "nothing was applied"
    );

    let malformed = GraphWorkGraph::new(f.pool.clone(), "tenant-without-prefix");
    let refused = malformed.revision(WORKSPACE).await.expect_err("malformed");
    assert!(
        matches!(
            refused,
            OrchestrationError::PortUnavailable {
                owner: GRAPH_TRANSACTION_OWNER,
                ..
            }
        ),
        "{refused:?}"
    );

    drop_pool(&f.pool, &f.name).await;
}
