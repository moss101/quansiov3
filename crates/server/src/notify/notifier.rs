//! The notifier: RuntimeEvents in, notifications out (DOMAIN.md §13.3).
//!
//! A consumer of the tenant's event log with its own `event_cursors` position. For each
//! event it decides who should hear about it and as what kind:
//!
//! | event | kind | who |
//! |---|---|---|
//! | `thread.attention_changed` (mentioned a person) | `mention` | that person |
//! | `question.asked` | `needs_answer` | the people in the question's thread (else who started the run) |
//! | `approval.requested` | `needs_approval` | the workspace's admins and approvers |
//! | `routine.fire_completed` | `completed` | the routine's owner |
//! | `routine.fire_failed` | `failed` | the routine's owner |
//! | `run.blocked` / `run.failed` of a routine's run | `blocked` / `failed` | the routine's owner |
//!
//! The notifications and the cursor advance commit together, and a notification is unique
//! per (event, user, kind), so consuming an event twice notifies once.

use quansio_events::{advance_cursor_in_tx, EventError, EventStore, RuntimeEvent};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool, Row};

use super::store::{create_in, NewNotification};

/// The cursor stream the notifier advances.
pub const NOTIFIER_STREAM: &str = "notify:events";
/// The consumer name.
pub const NOTIFIER_CONSUMER: &str = "notifier";

/// The notifier for one tenant.
#[derive(Debug, Clone)]
pub struct Notifier {
    events: EventStore,
    tenant_id: String,
}

impl Notifier {
    /// A notifier over the pool for one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// Consume up to `limit` events. Returns how many notifications were made.
    ///
    /// # Errors
    /// Returns a database or event error; nothing is made and the cursor does not move.
    pub async fn scan(&self, limit: i64) -> Result<usize, EventError> {
        let position = self
            .events
            .load_cursor(&self.tenant_id, NOTIFIER_STREAM, NOTIFIER_CONSUMER)
            .await?
            .map(|cursor| cursor.sequence());
        let batch = self
            .events
            .read_events_after(&self.tenant_id, position, limit)
            .await?;
        let Some(last) = batch.last().map(|event| event.sequence) else {
            return Ok(0);
        };
        let tenant = self.tenant_id.clone();
        let made = self
            .events
            .commit_mutation_tx(&self.tenant_id, move |tx, staged| {
                Box::pin(async move {
                    let mut made = 0usize;
                    for event in &batch {
                        for notification in recipients(tx, &tenant, event).await? {
                            if create_in(tx, staged, &tenant, &notification)
                                .await?
                                .is_some()
                            {
                                made += 1;
                            }
                        }
                    }
                    advance_cursor_in_tx(tx, &tenant, NOTIFIER_STREAM, NOTIFIER_CONSUMER, last)
                        .await?;
                    Ok(made)
                })
            })
            .await;
        match made {
            Ok(made) => Ok(made),
            // Nothing to notify in this batch: move the cursor on alone.
            Err(EventError::NoEventStaged) => {
                let mut tx = self
                    .events
                    .begin_tenant_transaction(&self.tenant_id)
                    .await?;
                advance_cursor_in_tx(
                    &mut tx,
                    &self.tenant_id,
                    NOTIFIER_STREAM,
                    NOTIFIER_CONSUMER,
                    last,
                )
                .await?;
                tx.commit().await?;
                Ok(0)
            }
            Err(error) => Err(error),
        }
    }
}

fn text<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// Who hears about one event, and as what.
async fn recipients(
    conn: &mut PgConnection,
    tenant_id: &str,
    event: &RuntimeEvent,
) -> Result<Vec<NewNotification>, EventError> {
    let Some(workspace_id) = event.workspace_id.clone() else {
        return Ok(Vec::new());
    };
    let event_type = event.event_type.to_string();
    let payload = &event.payload;
    let source = Some(event.event_id.to_string());
    let one =
        |user_id: &str, kind: &'static str, title: String, reference: Value| NewNotification {
            workspace_id: workspace_id.clone(),
            user_id: user_id.to_string(),
            kind,
            title,
            reference,
            source_event_id: source.clone(),
        };
    Ok(match event_type.as_str() {
        "thread.attention_changed" => {
            let principal = payload.get("principal");
            let is_person = principal
                .and_then(|p| p.get("kind"))
                .and_then(Value::as_str)
                == Some("user");
            match (
                text(payload, "attention"),
                principal.and_then(|p| p.get("id")).and_then(Value::as_str),
            ) {
                (Some("mentioned"), Some(user)) if is_person => {
                    let title = thread_title(conn, tenant_id, &event.aggregate_id).await?;
                    vec![one(
                        user,
                        "mention",
                        format!("You were mentioned in {title}"),
                        json!({ "thread_id": event.aggregate_id, "message_id": text(payload, "message_id") }),
                    )]
                }
                _ => Vec::new(),
            }
        }
        "question.asked" => {
            let run_id = text(payload, "run_id").unwrap_or("");
            let (thread_id, people) =
                question_audience(conn, tenant_id, &event.aggregate_id, run_id).await?;
            people
                .iter()
                .map(|user| {
                    one(
                        user,
                        "needs_answer",
                        "A question needs your answer".to_string(),
                        json!({ "question_id": event.aggregate_id, "run_id": run_id, "thread_id": thread_id }),
                    )
                })
                .collect()
        }
        "approval.requested" => {
            let approvers: Vec<String> = sqlx::query_scalar(
                "SELECT user_id FROM workspace_memberships WHERE tenant_id = $1 AND workspace_id = $2 \
                 AND status = 'active' AND role IN ('admin', 'approver') ORDER BY user_id",
            )
            .bind(tenant_id)
            .bind(&workspace_id)
            .fetch_all(&mut *conn)
            .await?;
            approvers
                .iter()
                .map(|user| {
                    one(
                        user,
                        "needs_approval",
                        "An action needs your approval".to_string(),
                        json!({ "approval_id": text(payload, "approval_id"), "run_id": text(payload, "run_id") }),
                    )
                })
                .collect()
        }
        "routine.fire_completed" | "routine.fire_failed" => {
            let Some(owner) = text(payload, "owner_user_id") else {
                return Ok(Vec::new());
            };
            let routine_id = text(payload, "routine_id").unwrap_or(&event.aggregate_id);
            let title = routine_title(conn, tenant_id, routine_id).await?;
            let (kind, line) = if event_type == "routine.fire_completed" {
                ("completed", format!("Routine \u{201c}{title}\u{201d} ran"))
            } else {
                ("failed", format!("Routine \u{201c}{title}\u{201d} failed"))
            };
            vec![one(
                owner,
                kind,
                line,
                json!({ "routine_id": routine_id, "run_id": text(payload, "run_id"), "fire_id": text(payload, "fire_id") }),
            )]
        }
        "run.blocked" | "run.failed" => {
            let owner: Option<(String, String)> = sqlx::query_as(
                "SELECT r.owner_user_id, r.id FROM routine_fires f JOIN routines r \
                   ON r.tenant_id = f.tenant_id AND r.id = f.routine_id \
                 WHERE f.tenant_id = $1 AND f.run_id = $2 LIMIT 1",
            )
            .bind(tenant_id)
            .bind(&event.aggregate_id)
            .fetch_optional(&mut *conn)
            .await?;
            match owner {
                Some((owner, routine_id)) => {
                    let title = routine_title(conn, tenant_id, &routine_id).await?;
                    let blocked = event_type == "run.blocked";
                    vec![one(
                        &owner,
                        if blocked { "blocked" } else { "failed" },
                        format!(
                            "Routine \u{201c}{title}\u{201d} {}",
                            if blocked { "is blocked" } else { "failed" }
                        ),
                        json!({ "routine_id": routine_id, "run_id": event.aggregate_id }),
                    )]
                }
                None => Vec::new(),
            }
        }
        _ => Vec::new(),
    })
}

async fn thread_title(
    conn: &mut PgConnection,
    tenant_id: &str,
    thread_id: &str,
) -> Result<String, EventError> {
    let title: Option<Option<String>> =
        sqlx::query_scalar("SELECT title FROM threads WHERE tenant_id = $1 AND id = $2")
            .bind(tenant_id)
            .bind(thread_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(title.flatten().unwrap_or_else(|| "a thread".to_string()))
}

async fn routine_title(
    conn: &mut PgConnection,
    tenant_id: &str,
    routine_id: &str,
) -> Result<String, EventError> {
    let title: Option<Option<String>> = sqlx::query_scalar(
        "SELECT objective_template ->> 'title' FROM routines WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(routine_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(title.flatten().unwrap_or_else(|| routine_id.to_string()))
}

/// The thread a question belongs to and the people who should answer it: the thread's
/// active user participants, else the person whose message started the run.
async fn question_audience(
    conn: &mut PgConnection,
    tenant_id: &str,
    question_id: &str,
    run_id: &str,
) -> Result<(Option<String>, Vec<String>), EventError> {
    let row = sqlx::query(
        "SELECT COALESCE(q.thread_id, m.thread_id) AS thread_id, \
                CASE WHEN m.author_kind = 'user' THEN m.author_id END AS starter \
         FROM runs r LEFT JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
         LEFT JOIN questions q ON q.tenant_id = r.tenant_id AND q.id = $3 \
         WHERE r.tenant_id = $1 AND r.id = $2",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(question_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok((None, Vec::new()));
    };
    let thread_id: Option<String> = row.try_get("thread_id")?;
    let starter: Option<String> = row.try_get("starter")?;
    let mut people: Vec<String> = match &thread_id {
        Some(thread) => sqlx::query_scalar(
            "SELECT principal_id FROM thread_participants WHERE tenant_id = $1 AND thread_id = $2 \
                 AND kind = 'user' AND left_at IS NULL ORDER BY joined_at, id",
        )
        .bind(tenant_id)
        .bind(thread)
        .fetch_all(&mut *conn)
        .await?,
        None => Vec::new(),
    };
    if people.is_empty() {
        if let Some(starter) = starter {
            people.push(starter);
        }
    }
    Ok((thread_id, people))
}
