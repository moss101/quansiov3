//! The human question protocol (DOMAIN.md §3.4, §5.6).
//!
//! A model question becomes a durable `questions` row and the run parks in
//! `WAITING_QUESTION`; only the answer (or expiry) matching that question releases it. The
//! wait itself lives in ProtocolState, so a restarted runtime resumes from durable rows
//! rather than from anything the model said.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use quansio_core::{CanonicalId, Generation, Prefix, UlidGenerator};
use quansio_events::event_type::EventType;
use quansio_events::{EventDraft, EventStore};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::control::schema;
use crate::policy::PolicyStore;

use super::super::state_machine::{
    ProposedQuestion, QuestionContext, QuestionOutcome, QuestionPort, RunStatus, RuntimeError,
    RuntimeIdentity, RuntimeStore, WaitResolution,
};

/// The aggregate type question events are stamped with.
pub const QUESTION_AGGREGATE: &str = "question";

/// Question lifecycle state (DOMAIN.md §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionStatus {
    /// Waiting for a human.
    Open,
    /// Answered by a human.
    Answered,
    /// Expired before an answer arrived.
    Expired,
    /// Withdrawn with its run.
    Cancelled,
}

impl QuestionStatus {
    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Answered => "answered",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse the durable spelling.
    ///
    /// # Errors
    /// Returns the offending value when it is not a question status.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "open" => Ok(Self::Open),
            "answered" => Ok(Self::Answered),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("{other:?} is not a question status")),
        }
    }
}

/// A durable Question row (DOMAIN.md §3.4).
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionRecord {
    /// `q_…` question id.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Run that asked.
    pub run_id: String,
    /// Step the question is recorded under.
    pub step_id: Option<String>,
    /// Thread the question was posted to.
    pub thread_id: Option<String>,
    /// Question kind (`free_text`, `single_choice`, `multi_choice`, `confirm`).
    pub kind: String,
    /// Question text.
    pub prompt: String,
    /// Choices for a choice question.
    pub options: Vec<String>,
    /// Whether an answer is required.
    pub required: bool,
    /// Lifecycle state.
    pub status: QuestionStatus,
    /// Answer, once supplied.
    pub answer: Option<Value>,
    /// When the question expires, when the effective policy sets a TTL.
    pub expires_at: Option<DateTime<Utc>>,
}

impl QuestionRecord {
    /// Whether the question is still waiting for a human.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.status == QuestionStatus::Open
    }
}

/// An answer to a question, as the human supplied it.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionAnswer {
    /// User who answered.
    pub user_id: String,
    /// Answer value; its shape depends on the question kind.
    pub value: Value,
}

/// A question to create.
#[derive(Debug, Clone, PartialEq)]
pub struct NewQuestion {
    /// Run that asked.
    pub run_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Step the question is recorded under.
    pub step_id: Option<String>,
    /// Thread to post the question to.
    pub thread_id: Option<String>,
    /// Question kind.
    pub kind: String,
    /// Question text.
    pub prompt: String,
    /// Choices, for a choice question.
    pub options: Vec<String>,
    /// Whether an answer is required.
    pub required: bool,
    /// Time to live; `None` leaves the question open until answered or cancelled.
    pub ttl_seconds: Option<i64>,
    /// Run generation the ask is fenced by.
    pub generation: Option<Generation>,
}

/// Outcome of answering a question.
#[derive(Debug, Clone, PartialEq)]
pub struct AnsweredQuestion {
    /// The updated row.
    pub question: QuestionRecord,
    /// Run this answer released from `WAITING_QUESTION`, when one was waiting.
    pub resumed_run_id: Option<String>,
}

/// Validate an answer against the question's kind and choices.
///
/// # Errors
/// Returns [`RuntimeError::InvalidArgument`] when the value cannot answer this question.
pub fn validate_answer(record: &QuestionRecord, value: &Value) -> Result<(), RuntimeError> {
    let invalid = |detail: &str| RuntimeError::InvalidArgument(detail.to_string());
    match record.kind.as_str() {
        "free_text" => {
            if !value.is_string() {
                return Err(invalid("a free_text answer must be a string"));
            }
        }
        "confirm" => {
            if !value.is_boolean() {
                return Err(invalid("a confirm answer must be a boolean"));
            }
        }
        "single_choice" => {
            let choice = value
                .as_str()
                .ok_or_else(|| invalid("a single_choice answer must be a string"))?;
            if !record.options.iter().any(|option| option == choice) {
                return Err(invalid("the answer is not one of the question's choices"));
            }
        }
        "multi_choice" => {
            let choices = value
                .as_array()
                .ok_or_else(|| invalid("a multi_choice answer must be an array"))?;
            for choice in choices {
                let choice = choice
                    .as_str()
                    .ok_or_else(|| invalid("multi_choice entries must be strings"))?;
                if !record.options.iter().any(|option| option == choice) {
                    return Err(invalid("an answer is not one of the question's choices"));
                }
            }
        }
        other => return Err(invalid(&format!("unknown question kind {other:?}"))),
    }
    Ok(())
}

/// The durable question protocol.
#[derive(Debug, Clone)]
pub struct QuestionService {
    pool: PgPool,
    events: EventStore,
    identity: RuntimeIdentity,
}

impl QuestionService {
    /// Build the service for one tenant.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        schema::validate_tenant_id(&identity.tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool.clone()),
            pool,
            identity,
        })
    }

    /// The event identity this service stamps on its events.
    #[must_use]
    pub fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    /// Create a question and return the durable row.
    ///
    /// # Errors
    /// Returns [`RuntimeError::InvalidArgument`] for an unknown kind or a choice question
    /// with no choices, and a database error when the insert fails.
    pub async fn create(&self, new: NewQuestion) -> Result<QuestionRecord, RuntimeError> {
        if !matches!(
            new.kind.as_str(),
            "free_text" | "single_choice" | "multi_choice" | "confirm"
        ) {
            return Err(RuntimeError::InvalidArgument(format!(
                "unknown question kind {:?}",
                new.kind
            )));
        }
        if matches!(new.kind.as_str(), "single_choice" | "multi_choice") && new.options.is_empty() {
            return Err(RuntimeError::InvalidArgument(
                "a choice question must offer choices".to_string(),
            ));
        }
        if new.prompt.trim().is_empty() {
            return Err(RuntimeError::InvalidArgument(
                "a question needs a prompt".to_string(),
            ));
        }

        let mut generator = UlidGenerator::new();
        let id = CanonicalId::generate(Prefix::Question, &mut generator).to_string();
        let expires_at = new
            .ttl_seconds
            .map(|seconds| Utc::now() + Duration::seconds(seconds));
        let options_value = serde_json::to_value(&new.options)?;
        let tenant_id = self.identity.tenant_id.clone();
        let correlation_id = self.identity.correlation_id;
        let actor = self.identity.actor.clone();
        let commit_tenant = tenant_id.clone();

        let record = self
            .events
            .commit_mutation_tx(&commit_tenant, move |tx, batch| {
                let id = id.clone();
                let options_value = options_value.clone();
                let actor = actor.clone();
                let new = new.clone();
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO questions (id, tenant_id, workspace_id, run_id, step_id, \
                         thread_id, kind, prompt, options, required, status, expires_at) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'open', $11)",
                    )
                    .bind(&id)
                    .bind(&tenant_id)
                    .bind(&new.workspace_id)
                    .bind(&new.run_id)
                    .bind(&new.step_id)
                    .bind(&new.thread_id)
                    .bind(&new.kind)
                    .bind(&new.prompt)
                    .bind(&options_value)
                    .bind(new.required)
                    .bind(expires_at)
                    .execute(&mut **tx)
                    .await?;

                    let mut draft = EventDraft::new(
                        QUESTION_AGGREGATE,
                        &id,
                        1,
                        EventType::parse("question.asked")?,
                        correlation_id,
                        actor.clone(),
                    )
                    .with_workspace(&new.workspace_id)
                    .with_payload(json!({
                        "question_id": id,
                        "run_id": new.run_id,
                        "step_id": new.step_id,
                        "kind": new.kind,
                        "prompt": new.prompt,
                        "options": options_value,
                        "required": new.required,
                        "expires_at": expires_at.map(|at| at.to_rfc3339()),
                    }));
                    if let Some(generation) = new.generation {
                        draft = draft.with_generation(generation);
                    }
                    batch.emit(draft);

                    Ok(QuestionRecord {
                        id,
                        tenant_id,
                        workspace_id: new.workspace_id,
                        run_id: new.run_id,
                        step_id: new.step_id,
                        thread_id: new.thread_id,
                        kind: new.kind,
                        prompt: new.prompt,
                        options: new.options,
                        required: new.required,
                        status: QuestionStatus::Open,
                        answer: None,
                        expires_at,
                    })
                }) as _
            })
            .await?;
        Ok(record)
    }

    /// Load one question.
    ///
    /// # Errors
    /// Returns [`RuntimeError::NotFound`] when the question is not visible to this tenant.
    pub async fn load(&self, question_id: &str) -> Result<QuestionRecord, RuntimeError> {
        let mut tx = self.tenant_transaction().await?;
        let record =
            self.fetch_one(&mut tx, question_id)
                .await?
                .ok_or_else(|| RuntimeError::NotFound {
                    entity: "question",
                    id: question_id.to_string(),
                    tenant_id: self.identity.tenant_id.clone(),
                })?;
        tx.commit().await?;
        Ok(record)
    }

    /// List the questions a run asked, oldest first.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn for_run(&self, run_id: &str) -> Result<Vec<QuestionRecord>, RuntimeError> {
        let mut tx = self.tenant_transaction().await?;
        let rows = sqlx::query(
            "SELECT id, tenant_id, workspace_id, run_id, step_id, thread_id, kind, prompt, \
             options, required, status, answer, expires_at FROM questions \
             WHERE tenant_id = $1 AND run_id = $2 ORDER BY created_at",
        )
        .bind(&self.identity.tenant_id)
        .bind(run_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(decode_question).collect()
    }

    /// Record a human answer and release the waiting run.
    ///
    /// The answer is validated against the question's kind and choices before anything is
    /// written, so an invalid answer cannot half-release a run.
    ///
    /// # Errors
    /// Returns [`RuntimeError::InvalidArgument`] for a malformed answer,
    /// [`RuntimeError::StateConflict`] when the question is not open, and
    /// [`RuntimeError::NotFound`] when it is not visible.
    pub async fn answer(
        &self,
        question_id: &str,
        answer: QuestionAnswer,
    ) -> Result<AnsweredQuestion, RuntimeError> {
        let question = self.load(question_id).await?;
        if !question.is_open() {
            return Err(RuntimeError::StateConflict {
                entity: "question",
                id: question_id.to_string(),
            });
        }
        validate_answer(&question, &answer.value)?;
        let payload = json!({
            "question_id": question_id,
            "run_id": question.run_id,
            "user_id": answer.user_id,
            "value": answer.value,
        });

        let mut tx = self.tenant_transaction().await?;
        let updated = sqlx::query(
            "UPDATE questions SET status = 'answered', answer = $3, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND status = 'open' RETURNING id",
        )
        .bind(question_id)
        .bind(&self.identity.tenant_id)
        .bind(&payload)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            tx.rollback().await?;
            return Err(RuntimeError::StateConflict {
                entity: "question",
                id: question_id.to_string(),
            });
        }
        tx.commit().await?;

        self.emit(
            question_id,
            &question.workspace_id,
            2,
            "question.answered",
            payload.clone(),
        )
        .await?;

        // Release the run only through its own authoritative transition.
        let store = RuntimeStore::new(self.pool.clone(), self.identity.clone())?;
        let run_id = CanonicalId::parse_typed(&question.run_id, Prefix::Run)?;
        let run = store.load_run(&run_id).await?;
        let resumed = if run.status == RunStatus::WaitingQuestion {
            store
                .resolve_wait(
                    &run_id,
                    run.generation,
                    WaitResolution::Question {
                        question_id: question_id.to_string(),
                    },
                )
                .await?;
            Some(run.id.to_string())
        } else {
            None
        };

        let mut question = question;
        question.status = QuestionStatus::Answered;
        question.answer = Some(payload);
        Ok(AnsweredQuestion {
            question,
            resumed_run_id: resumed,
        })
    }

    /// Expire every open question whose TTL has passed and fail the runs waiting on them.
    ///
    /// # Errors
    /// Returns a database error when a read or transition fails.
    pub async fn expire_due(&self, now: DateTime<Utc>) -> Result<Vec<String>, RuntimeError> {
        let mut tx = self.tenant_transaction().await?;
        let due: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, run_id, workspace_id FROM questions \
             WHERE tenant_id = $1 AND status = 'open' AND expires_at IS NOT NULL AND expires_at <= $2",
        )
        .bind(&self.identity.tenant_id)
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let store = RuntimeStore::new(self.pool.clone(), self.identity.clone())?;
        let mut expired_ids = Vec::with_capacity(due.len());
        for (question_id, run_id, workspace_id) in due {
            let mut tx = self.tenant_transaction().await?;
            let updated = sqlx::query(
                "UPDATE questions SET status = 'expired', updated_at = now() \
                 WHERE id = $1 AND tenant_id = $2 AND status = 'open' RETURNING id",
            )
            .bind(&question_id)
            .bind(&self.identity.tenant_id)
            .fetch_optional(&mut *tx)
            .await?;
            tx.commit().await?;
            if updated.is_none() {
                continue;
            }
            self.emit(
                &question_id,
                &workspace_id,
                3,
                "question.expired",
                json!({
                    "question_id": question_id,
                    "run_id": run_id,
                    "expired_at": now.to_rfc3339(),
                }),
            )
            .await?;

            let parsed = CanonicalId::parse_typed(&run_id, Prefix::Run)?;
            let run = store.load_run(&parsed).await?;
            if run.status == RunStatus::WaitingQuestion {
                store
                    .transition_run(
                        &parsed,
                        run.generation,
                        RunStatus::Failed,
                        Some("QUESTION_EXPIRED".to_string()),
                    )
                    .await?;
            }
            expired_ids.push(question_id);
        }
        Ok(expired_ids)
    }

    /// The effective `question_default_ttl` for a workspace, when a policy sets one.
    ///
    /// # Errors
    /// Returns a database error when the policy set cannot be read.
    pub async fn ttl_seconds(&self, workspace_id: &str) -> Result<Option<i64>, RuntimeError> {
        let store = PolicyStore::new(self.pool.clone(), self.identity.clone())
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let policies = store
            .load_policy_set(workspace_id)
            .await
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let ttl = policies
            .in_order()
            .iter()
            .map(|policy| policy.question_default_ttl_seconds)
            .min();
        Ok(ttl.filter(|seconds| *seconds > 0))
    }

    async fn emit(
        &self,
        question_id: &str,
        workspace_id: &str,
        version: u64,
        event: &str,
        payload: Value,
    ) -> Result<(), RuntimeError> {
        let draft = EventDraft::new(
            QUESTION_AGGREGATE,
            question_id,
            version,
            EventType::parse(event)?,
            self.identity.correlation_id,
            self.identity.actor.clone(),
        )
        .with_workspace(workspace_id)
        .with_payload(payload);
        let tenant_id = self.identity.tenant_id.clone();
        self.events
            .commit_mutation_tx(&tenant_id, move |_tx, batch| {
                Box::pin(async move {
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await?;
        Ok(())
    }

    async fn fetch_one(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        question_id: &str,
    ) -> Result<Option<QuestionRecord>, RuntimeError> {
        let row = sqlx::query(
            "SELECT id, tenant_id, workspace_id, run_id, step_id, thread_id, kind, prompt, \
             options, required, status, answer, expires_at FROM questions \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(question_id)
        .bind(&self.identity.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        row.as_ref().map(decode_question).transpose()
    }

    async fn tenant_transaction(&self) -> Result<Transaction<'static, Postgres>, RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        Ok(tx)
    }
}

#[async_trait]
impl QuestionPort for QuestionService {
    async fn ask(
        &self,
        question: ProposedQuestion,
        context: QuestionContext,
    ) -> Result<QuestionOutcome, RuntimeError> {
        let store = RuntimeStore::new(self.pool.clone(), self.identity.clone())?;
        let run_id = CanonicalId::parse_typed(&context.run_id, Prefix::Run)?;
        let run = store.load_run(&run_id).await?;
        let ttl = self.ttl_seconds(&run.workspace_id).await?;
        let record = self
            .create(NewQuestion {
                run_id: context.run_id.clone(),
                workspace_id: run.workspace_id.clone(),
                step_id: Some(context.step_id.clone()),
                thread_id: context.thread_id.clone(),
                kind: question.kind,
                prompt: question.prompt,
                options: Vec::new(),
                required: true,
                ttl_seconds: ttl,
                generation: Generation::new(context.generation).ok(),
            })
            .await?;
        Ok(QuestionOutcome::Asked {
            question_id: record.id,
        })
    }
}

fn decode_question(row: &sqlx::postgres::PgRow) -> Result<QuestionRecord, RuntimeError> {
    let status: String = row.try_get("status")?;
    let options: Value = row.try_get("options")?;
    let answer: Option<Value> = row.try_get("answer")?;
    let status = QuestionStatus::parse(&status).map_err(|value| RuntimeError::UnknownState {
        entity: "question_status",
        value,
    })?;
    Ok(QuestionRecord {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        run_id: row.try_get("run_id")?,
        step_id: row.try_get("step_id")?,
        thread_id: row.try_get("thread_id")?,
        kind: row.try_get("kind")?,
        prompt: row.try_get("prompt")?,
        options: options
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        required: row.try_get("required")?,
        status,
        answer,
        expires_at: row.try_get("expires_at")?,
    })
}
