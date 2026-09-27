//! RUN-011 coverage: the durable question protocol's refusals, answer validation per kind,
//! and expiry (DOMAIN.md §3.4, §5.6).
//!
//! Everything is real: the question rows, their RuntimeEvents and the run transitions an
//! answer or an expiry drives. The model is a conformance stub that asks one question.
//!
//! Real boundary: none. The suite needs PostgreSQL through `QUANSIO_TEST_POSTGRES_URL`;
//! absent, every test prints the `BLOCKED_EXTERNAL` marker and returns.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_server::control::schema;
use quansio_server::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, ProposedQuestion, Run, RunStatus,
    RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput, TurnOutcome,
};
use quansio_server::runtime::turn_loop::questions::validate_answer;
use quansio_server::runtime::turn_loop::{
    NewQuestion, QuestionAnswer, QuestionRecord, QuestionService, QuestionStatus,
};
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA0Q1";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA0Q1";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0Q1";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0Q1";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0QA0Q1";

/// A model that asks one question, then answers in plain text.
#[derive(Clone, Default)]
struct AskingModel {
    asked: Arc<Mutex<bool>>,
}

#[async_trait]
impl ModelProposalSource for AskingModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let mut asked = self.asked.lock().expect("asked");
        if *asked {
            return Ok(ModelProposal {
                assistant_text: Some("done".to_string()),
                ..ModelProposal::default()
            });
        }
        *asked = true;
        Ok(ModelProposal {
            question: Some(ProposedQuestion {
                question_id: "question_1".to_string(),
                prompt: "Deploy to production?".to_string(),
                kind: "confirm".to_string(),
                options: Vec::new(),
                required: true,
            }),
            ..ModelProposal::default()
        })
    }
}

struct Fixture {
    name: String,
    pool: PgPool,
    questions: QuestionService,
    engine: RuntimeEngine,
}

/// A scratch tenant whose policy sets `question_default_ttl` to `ttl_seconds`.
async fn prepare(prefix: &str, ttl_seconds: i64) -> Option<Fixture> {
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
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules, \
         question_default_ttl_seconds) VALUES ($1, $2, NULL, 'tenant', '[]'::jsonb, $3)",
    )
    .bind("pol_01J8Z3K6F1N8VQ2X5W9Y0QA0Q1")
    .bind(TENANT)
    .bind(ttl_seconds)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");

    let identity = RuntimeIdentity::system(
        TENANT,
        "questions-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let questions = QuestionService::new(pool.clone(), identity.clone()).expect("questions");
    let engine = RuntimeEngine::new(pool.clone(), identity.clone())
        .expect("engine")
        .with_model_source(Arc::new(AskingModel::default()))
        .with_question_port(Arc::new(questions.clone()));
    Some(Fixture {
        name,
        pool,
        questions,
        engine,
    })
}

async fn finish(fixture: Fixture) {
    drop_pool(&fixture.pool, &fixture.name).await;
}

/// A started run of the seed work node.
async fn running(fixture: &Fixture) -> Run {
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

fn question(run: &Run, kind: &str, prompt: &str, options: &[&str]) -> NewQuestion {
    NewQuestion {
        run_id: run.id.to_string(),
        workspace_id: WORKSPACE.to_string(),
        step_id: None,
        thread_id: None,
        kind: kind.to_string(),
        prompt: prompt.to_string(),
        options: options.iter().map(|option| (*option).to_string()).collect(),
        required: true,
        ttl_seconds: None,
        generation: Some(run.generation),
    }
}

fn answer(value: Value) -> QuestionAnswer {
    QuestionAnswer {
        user_id: USER.to_string(),
        value,
    }
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

#[tokio::test]
async fn a_malformed_question_is_refused_and_writes_nothing() {
    let Some(fixture) = prepare("qa001_q_create", 3_600).await else {
        blocked_marker();
        return;
    };
    assert_eq!(fixture.questions.identity().tenant_id, TENANT);
    let run = running(&fixture).await;

    for (new, why) in [
        (question(&run, "ranking", "Rank these", &["a"]), "kind"),
        (
            question(&run, "single_choice", "Which one?", &[]),
            "choices",
        ),
        (
            question(&run, "multi_choice", "Which ones?", &[]),
            "choices",
        ),
        (question(&run, "free_text", "   ", &[]), "prompt"),
    ] {
        let refused = fixture
            .questions
            .create(new)
            .await
            .expect_err("a malformed question is refused");
        assert_eq!(refused.code(), "VALIDATION_SCHEMA");
        assert!(
            refused.to_string().contains(why),
            "the refusal names the {why}: {refused}"
        );
    }
    assert!(
        fixture
            .questions
            .for_run(&run.id.to_string())
            .await
            .expect("list")
            .is_empty(),
        "no refused question was written"
    );
    assert!(events_of_type(&fixture.pool, "question.asked")
        .await
        .is_empty());

    let asked = fixture
        .questions
        .create(question(&run, "free_text", "What is the budget?", &[]))
        .await
        .expect("create");
    let listed = fixture
        .questions
        .for_run(&run.id.to_string())
        .await
        .expect("list");
    assert_eq!(
        listed,
        vec![asked.clone()],
        "the durable row is the one returned"
    );
    assert_eq!(
        asked.expires_at, None,
        "no ttl leaves it open until answered"
    );

    let missing = fixture
        .questions
        .load("q_01J8Z3K6F1N8VQ2X5W9Y0QA0Q1")
        .await
        .expect_err("an unknown question");
    assert_eq!(missing.code(), "NOT_FOUND");
    finish(fixture).await;
}

#[tokio::test]
async fn each_question_kind_accepts_only_answers_of_its_own_shape() {
    let Some(fixture) = prepare("qa001_q_answers", 3_600).await else {
        blocked_marker();
        return;
    };
    let run = running(&fixture).await;
    let free = fixture
        .questions
        .create(question(&run, "free_text", "Anything else?", &[]))
        .await
        .expect("free text");
    let multi = fixture
        .questions
        .create(question(
            &run,
            "multi_choice",
            "Which regions?",
            &["eu-west", "us-east", "ap-south"],
        ))
        .await
        .expect("multi choice");

    for (id, value, why) in [
        (&free.id, json!(42), "must be a string"),
        (&multi.id, json!("eu-west"), "must be an array"),
        (&multi.id, json!(["eu-west", 7]), "must be strings"),
        (&multi.id, json!(["eu-west", "mars"]), "not one of"),
    ] {
        let refused = fixture
            .questions
            .answer(id, answer(value))
            .await
            .expect_err("a mis-shaped answer is refused");
        assert!(refused.to_string().contains(why), "{why}: {refused}");
        assert!(
            fixture.questions.load(id).await.expect("load").is_open(),
            "a refused answer leaves the question open"
        );
    }

    // The run is RUNNING, not waiting on this question: the answer is recorded and no run
    // is released.
    let answered = fixture
        .questions
        .answer(&multi.id, answer(json!(["eu-west", "ap-south"])))
        .await
        .expect("answer");
    assert_eq!(answered.resumed_run_id, None);
    assert_eq!(answered.question.status, QuestionStatus::Answered);
    let stored = fixture.questions.load(&multi.id).await.expect("load");
    assert_eq!(stored.status, QuestionStatus::Answered);
    assert_eq!(
        stored.answer.expect("answer")["value"],
        json!(["eu-west", "ap-south"])
    );
    let unchanged = fixture.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(unchanged.status, RunStatus::Running);

    let again = fixture
        .questions
        .answer(&multi.id, answer(json!(["us-east"])))
        .await
        .expect_err("a question is answered once");
    assert_eq!(again.code(), "CONFLICT_STATE");

    // A stored kind the protocol does not know can never be answered.
    let unknown = QuestionRecord {
        kind: "ranking".to_string(),
        ..free
    };
    let refused = validate_answer(&unknown, &json!("first")).expect_err("unknown kind");
    assert!(refused.to_string().contains("ranking"), "{refused}");
    finish(fixture).await;
}

#[tokio::test]
async fn an_expired_question_fails_the_run_waiting_on_it_and_cannot_then_be_answered() {
    let Some(fixture) = prepare("qa001_q_expiry", 60).await else {
        blocked_marker();
        return;
    };
    let run = running(&fixture).await;
    let outcome = fixture
        .engine
        .run_turn(
            &run.id,
            run.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let TurnOutcome::Parked {
        state, wait_key, ..
    } = outcome
    else {
        panic!("a question parks the run: {outcome:?}");
    };
    assert_eq!(state, RunStatus::WaitingQuestion);
    let asked = fixture.questions.load(&wait_key).await.expect("question");
    let expires_at = asked.expires_at.expect("the policy ttl sets an expiry");
    let now = chrono::Utc::now();
    assert!(
        expires_at > now && expires_at <= now + chrono::Duration::seconds(61),
        "the expiry follows question_default_ttl: {expires_at}"
    );

    assert!(
        fixture
            .questions
            .expire_due(now)
            .await
            .expect("sweep")
            .is_empty(),
        "nothing is due before the ttl"
    );
    let waiting = fixture.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(waiting.status, RunStatus::WaitingQuestion);

    let later = now + chrono::Duration::minutes(5);
    let expired = fixture.questions.expire_due(later).await.expect("sweep");
    assert_eq!(expired, vec![wait_key.clone()]);
    let stored = fixture.questions.load(&wait_key).await.expect("question");
    assert_eq!(stored.status, QuestionStatus::Expired);
    assert!(!stored.is_open());
    let events = events_of_type(&fixture.pool, "question.expired").await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["question_id"], json!(wait_key));
    assert_eq!(events[0]["run_id"], json!(run.id.to_string()));

    let failed = fixture.engine.store().load_run(&run.id).await.expect("run");
    assert_eq!(failed.status, RunStatus::Failed, "the waiting run fails");
    assert_eq!(failed.terminal_reason.as_deref(), Some("QUESTION_EXPIRED"));

    let late = fixture
        .questions
        .answer(&wait_key, answer(json!(true)))
        .await
        .expect_err("an expired question cannot be answered");
    assert_eq!(late.code(), "CONFLICT_STATE");
    assert!(
        fixture
            .questions
            .expire_due(later)
            .await
            .expect("sweep")
            .is_empty(),
        "a second sweep expires nothing again"
    );
    assert_eq!(
        events_of_type(&fixture.pool, "question.expired")
            .await
            .len(),
        1
    );
    finish(fixture).await;
}

#[test]
fn question_statuses_round_trip_and_unknown_spellings_are_refused() {
    for status in [
        QuestionStatus::Open,
        QuestionStatus::Answered,
        QuestionStatus::Expired,
        QuestionStatus::Cancelled,
    ] {
        assert_eq!(QuestionStatus::parse(status.as_str()), Ok(status));
    }
    assert_eq!(QuestionStatus::Expired.as_str(), "expired");
    assert_eq!(QuestionStatus::Cancelled.as_str(), "cancelled");
    let refused = QuestionStatus::parse("pending").expect_err("not a status");
    assert!(refused.contains("pending"), "{refused}");
}
