//! RUN-008 acceptance: a model's completion claim can never certify itself.
//!
//! These tests drive the real turn loop with the real contract verifier: a stub model proposes a
//! completion claim, the engine records a Verify step, the verifier evaluates the WorkNode's
//! CompletionContract against real artifact metadata and real EffectRecords, and the run reaches
//! `SUCCEEDED` only when every required check passes.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::state_machine::{
    CompletionClaim, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, RunStatus,
    RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput, TurnOutcome,
};
use quansio_server::runtime::verification::{
    ContractVerifier, SemanticVerdict, SemanticVerificationRequest, SemanticVerifierPort,
    VerificationError,
};
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0RR008";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0RR008";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0RR008";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0RR008";

/// A model that proposes scripted turns and then stops.
#[derive(Clone, Default)]
struct StubModel {
    proposals: Arc<Mutex<VecDeque<ModelProposal>>>,
}

impl StubModel {
    fn claiming(summary: &str) -> Self {
        Self {
            proposals: Arc::new(Mutex::new(
                vec![ModelProposal {
                    completion_claim: Some(CompletionClaim {
                        claim_id: "claim_1".to_string(),
                        summary: summary.to_string(),
                        evidence_ids: Vec::new(),
                    }),
                    ..ModelProposal::default()
                }]
                .into(),
            )),
        }
    }
}

#[async_trait]
impl ModelProposalSource for StubModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        Ok(self
            .proposals
            .lock()
            .expect("proposals")
            .pop_front()
            .unwrap_or_default())
    }
}

/// A semantic verifier with a fixed verdict.
#[derive(Clone, Copy)]
struct FixedSemantic {
    agrees: bool,
}

#[async_trait]
impl SemanticVerifierPort for FixedSemantic {
    async fn verify(
        &self,
        _request: SemanticVerificationRequest,
    ) -> Result<SemanticVerdict, VerificationError> {
        Ok(SemanticVerdict {
            agrees: self.agrees,
            critique: if self.agrees {
                "the deliverable matches the rubric".to_string()
            } else {
                "the report summarises work that the evidence does not show".to_string()
            },
            model: "independent-verifier".to_string(),
        })
    }
}

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let mut generator = UlidGenerator::new();
    let identity = RuntimeIdentity::system(
        TENANT,
        "verification-test",
        CorrelationId::generate(&mut generator),
    );
    Some(Fixture {
        name,
        pool,
        identity,
    })
}

/// Set the seed node's CompletionContract.
async fn set_contract(fixture: &Fixture, contract: Value) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE work_nodes SET completion_contract = $1 WHERE id = $2 AND tenant_id = $3")
        .bind(&contract)
        .bind(WORK_NODE)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("contract");
    tx.commit().await.expect("commit");
}

/// Create an artifact of `role` with `versions` versions, the way the artifact path would.
async fn create_artifact(fixture: &Fixture, role: &str, versions: u32) -> String {
    let mut generator = UlidGenerator::new();
    let artifact = CanonicalId::generate(Prefix::Artifact, &mut generator).to_string();
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO artifacts (id, tenant_id, workspace_id, kind, title, role, origin) \
         VALUES ($1, $2, $3, 'document', 'deliverable', $4, $5)",
    )
    .bind(&artifact)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(role)
    .bind(json!({"kind": "agent", "run_id": null}))
    .execute(&mut *tx)
    .await
    .expect("artifact");
    for version in 0..versions {
        let version_id = CanonicalId::generate(Prefix::ArtifactVersion, &mut generator).to_string();
        sqlx::query(
            "INSERT INTO artifact_versions (id, tenant_id, artifact_id, seq, content_digest, \
             size_bytes, media_type, object_key) \
             VALUES ($1, $2, $3, $4, $5, 1, 'text/plain', $6)",
        )
        .bind(&version_id)
        .bind(TENANT)
        .bind(&artifact)
        .bind(i32::try_from(version + 1).unwrap_or(1))
        .bind(format!("sha256:{version}"))
        .bind(format!("objects/{version_id}"))
        .execute(&mut *tx)
        .await
        .expect("artifact version");
    }
    tx.commit().await.expect("commit");
    artifact
}

/// Run one turn with the given verifier and return (outcome, run status) after a second empty
/// turn has let the loop finish.
async fn start_run(
    fixture: &Fixture,
    model: StubModel,
    verifier: ContractVerifier,
) -> (RuntimeEngine, quansio_server::runtime::state_machine::Run) {
    let engine = RuntimeEngine::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("engine")
        .with_model_source(Arc::new(model))
        .with_verification(Arc::new(verifier));
    let run = engine
        .create_run(NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
            CanonicalId::parse_typed(WORK_AGENT, Prefix::AgentThread).expect("agent"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let run = engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    let run = engine.start(&run.id, run.generation).await.expect("start");
    (engine, run)
}

/// Run the turn whose proposal carries the completion claim.
async fn finish_turn(
    engine: &RuntimeEngine,
    run: &quansio_server::runtime::state_machine::Run,
) -> (TurnOutcome, RunStatus) {
    let outcome = engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let after = engine.store().load_run(&run.id).await.expect("run");
    (outcome, after.status)
}

async fn claim_and_verify(
    fixture: &Fixture,
    model: StubModel,
    verifier: ContractVerifier,
) -> (TurnOutcome, RunStatus) {
    let (engine, run) = start_run(fixture, model, verifier).await;
    finish_turn(&engine, &run).await
}

/// The verifier wired to the fixture's stores, with an optional semantic verifier.
fn verifier(
    fixture: &Fixture,
    semantic: Option<Arc<dyn SemanticVerifierPort>>,
) -> ContractVerifier {
    let verifier =
        ContractVerifier::new(fixture.pool.clone(), fixture.identity.clone()).expect("verifier");
    match semantic {
        Some(port) => verifier.with_semantic_verifier(port),
        None => verifier,
    }
}

const WORK_AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0RR008";

async fn seed_agent_thread(fixture: &Fixture) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(WORK_AGENT)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    tx.commit().await.expect("commit");
}

// ---------------------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_model_claim_alone_cannot_succeed_a_run_without_its_artifact() {
    let Some(fixture) = prepare("run008_false_done").await else {
        blocked_marker();
        return;
    };
    seed_agent_thread(&fixture).await;
    set_contract(
        &fixture,
        json!({"deterministic_checks": [
            {"kind": "artifact_exists", "artifact_role": "report", "min_versions": 1}
        ]}),
    )
    .await;

    let (outcome, status) = claim_and_verify(
        &fixture,
        StubModel::claiming("the report is written"),
        verifier(&fixture, None),
    )
    .await;

    assert!(
        !matches!(outcome, TurnOutcome::Succeeded { .. }),
        "a claim without its artifact must not succeed the run: {outcome:?}"
    );
    assert_ne!(
        status,
        RunStatus::Succeeded,
        "the run stays incomplete until verification passes"
    );
    let steps = steps_of_last_run(&fixture).await;
    let verify = steps
        .iter()
        .find(|(kind, _)| kind == "verify")
        .expect("a verify step was recorded");
    assert_eq!(verify.1, "failed", "the verification step failed");
    let feedback = verify_feedback(&fixture).await;
    assert!(
        feedback.contains("artifact_exists") && feedback.contains("report"),
        "the feedback names the failed check and its role: {feedback}"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn the_same_claim_verifies_once_the_contract_is_satisfied() {
    let Some(fixture) = prepare("run008_missing_artifact").await else {
        blocked_marker();
        return;
    };
    seed_agent_thread(&fixture).await;
    set_contract(
        &fixture,
        json!({"deterministic_checks": [
            {"kind": "artifact_exists", "artifact_role": "report", "min_versions": 2}
        ]}),
    )
    .await;

    // One version is not enough for a contract that requires two.
    create_artifact(&fixture, "report", 1).await;
    let (outcome, _) = claim_and_verify(
        &fixture,
        StubModel::claiming("done"),
        verifier(&fixture, None),
    )
    .await;
    assert!(!matches!(outcome, TurnOutcome::Succeeded { .. }));
    let feedback = verify_feedback(&fixture).await;
    assert!(
        feedback.contains("has 1 version(s)") || feedback.contains("the best candidate has 1"),
        "feedback reports what was found: {feedback}"
    );

    // A second version satisfies it, and a fresh run's claim now verifies through the engine.
    create_artifact(&fixture, "report", 2).await;
    let (outcome, status) = claim_and_verify(
        &fixture,
        StubModel::claiming("done"),
        verifier(&fixture, None),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Succeeded { .. }),
        "a satisfied contract succeeds the run: {outcome:?}"
    );
    assert_eq!(status, RunStatus::Succeeded);
    finish(fixture).await;
}

#[tokio::test]
async fn an_unimplemented_check_kind_fails_closed_and_names_its_owner() {
    let Some(fixture) = prepare("run008_unimplemented").await else {
        blocked_marker();
        return;
    };
    seed_agent_thread(&fixture).await;
    set_contract(
        &fixture,
        json!({"deterministic_checks": [
            {"kind": "test_command", "command": "cargo test -p x", "expect_exit": 0}
        ]}),
    )
    .await;

    let (outcome, status) = claim_and_verify(
        &fixture,
        StubModel::claiming("tests pass"),
        verifier(&fixture, None),
    )
    .await;
    assert!(!matches!(outcome, TurnOutcome::Succeeded { .. }));
    assert_ne!(status, RunStatus::Succeeded);
    let feedback = verify_feedback(&fixture).await;
    assert!(
        feedback.contains("test_command") && feedback.contains("EXEC-006"),
        "an unimplemented check is never skipped: {feedback}"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn a_semantic_verifier_disagreement_keeps_work_incomplete() {
    let Some(fixture) = prepare("run008_semantic").await else {
        blocked_marker();
        return;
    };
    seed_agent_thread(&fixture).await;
    set_contract(
        &fixture,
        json!({
            "deterministic_checks": [
                {"kind": "artifact_exists", "artifact_role": "report"}
            ],
            "semantic_verification": {"required": true, "rubric_id": "r1", "independent_model": true}
        }),
    )
    .await;
    create_artifact(&fixture, "report", 1).await;

    let (outcome, status) = claim_and_verify(
        &fixture,
        StubModel::claiming("done"),
        verifier(&fixture, Some(Arc::new(FixedSemantic { agrees: false }))),
    )
    .await;
    assert!(
        !matches!(outcome, TurnOutcome::Succeeded { .. }),
        "a disagreeing verifier must not let the work complete"
    );
    assert_ne!(status, RunStatus::Succeeded);
    let feedback = verify_feedback(&fixture).await;
    assert!(
        feedback.contains("disagrees") && feedback.contains("independent-verifier"),
        "the critique is the feedback: {feedback}"
    );

    // The same claim with an agreeing verifier completes.
    let fixture2 = prepare("run008_semantic_ok").await;
    let Some(fixture2) = fixture2 else {
        finish(fixture).await;
        return;
    };
    seed_agent_thread(&fixture2).await;
    set_contract(
        &fixture2,
        json!({
            "deterministic_checks": [
                {"kind": "artifact_exists", "artifact_role": "report"}
            ],
            "semantic_verification": {"required": true, "rubric_id": "r1", "independent_model": true}
        }),
    )
    .await;
    create_artifact(&fixture2, "report", 1).await;
    let (outcome, status) = claim_and_verify(
        &fixture2,
        StubModel::claiming("done"),
        verifier(&fixture2, Some(Arc::new(FixedSemantic { agrees: true }))),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Succeeded { .. }),
        "an agreeing verifier completes the run: {outcome:?}"
    );
    assert_eq!(status, RunStatus::Succeeded);
    finish(fixture).await;
    finish(fixture2).await;
}

#[tokio::test]
async fn a_contract_that_binds_nothing_cannot_certify_completion() {
    let Some(fixture) = prepare("run008_empty").await else {
        blocked_marker();
        return;
    };
    seed_agent_thread(&fixture).await;
    // The seeded WorkNode ships with an empty CompletionContract.
    let (outcome, status) = claim_and_verify(
        &fixture,
        StubModel::claiming("all done"),
        verifier(&fixture, None),
    )
    .await;
    assert!(!matches!(outcome, TurnOutcome::Succeeded { .. }));
    assert_ne!(status, RunStatus::Succeeded);
    let feedback = verify_feedback(&fixture).await;
    assert!(
        feedback.contains("binds no check"),
        "an empty contract certifies nothing: {feedback}"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn an_unsettled_effect_keeps_the_run_incomplete() {
    let Some(fixture) = prepare("run008_effects").await else {
        blocked_marker();
        return;
    };
    seed_agent_thread(&fixture).await;
    set_contract(
        &fixture,
        json!({"deterministic_checks": [
            {"kind": "effects_settled", "effect_classes": []}
        ]}),
    )
    .await;
    // The run exists first; its effect is then unsettled, so nothing may be certified.
    let (engine, run) = start_run(
        &fixture,
        StubModel::claiming("done"),
        verifier(&fixture, None),
    )
    .await;
    let unsettled = insert_effect(&fixture, &run.id.to_string(), "OUTCOME_UNKNOWN").await;
    let (outcome, status) = finish_turn(&engine, &run).await;
    assert!(!matches!(outcome, TurnOutcome::Succeeded { .. }));
    assert_ne!(status, RunStatus::Succeeded);
    let feedback = verify_feedback(&fixture).await;
    assert!(
        feedback.contains("effects_settled") && feedback.contains("OUTCOME_UNKNOWN"),
        "an unsettled effect is reported by class and status: {feedback}"
    );

    // Settle it; a fresh run's claim then verifies.
    settle_effect(&fixture, &unsettled).await;
    let (outcome, status) = claim_and_verify(
        &fixture,
        StubModel::claiming("done"),
        verifier(&fixture, None),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Succeeded { .. }),
        "settled effects satisfy the contract: {outcome:?}"
    );
    assert_eq!(status, RunStatus::Succeeded);
    finish(fixture).await;
}

/// The steps of the most recently created run.
async fn steps_of_last_run(fixture: &Fixture) -> Vec<(String, String)> {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT s.kind, s.status FROM steps s \
         JOIN turns t ON t.id = s.turn_id \
         WHERE t.run_id = (SELECT id FROM runs WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 1) \
         ORDER BY s.seq",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("steps");
    tx.commit().await.expect("commit");
    rows
}

/// The feedback recorded on the last failed verify step.
async fn verify_feedback(fixture: &Fixture) -> String {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let payload: Option<Value> = sqlx::query_scalar(
        "SELECT a.error FROM attempts a JOIN steps s ON s.id = a.step_id \
         WHERE s.kind = 'verify' AND s.status = 'failed' AND a.tenant_id = $1 \
         ORDER BY a.created_at DESC LIMIT 1",
    )
    .bind(TENANT)
    .fetch_optional(&mut *tx)
    .await
    .expect("feedback");
    tx.commit().await.expect("commit");
    payload
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<no failed verify step>".to_string())
}

/// Insert an effect record directly in the given status for the seeded run.
async fn insert_effect(fixture: &Fixture, run_id: &str, status: &str) -> String {
    let mut generator = UlidGenerator::new();
    let id = CanonicalId::generate(Prefix::EffectRecord, &mut generator).to_string();
    let run = run_id.to_string();
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, effect_class, tier, \
         resource, params_digest, idempotency_key, capability_projection_id, status, target_kind, \
         target_id, generation) \
         VALUES ($1, $2, $3, $4, 'message.send', 1, $5, $9, $6, $7, $8, 'server', 'test', 1)",
    )
    .bind(&id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(&run)
    .bind(json!({"kind": "domain", "selector": "example.com"}))
    .bind(format!("key-{id}"))
    .bind(format!("cap_{id}"))
    .bind(status)
    .bind("0".repeat(64))
    .execute(&mut *tx)
    .await
    .expect("effect");
    tx.commit().await.expect("commit");
    id
}

async fn settle_effect(fixture: &Fixture, effect_id: &str) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "UPDATE effect_records SET status = 'SETTLED_SUCCESS' WHERE id = $1 AND tenant_id = $2",
    )
    .bind(effect_id)
    .bind(TENANT)
    .execute(&mut *tx)
    .await
    .expect("settle");
    tx.commit().await.expect("commit");
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}
