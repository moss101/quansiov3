//! Who may read a thread and receive its events (DOMAIN.md §3.1 protection).
//!
//! A `direct` or `group` thread with participant rows is *protected*: only its active
//! participants who still hold an active workspace membership may read it. An `objective`
//! thread, and a thread with no participant rows (every thread written before APP-009),
//! is readable by the workspace. The answer is read from the rows every time a projection
//! or a command asks ([`thread_access`]); the stream asks once per thread and remembers the
//! answer until a membership event for that thread (or a workspace membership change)
//! passes through it ([`ThreadGate`]), which is what makes "a removed participant receives
//! no *new* protected event" hold: the removal is ordered before every event committed
//! after it.

use std::collections::HashMap;

use quansio_events::{LiveFrame, RuntimeEvent};
use serde_json::Value;
use sqlx::{PgPool, Row};

use crate::control::schema;

/// The answer for one reader and one thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// The thread is not protected (objective, or no participants).
    Open,
    /// The reader is an active participant with an active workspace membership.
    Participant,
    /// The thread is protected and the reader may not see it.
    Denied,
    /// No such thread in this tenant.
    Missing,
}

impl Access {
    /// Whether the reader may see the thread.
    #[must_use]
    pub const fn admits(&self) -> bool {
        matches!(self, Self::Open | Self::Participant)
    }
}

/// Decide one reader's access to one thread from the rows.
///
/// # Errors
/// Returns a database error.
pub async fn thread_access(
    pool: &PgPool,
    tenant_id: &str,
    thread_id: &str,
    user_id: Option<&str>,
) -> Result<Access, sqlx::Error> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let row = sqlx::query(
        "SELECT t.kind, t.workspace_id, \
                EXISTS (SELECT 1 FROM thread_participants p WHERE p.tenant_id = t.tenant_id AND p.thread_id = t.id) AS has_participants, \
                EXISTS (SELECT 1 FROM thread_participants p \
                         JOIN workspace_memberships m ON m.tenant_id = p.tenant_id AND m.workspace_id = t.workspace_id \
                              AND m.user_id = p.principal_id AND m.status = 'active' \
                         WHERE p.tenant_id = t.tenant_id AND p.thread_id = t.id AND p.kind = 'user' \
                           AND p.principal_id = $3 AND p.left_at IS NULL) AS participates \
         FROM threads t WHERE t.tenant_id = $1 AND t.id = $2",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .bind(user_id.unwrap_or(""))
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let Some(row) = row else {
        return Ok(Access::Missing);
    };
    let kind: String = row.get("kind");
    let protected = kind != "objective" && row.get::<bool, _>("has_participants");
    if !protected {
        return Ok(Access::Open);
    }
    Ok(if user_id.is_some() && row.get::<bool, _>("participates") {
        Access::Participant
    } else {
        Access::Denied
    })
}

/// The per-subscriber filter the stream applies to durable events and live frames.
#[derive(Debug)]
pub struct ThreadGate {
    pool: PgPool,
    tenant_id: String,
    user_id: Option<String>,
    threads: HashMap<String, bool>,
    runs: HashMap<String, Option<String>>,
    questions: HashMap<String, Option<String>>,
}

impl ThreadGate {
    /// A gate for one subscriber.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: &str, user_id: Option<&str>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.to_string(),
            user_id: user_id.map(str::to_string),
            threads: HashMap::new(),
            runs: HashMap::new(),
            questions: HashMap::new(),
        }
    }

    /// Whether this subscriber may see the thread now (remembered until a membership event
    /// for it passes). A database failure denies: the gate fails closed.
    pub async fn admits_thread(&mut self, thread_id: &str) -> bool {
        if let Some(known) = self.threads.get(thread_id) {
            return *known;
        }
        let admits = thread_access(
            &self.pool,
            &self.tenant_id,
            thread_id,
            self.user_id.as_deref(),
        )
        .await
        .is_ok_and(|access| access.admits() || access == Access::Missing);
        self.threads.insert(thread_id.to_string(), admits);
        admits
    }

    /// Whether this subscriber may receive a durable event.
    ///
    /// `thread.*` events are gated by their thread, `question.*` events by the thread of
    /// their question or run, `notification.*` events reach only the user they name; every
    /// other family passes (runs, approvals and effects are
    /// workspace governance, readable by workspace roles). A participant always receives
    /// the event that adds or removes *them*, so a removed person learns they were removed —
    /// and nothing of the thread after it.
    pub async fn admits_event(&mut self, event: &RuntimeEvent) -> bool {
        let family = event.event_type.family().as_str();
        let membership_change = event.aggregate_type == "thread"
            && matches!(
                event.event_type.name(),
                "participant_added" | "participant_removed"
            );
        if family == "notification" {
            // A notification is its recipient's alone (APP-010).
            return self.user_id.is_some()
                && event.payload.get("user_id").and_then(Value::as_str) == self.user_id.as_deref();
        }
        if family == "member" || family == "workspace" {
            // A workspace membership change can revoke every protected thread at once.
            self.threads.clear();
            return true;
        }
        let thread_id = if event.aggregate_type == "thread" {
            Some(event.aggregate_id.clone())
        } else if family == "question" {
            self.question_thread(event).await
        } else {
            None
        };
        let Some(thread_id) = thread_id else {
            return true;
        };
        if membership_change {
            // Decide on fresh rows: the change this event announces is already committed.
            self.threads.remove(&thread_id);
            if self.names_subscriber(&event.payload) {
                // The person added or removed always hears it; the next event is decided on
                // the membership it announced.
                return true;
            }
        }
        self.admits_thread(&thread_id).await
    }

    /// Whether this subscriber may receive a live frame: a frame of a run that a protected
    /// thread started is that thread's content in flight.
    pub async fn admits_live(&mut self, frame: &LiveFrame) -> bool {
        if let Some(thread_id) = frame.channel.strip_prefix("thread:") {
            let thread_id = thread_id.to_string();
            return self.admits_thread(&thread_id).await;
        }
        let run_id = frame
            .frame
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| frame.channel.strip_prefix("run:").map(str::to_string));
        let Some(run_id) = run_id else {
            return true;
        };
        match self.run_thread(&run_id).await {
            Some(thread_id) => self.admits_thread(&thread_id).await,
            None => true,
        }
    }

    fn names_subscriber(&self, payload: &Value) -> bool {
        let Some(user) = self.user_id.as_deref() else {
            return false;
        };
        payload.get("principal").is_some_and(|principal| {
            principal.get("kind").and_then(Value::as_str) == Some("user")
                && principal.get("id").and_then(Value::as_str) == Some(user)
        })
    }

    async fn question_thread(&mut self, event: &RuntimeEvent) -> Option<String> {
        if let Some(thread) = event.payload.get("thread_id").and_then(Value::as_str) {
            return Some(thread.to_string());
        }
        if let Some(known) = self.questions.get(&event.aggregate_id) {
            return known.clone();
        }
        let from_row: Option<Option<String>> = self
            .scalar(
                "SELECT thread_id FROM questions WHERE tenant_id = $1 AND id = $2",
                &event.aggregate_id,
            )
            .await;
        let thread = match from_row {
            Some(Some(thread)) => Some(thread),
            _ => match event.payload.get("run_id").and_then(Value::as_str) {
                Some(run) => {
                    let run = run.to_string();
                    self.run_thread(&run).await
                }
                None => None,
            },
        };
        self.questions
            .insert(event.aggregate_id.clone(), thread.clone());
        thread
    }

    async fn run_thread(&mut self, run_id: &str) -> Option<String> {
        if let Some(known) = self.runs.get(run_id) {
            return known.clone();
        }
        let thread = self
            .scalar(
                "SELECT m.thread_id FROM runs r JOIN messages m \
                   ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
                 WHERE r.tenant_id = $1 AND r.id = $2",
                run_id,
            )
            .await
            .flatten();
        self.runs.insert(run_id.to_string(), thread.clone());
        thread
    }

    async fn scalar(&self, sql: &str, id: &str) -> Option<Option<String>> {
        let mut tx = self.pool.begin().await.ok()?;
        schema::set_tenant_context(&mut tx, &self.tenant_id)
            .await
            .ok()?;
        let value: Option<Option<String>> = sqlx::query_scalar(sql)
            .bind(&self.tenant_id)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .ok()?;
        tx.commit().await.ok()?;
        value
    }
}
