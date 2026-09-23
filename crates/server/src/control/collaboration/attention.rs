//! Derived attention (DOMAIN.md §3.1): nothing here is stored.
//!
//! For a person: the messages that mention them after their own last message in the thread,
//! and the thread's open questions. For a teammate: its AgentThread's status and its latest
//! run in the thread, which needs a human while it waits on an approval or a question.

use std::collections::HashMap;

use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use super::store::Participant;
use crate::control::schema;

/// One participant's attention in one thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantAttention {
    /// `user` or `teammate` (or `worker`).
    pub kind: String,
    /// The principal.
    pub id: String,
    /// Messages mentioning a person after their own last message.
    pub mentions_pending: i64,
    /// Open questions in the thread (a person answers them).
    pub open_questions: i64,
    /// A teammate's AgentThread status.
    pub agent_status: Option<String>,
    /// A teammate's latest run in the thread: `(run_id, status)`.
    pub run: Option<(String, String)>,
}

impl ParticipantAttention {
    /// Whether a human should look: a person mentioned or asked, a teammate's run parked on
    /// a person, or a teammate whose thread is suspended.
    #[must_use]
    pub fn needs_human(&self) -> bool {
        self.mentions_pending > 0
            || self.open_questions > 0
            || self.run.as_ref().is_some_and(|(_, status)| {
                status == "WAITING_APPROVAL" || status == "WAITING_QUESTION"
            })
            || self.agent_status.as_deref() == Some("SUSPENDED")
    }

    /// The projection's spelling.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "id": self.id,
            "mentions_pending": self.mentions_pending,
            "open_questions": self.open_questions,
            "agent_status": self.agent_status,
            "run": self.run.as_ref().map(|(id, status)| json!({ "id": id, "status": status })),
            "needs_human": self.needs_human(),
        })
    }
}

const OPEN_QUESTIONS: &str = "(SELECT count(*) FROM questions q WHERE q.tenant_id = $1 AND q.status = 'open' \
     AND (q.thread_id = $2 OR q.run_id IN (SELECT r.id FROM runs r JOIN messages rm \
          ON rm.tenant_id = r.tenant_id AND rm.id = r.trigger_ref WHERE r.tenant_id = $1 AND rm.thread_id = $2)))";

/// Attention of every active participant of one thread.
///
/// # Errors
/// Returns a database error.
pub async fn participant_attention(
    pool: &PgPool,
    tenant_id: &str,
    thread_id: &str,
    participants: &[Participant],
) -> Result<Vec<ParticipantAttention>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let open_questions: i64 = sqlx::query_scalar(&format!("SELECT {OPEN_QUESTIONS}"))
        .bind(tenant_id)
        .bind(thread_id)
        .fetch_one(&mut *tx)
        .await?;
    let mut out = Vec::new();
    for participant in participants
        .iter()
        .filter(|participant| participant.is_active())
    {
        let mut attention = ParticipantAttention {
            kind: participant.kind.clone(),
            id: participant.principal_id.clone(),
            mentions_pending: 0,
            open_questions: 0,
            agent_status: None,
            run: None,
        };
        if participant.kind == "user" {
            attention.mentions_pending = sqlx::query_scalar(
                "SELECT count(*) FROM messages m WHERE m.tenant_id = $1 AND m.thread_id = $2 \
                   AND m.deleted_at IS NULL AND m.mentions @> $4::jsonb \
                   AND m.seq > COALESCE((SELECT MAX(o.seq) FROM messages o WHERE o.tenant_id = $1 \
                       AND o.thread_id = $2 AND o.author_kind = 'user' AND o.author_id = $3), 0)",
            )
            .bind(tenant_id)
            .bind(thread_id)
            .bind(&participant.principal_id)
            .bind(json!([{ "kind": "user", "id": participant.principal_id }]))
            .fetch_one(&mut *tx)
            .await?;
            attention.open_questions = open_questions;
        } else if participant.kind == "teammate" {
            attention.agent_status = sqlx::query_scalar(
                "SELECT status FROM agent_threads WHERE tenant_id = $1 AND definition_id = $2 \
                 ORDER BY created_at LIMIT 1",
            )
            .bind(tenant_id)
            .bind(&participant.principal_id)
            .fetch_optional(&mut *tx)
            .await?;
            let run = sqlx::query(
                "SELECT r.id, r.status FROM runs r \
                 JOIN agent_threads a ON a.tenant_id = r.tenant_id AND a.id = r.agent_thread_id \
                 JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
                 WHERE r.tenant_id = $1 AND a.definition_id = $2 AND m.thread_id = $3 \
                 ORDER BY r.created_at DESC LIMIT 1",
            )
            .bind(tenant_id)
            .bind(&participant.principal_id)
            .bind(thread_id)
            .fetch_optional(&mut *tx)
            .await?;
            attention.run = run.map(|row| (row.get("id"), row.get("status")));
        }
        out.push(attention);
    }
    tx.commit().await?;
    Ok(out)
}

/// One person's attention across a workspace's threads: `thread_id → (mentions pending,
/// open questions)`.
///
/// # Errors
/// Returns a database error.
pub async fn thread_attention_for(
    pool: &PgPool,
    tenant_id: &str,
    workspace_id: &str,
    user_id: &str,
) -> Result<HashMap<String, (i64, i64)>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let rows = sqlx::query(
        "SELECT t.id, \
            (SELECT count(*) FROM messages m WHERE m.tenant_id = t.tenant_id AND m.thread_id = t.id \
               AND m.deleted_at IS NULL AND m.mentions @> $4::jsonb \
               AND m.seq > COALESCE((SELECT MAX(o.seq) FROM messages o WHERE o.tenant_id = t.tenant_id \
                   AND o.thread_id = t.id AND o.author_kind = 'user' AND o.author_id = $3), 0)) AS mentions_pending, \
            (SELECT count(*) FROM questions q WHERE q.tenant_id = t.tenant_id AND q.status = 'open' \
               AND (q.thread_id = t.id OR q.run_id IN (SELECT r.id FROM runs r JOIN messages rm \
                    ON rm.tenant_id = r.tenant_id AND rm.id = r.trigger_ref \
                    WHERE r.tenant_id = t.tenant_id AND rm.thread_id = t.id))) AS open_questions \
         FROM threads t WHERE t.tenant_id = $1 AND t.workspace_id = $2 AND t.archived_at IS NULL",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(user_id)
    .bind(json!([{ "kind": "user", "id": user_id }]))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("id"),
                (row.get("mentions_pending"), row.get("open_questions")),
            )
        })
        .collect())
}
