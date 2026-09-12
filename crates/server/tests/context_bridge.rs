//! INT-005 acceptance: the runtime's side of the ContextProjection boundary.
//!
//! The suites drive the shipped bridge over real durable state: a projection the plane labelled is
//! accepted and its id is recorded on the turn the run executed, and a projection the plane could
//! not label is refused before the run ever sees it, leaving no trace.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::context_bridge::{
    validate_projection, ContextBridgeError, ContextBridgePort, ProjectionRequest,
    ProjectionSegment, ReceivedProjection, UnavailableContextBridge,
};
use quansio_server::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, RunStatus, RunTriggerKind,
    RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0RR015";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0RR015";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0RR015";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0RR015";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0RR015";
const PROJECTION: &str = "ctx_01J8Z3K6F1N8VQ2X5W9Y0RR015";
const SNAPSHOT: &str = "snap_canonical_1";

/// A model that proposes nothing, so the turn completes without tool work.
#[derive(Clone, Default)]
struct QuietModel;

#[async_trait]
impl ModelProposalSource for QuietModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        Ok(ModelProposal {
            assistant_text: Some("done".to_string()),
            ..ModelProposal::default()
        })
    }
}

/// A plane that returns a scripted projection.
#[derive(Clone)]
struct ScriptedPlane {
    projection: ReceivedProjection,
    requests: Arc<Mutex<Vec<ProjectionRequest>>>,
}

#[async_trait]
impl ContextBridgePort for ScriptedPlane {
    async fn project(
        &self,
        request: ProjectionRequest,
    ) -> Result<ReceivedProjection, ContextBridgeError> {
        self.requests.lock().expect("requests").push(request);
        Ok(self.projection.clone())
    }
}

fn segment(id: &str, trust: &str) -> ProjectionSegment {
    ProjectionSegment {
        segment_id: id.to_string(),
        source: "artifact://plan".to_string(),
        trust_level: trust.to_string(),
        tokens: 5,
    }
}

fn projection(segments: Vec<ProjectionSegment>) -> ReceivedProjection {
    ReceivedProjection {
        projection_id: PROJECTION.to_string(),
        snapshot_id: SNAPSHOT.to_string(),
        segments,
        tokens_used: 10,
        degraded: false,
    }
}

struct Fixture {
    name: String,
    pool: PgPool,
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
        .expect("context");
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
    let mut generator = UlidGenerator::new();
    let identity = RuntimeIdentity::system(
        TENANT,
        "context-bridge-test",
        CorrelationId::generate(&mut generator),
    );
    let engine = RuntimeEngine::new(pool.clone(), identity)
        .expect("engine")
        .with_model_source(Arc::new(QuietModel));
    Some(Fixture { name, pool, engine })
}

async fn start_run(fixture: &Fixture) -> quansio_server::runtime::state_machine::Run {
    let run = fixture
        .engine
        .create_run(NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
            CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
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

async fn recorded_projections(fixture: &Fixture) -> Vec<Option<String>> {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    let rows: Vec<Option<String>> =
        sqlx::query_scalar("SELECT context_projection_id FROM turns WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_all(&mut *tx)
            .await
            .expect("turns");
    tx.commit().await.expect("commit");
    rows
}

async fn turn_count(fixture: &Fixture) -> i64 {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    let count = sqlx::query_scalar("SELECT COUNT(*) FROM turns WHERE tenant_id = $1")
        .bind(TENANT)
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    count
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

// ---------------------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_labelled_projection_is_accepted_and_its_id_is_recorded_on_the_turn() {
    let Some(fixture) = prepare("int005_record").await else {
        blocked_marker();
        return;
    };
    let plane = ScriptedPlane {
        projection: projection(vec![
            segment("seg_system", "trusted_system"),
            segment("seg_tool", "untrusted_external"),
        ]),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let run = start_run(&fixture).await;

    // The run asks the plane for its projection, and the boundary check accepts it.
    let received = plane
        .project(ProjectionRequest {
            run_id: run.id.to_string(),
            workspace_id: WORKSPACE.to_string(),
            program_json: "{\"channels\":[{\"channel\":\"exact\",\"value\":\"plan\"}]}".to_string(),
            snapshot_id: SNAPSHOT.to_string(),
            token_budget: 1_000,
        })
        .await
        .expect("the plane answers");
    assert_eq!(
        plane.requests.lock().expect("requests").len(),
        1,
        "the plane was asked once"
    );
    let validated = validate_projection(received, Some(SNAPSHOT)).expect("labelled projection");

    // The validated id is what the turn records.
    let mut input = TurnInput::new(RunTriggerKind::Manual);
    input.context_projection_id = Some(validated.projection_id.clone());
    fixture
        .engine
        .run_turn(&run.id, run.generation, input)
        .await
        .expect("turn");

    assert_eq!(
        recorded_projections(&fixture).await,
        vec![Some(PROJECTION.to_string())],
        "the turn records the projection it was given, so context is traceable"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_projection_the_plane_could_not_label_is_refused_before_the_run_sees_it() {
    let Some(fixture) = prepare("int005_refuse").await else {
        blocked_marker();
        return;
    };
    let plane = ScriptedPlane {
        // The plane returned a segment with no trust level: a bug the runtime must not absorb.
        projection: projection(vec![segment("seg_unlabelled", "")]),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let run = start_run(&fixture).await;
    let before = turn_count(&fixture).await;

    let received = plane
        .project(ProjectionRequest {
            run_id: run.id.to_string(),
            workspace_id: WORKSPACE.to_string(),
            program_json: "{}".to_string(),
            snapshot_id: SNAPSHOT.to_string(),
            token_budget: 100,
        })
        .await
        .expect("the plane answers");
    let error = validate_projection(received, Some(SNAPSHOT)).expect_err("unlabelled segment");
    assert_eq!(error.code(), "VALIDATION_SCHEMA");

    // Nothing was started: the refusal happened before the run could see the projection.
    assert_eq!(turn_count(&fixture).await, before, "no turn was started");
    assert_eq!(
        fixture
            .engine
            .store()
            .load_run(&run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Running,
        "the run is untouched by the refusal"
    );

    // And a stale projection is refused the same way, before any turn.
    let stale = validate_projection(
        projection(vec![segment("seg_system", "trusted_system")]),
        Some("snap_canonical_2"),
    )
    .expect_err("stale snapshot");
    assert_eq!(stale.code(), "CONFLICT_STATE");
    assert_eq!(turn_count(&fixture).await, before);
    finish(fixture).await;
}

#[tokio::test]
async fn an_unwired_bridge_fails_closed_rather_than_fabricating_context() {
    let error = UnavailableContextBridge
        .project(ProjectionRequest {
            run_id: "run_1".to_string(),
            workspace_id: "ws_1".to_string(),
            program_json: "{}".to_string(),
            snapshot_id: SNAPSHOT.to_string(),
            token_budget: 10,
        })
        .await
        .expect_err("unwired");
    assert!(matches!(error, ContextBridgeError::Unavailable { .. }));
    assert_eq!(error.code(), "INTERNAL");
}
