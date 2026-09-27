//! RUN-008 coverage: CompletionContract parsing refusals, the owners unimplemented checks
//! name, and the verifier's fail-closed refusals (DOMAIN.md §4.4).
//!
//! The DB tests call the real `ContractVerifier` against a real run, WorkNode contract and
//! Effect Ledger; nothing is stubbed.
//!
//! Real boundary: none. The DB suites need PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`;
//! absent, they print the `BLOCKED_EXTERNAL` marker and return.

use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::state_machine::{
    CompletionClaim, NewRun, Run, RunTriggerKind, RuntimeEngine, RuntimeIdentity,
    VerificationContext, VerificationOutcome, VerificationPort,
};
use quansio_server::runtime::verification::{
    run_check, CheckScope, CompletionContract, ContractVerifier, DeterministicCheck,
    CITATION_CHECK_OWNER, COMMAND_CHECK_OWNER, PREDICATE_OWNER, SEMANTIC_VERIFIER_OWNER,
};
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA0V1";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA0V1";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0V1";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0V1";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0QA0V1";

#[test]
fn a_contract_with_mistyped_sections_is_malformed_rather_than_empty() {
    for (contract, needle) in [
        (json!("finish the report"), "must be a JSON object"),
        (
            json!({ "deterministic_checks": { "kind": "artifact_exists" } }),
            "deterministic_checks must be an array",
        ),
        (
            json!({ "semantic_verification": ["required"] }),
            "semantic_verification must be an object",
        ),
        (
            json!({ "deterministic_checks": ["artifact_exists"] }),
            "a deterministic check must be an object",
        ),
    ] {
        let error = CompletionContract::parse(&contract).expect_err("malformed");
        assert_eq!(error.code(), "VALIDATION_SCHEMA");
        assert!(error.to_string().contains(needle), "{needle}: {error}");
    }
}

#[test]
fn every_check_kind_parses_and_the_unimplemented_ones_name_their_owners_in_order() {
    let contract = CompletionContract::parse(&json!({
        "deterministic_checks": [
            { "kind": "artifact_exists", "artifact_role": "report" },
            { "kind": "citations_valid", "min_coverage": 0.9 },
            { "kind": "artifact_valid", "artifact_role": "report" },
            { "kind": "test_command", "command": "cargo test" },
            { "kind": "effects_settled" },
            { "kind": "assertion", "expr": "no_open_questions" }
        ]
    }))
    .expect("contract");
    let kinds: Vec<&str> = contract
        .deterministic_checks
        .iter()
        .map(DeterministicCheck::kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            "artifact_exists",
            "citations_valid",
            "artifact_valid",
            "test_command",
            "effects_settled",
            "assertion"
        ]
    );
    assert_eq!(
        contract.unimplementable_checks(),
        vec![
            ("citations_valid", CITATION_CHECK_OWNER),
            ("test_command", COMMAND_CHECK_OWNER),
            ("assertion", PREDICATE_OWNER),
        ],
        "only checks the runtime cannot run name an owner"
    );
    assert!(!contract.binds_nothing());
}

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
    run: Run,
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
        "verification-checks-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let run = RuntimeEngine::new(pool.clone(), identity.clone())
        .expect("engine")
        .create_run(NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
            CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    Some(Fixture {
        name,
        pool,
        identity,
        run,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

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

/// Ask the verifier to certify a model's claim for the fixture's run.
async fn verify(fixture: &Fixture, verifier: &ContractVerifier) -> VerificationOutcome {
    verifier
        .verify(
            CompletionClaim {
                claim_id: "claim_1".to_string(),
                summary: "the report is written and reviewed".to_string(),
                evidence_ids: vec!["evd_model_says_so".to_string()],
            },
            VerificationContext {
                run_id: fixture.run.id.to_string(),
                turn_id: "trn_01J8Z3K6F1N8VQ2X5W9Y0QA0V1".to_string(),
            },
        )
        .await
        .expect("verify")
}

#[tokio::test]
async fn a_contract_requiring_a_semantic_verdict_is_refused_when_no_verifier_is_installed() {
    let Some(fixture) = prepare("qa001_v_semantic").await else {
        blocked_marker();
        return;
    };
    set_contract(
        &fixture,
        json!({ "semantic_verification": { "required": true, "rubric_id": "rubric_report" } }),
    )
    .await;
    let verifier =
        ContractVerifier::new(fixture.pool.clone(), fixture.identity.clone()).expect("verifier");

    let outcome = verify(&fixture, &verifier).await;
    let VerificationOutcome::Rejected { feedback } = outcome else {
        panic!("a missing semantic verifier never certifies: {outcome:?}");
    };
    assert!(feedback.contains("unavailable"), "{feedback}");
    assert!(
        feedback.contains(SEMANTIC_VERIFIER_OWNER),
        "the refusal names the verifier's owner: {feedback}"
    );
    finish(fixture).await;
}

#[tokio::test]
async fn unrunnable_checks_fail_closed_with_their_owners_alongside_a_missing_deliverable() {
    let Some(fixture) = prepare("qa001_v_owners").await else {
        blocked_marker();
        return;
    };
    set_contract(
        &fixture,
        json!({
            "deterministic_checks": [
                { "kind": "assertion", "expr": "no_open_questions", "args": { "strict": true } },
                { "kind": "citations_valid", "min_coverage": 0.8 },
                { "kind": "artifact_valid", "artifact_role": "report" }
            ]
        }),
    )
    .await;
    let verifier =
        ContractVerifier::new(fixture.pool.clone(), fixture.identity.clone()).expect("verifier");

    let outcome = verify(&fixture, &verifier).await;
    let VerificationOutcome::Rejected { feedback } = outcome else {
        panic!("unrunnable checks never pass: {outcome:?}");
    };
    for needle in [
        "no_open_questions",
        PREDICATE_OWNER,
        "0.8",
        CITATION_CHECK_OWNER,
        "no artifact with role \"report\" exists",
    ] {
        assert!(
            feedback.contains(needle),
            "the feedback lists every failure ({needle}): {feedback}"
        );
    }
    finish(fixture).await;
}

#[tokio::test]
async fn an_effects_check_for_a_malformed_run_id_fails_closed_instead_of_passing_vacuously() {
    let Some(fixture) = prepare("qa001_v_effects").await else {
        blocked_marker();
        return;
    };
    let verifier =
        ContractVerifier::new(fixture.pool.clone(), fixture.identity.clone()).expect("verifier");
    let check = DeterministicCheck::EffectsSettled {
        effect_classes: Vec::new(),
    };
    let scope = |run_id| CheckScope {
        pool: &fixture.pool,
        tenant_id: TENANT,
        workspace_id: WORKSPACE,
        run_id,
        ledger: verifier.ledger(),
    };

    // A run with no effects is settled...
    let run_id = fixture.run.id.to_string();
    let settled = run_check(&check, &scope(&run_id)).await.expect("check");
    assert!(settled.passed, "{}", settled.detail);

    // ...but an id that is not a run is refused, even though it has no effects either.
    let error = run_check(&check, &scope("wn_01J8Z3K6F1N8VQ2X5W9Y0QA0V1"))
        .await
        .expect_err("not a run id");
    assert_eq!(error.code(), "NOT_FOUND");
    assert!(
        error.to_string().contains("wn_01J8Z3K6F1N8VQ2X5W9Y0QA0V1"),
        "{error}"
    );
    finish(fixture).await;
}
