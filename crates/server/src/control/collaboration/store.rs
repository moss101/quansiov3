//! `thread_participants` and the mutations that compose it with the conversation owner.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_events::{EventBatch, EventError, EventStore};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::control::conversation::{
    self, lock_thread_in, thread_event_in, ConversationError, Message, NewMessage, Thread,
};
use crate::runtime::state_machine::RuntimeIdentity;

/// Repository path used to attribute a rejected mutation.
pub const COLLABORATION_OWNER: &str = "crates/server/src/control/collaboration";

const COLUMNS: &str = "id, thread_id, kind, principal_id, role, joined_at, left_at";

type BoxCollaborationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CollaborationError>> + Send + 'a>>;

/// Failures from the collaboration owner.
#[derive(Debug, thiserror::Error)]
pub enum CollaborationError {
    /// The event store refused the commit.
    #[error("collaboration event store: {0}")]
    Event(#[from] EventError),
    /// PostgreSQL rejected a statement.
    #[error("collaboration database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The conversation owner refused its part.
    #[error("{0}")]
    Conversation(#[from] ConversationError),
    /// A request that is malformed.
    #[error("collaboration schema: {0}")]
    Validation(String),
    /// Something named is not visible here.
    #[error("{0}")]
    NotFound(String),
    /// The state does not allow the change.
    #[error("{0}")]
    Conflict(String),
    /// Nothing to change: the membership is already as asked.
    #[error("the membership is already in the requested state")]
    Unchanged,
}

impl CollaborationError {
    /// DOMAIN.md §15 code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) | Self::Unchanged => "CONFLICT_STATE",
            Self::Conversation(error) => error.code(),
            Self::Event(_) | Self::Database(_) => "INTERNAL",
        }
    }
}

/// Who a participant is (the schema's `thread_participants.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantKind {
    /// A person (`usr_…`).
    User,
    /// A teammate definition (`agt_…`).
    Teammate,
    /// A delegated worker AgentThread (`ath_…`).
    Worker,
}

impl ParticipantKind {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Teammate => "teammate",
            Self::Worker => "worker",
        }
    }

    /// Parse the column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "teammate" => Some(Self::Teammate),
            "worker" => Some(Self::Worker),
            _ => None,
        }
    }

    /// The id prefix a principal of this kind carries.
    #[must_use]
    pub const fn prefix(self) -> Prefix {
        match self {
            Self::User => Prefix::User,
            Self::Teammate => Prefix::Teammate,
            Self::Worker => Prefix::AgentThread,
        }
    }
}

/// A participant's role (the schema's `thread_participants.role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantRole {
    /// May remove others and grant ownership.
    Owner,
    /// Everyone else.
    Member,
}

impl ParticipantRole {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Member => "member",
        }
    }

    /// Parse the column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

/// One membership row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    /// `thp_<ULID>` (carried opaquely: the prefix is not in the DOMAIN §1.1 table).
    pub id: String,
    /// Thread.
    pub thread_id: String,
    /// `user`, `teammate` or `worker`.
    pub kind: String,
    /// `usr_…`, `agt_…` or `ath_…`.
    pub principal_id: String,
    /// `owner` or `member`.
    pub role: String,
    /// When they (last) joined.
    pub joined_at: DateTime<Utc>,
    /// When they left; `None` while active.
    pub left_at: Option<DateTime<Utc>>,
}

impl Participant {
    /// Whether the participant is in the thread now.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.left_at.is_none()
    }

    /// The projection's spelling.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "id": self.principal_id,
            "role": self.role,
            "joined_at": self.joined_at,
            "left_at": self.left_at,
            "active": self.is_active(),
        })
    }
}

/// What a completed handoff produced.
#[derive(Debug, Clone, PartialEq)]
pub struct HandOff {
    /// The teammate that left.
    pub from: Participant,
    /// The teammate that joined.
    pub to: Participant,
    /// The system notice posted in the thread.
    pub notice: Message,
}

/// Collaboration owner for one tenant.
#[derive(Debug, Clone)]
pub struct CollaborationStore {
    pool: PgPool,
    events: EventStore,
    identity: RuntimeIdentity,
}

impl CollaborationStore {
    /// Bind the store to one tenant identity.
    #[must_use]
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Self {
        Self {
            events: EventStore::new(pool.clone()),
            pool,
            identity,
        }
    }

    /// The pool reads use.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Every participant of a thread, active and past, in joining order.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn participants(
        &self,
        thread_id: &str,
    ) -> Result<Vec<Participant>, CollaborationError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM thread_participants WHERE tenant_id = $1 AND thread_id = $2 \
             ORDER BY joined_at, id"
        ))
        .bind(&self.identity.tenant_id)
        .bind(thread_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.iter().map(participant_from_row).collect())
    }

    /// The thread's responsible teammate: the most recently joined active teammate.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn responsible_teammate(
        &self,
        thread_id: &str,
    ) -> Result<Option<String>, CollaborationError> {
        Ok(self
            .participants(thread_id)
            .await?
            .into_iter()
            .filter(|participant| participant.is_active() && participant.kind == "teammate")
            .max_by(|a, b| a.joined_at.cmp(&b.joined_at).then_with(|| a.id.cmp(&b.id)))
            .map(|participant| participant.principal_id))
    }

    /// Refuse a principal that may not join a thread of `workspace_id`: a user without an
    /// active workspace membership, a teammate that is not active in the workspace, a worker
    /// thread of another workspace.
    ///
    /// # Errors
    /// Returns [`CollaborationError::Validation`] for a malformed id and
    /// [`CollaborationError::Conflict`] / [`CollaborationError::NotFound`] otherwise.
    pub async fn check_principal(
        &self,
        workspace_id: &str,
        kind: ParticipantKind,
        principal_id: &str,
    ) -> Result<(), CollaborationError> {
        CanonicalId::parse_typed(principal_id, kind.prefix()).map_err(|error| {
            CollaborationError::Validation(format!("{} participant id: {error}", kind.as_str()))
        })?;
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let ok: Option<String> = match kind {
            ParticipantKind::User => {
                sqlx::query_scalar(
                    "SELECT status FROM workspace_memberships \
                     WHERE tenant_id = $1 AND workspace_id = $2 AND user_id = $3",
                )
                .bind(&self.identity.tenant_id)
                .bind(workspace_id)
                .bind(principal_id)
                .fetch_optional(&mut *tx)
                .await?
            }
            ParticipantKind::Teammate => sqlx::query_scalar(
                "SELECT CASE WHEN availability = 'available' THEN status ELSE availability END \
                     FROM teammates WHERE tenant_id = $1 AND workspace_id = $2 AND id = $3",
            )
            .bind(&self.identity.tenant_id)
            .bind(workspace_id)
            .bind(principal_id)
            .fetch_optional(&mut *tx)
            .await?,
            ParticipantKind::Worker => {
                sqlx::query_scalar(
                    "SELECT lower(status) FROM agent_threads \
                     WHERE tenant_id = $1 AND workspace_id = $2 AND id = $3",
                )
                .bind(&self.identity.tenant_id)
                .bind(workspace_id)
                .bind(principal_id)
                .fetch_optional(&mut *tx)
                .await?
            }
        };
        tx.commit().await?;
        match ok.as_deref() {
            None => Err(CollaborationError::NotFound(format!(
                "{} {principal_id} is not in workspace {workspace_id}",
                kind.as_str()
            ))),
            Some("active") => Ok(()),
            Some(status) => Err(CollaborationError::Conflict(format!(
                "{} {principal_id} is {status} and cannot join a thread",
                kind.as_str()
            ))),
        }
    }

    /// Create a thread with its creator as owner and any further participants, in one
    /// transaction (`thread.created` then one `thread.participant_added` each).
    ///
    /// # Errors
    /// Returns a schema or database error; nothing is written on failure.
    pub async fn create_thread(
        &self,
        workspace_id: &str,
        kind: &str,
        title: Option<String>,
        creator: Option<&str>,
        others: Vec<(ParticipantKind, String, ParticipantRole)>,
    ) -> Result<(Thread, Vec<Participant>), CollaborationError> {
        for (kind, principal, _) in &others {
            self.check_principal(workspace_id, *kind, principal).await?;
        }
        let identity = self.identity.clone();
        let workspace = workspace_id.to_string();
        let kind = kind.to_string();
        let creator = creator.map(str::to_string);
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let thread =
                    conversation::create_thread_in(tx, batch, &identity, &workspace, &kind, title)
                        .await?;
                let mut members = Vec::new();
                if let Some(creator) = creator {
                    members.push(
                        upsert_in(
                            tx,
                            batch,
                            &identity,
                            &thread,
                            ParticipantKind::User,
                            &creator,
                            ParticipantRole::Owner,
                            "created",
                        )
                        .await?,
                    );
                }
                for (kind, principal, role) in others {
                    if members
                        .iter()
                        .any(|member: &Participant| member.principal_id == principal)
                    {
                        continue;
                    }
                    members.push(
                        upsert_in(
                            tx, batch, &identity, &thread, kind, &principal, role, "created",
                        )
                        .await?,
                    );
                }
                Ok((thread, members))
            })
        })
        .await
    }

    /// Post a message; when `join` names a teammate that is not an active participant, it
    /// joins in the same transaction (the teammate the message is addressed to).
    ///
    /// # Errors
    /// Returns [`CollaborationError::Conversation`] when the thread is missing.
    pub async fn post(
        &self,
        thread_id: &str,
        new: NewMessage,
        join: Option<String>,
    ) -> Result<(Thread, Message, Option<Participant>), CollaborationError> {
        let identity = self.identity.clone();
        let thread_id = thread_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let thread = lock_thread_in(tx, &identity.tenant_id, &thread_id).await?;
                let mut joined = None;
                if let Some(teammate) = join {
                    let current = load_in(
                        tx,
                        &identity.tenant_id,
                        &thread.id,
                        ParticipantKind::Teammate,
                        &teammate,
                    )
                    .await?;
                    if !current.as_ref().is_some_and(Participant::is_active) {
                        joined = Some(
                            upsert_in(
                                tx,
                                batch,
                                &identity,
                                &thread,
                                ParticipantKind::Teammate,
                                &teammate,
                                ParticipantRole::Member,
                                "addressed",
                            )
                            .await?,
                        );
                    }
                }
                let message =
                    conversation::insert_message_in(tx, batch, &identity, &thread, new).await?;
                Ok((thread, message, joined))
            })
        })
        .await
    }

    /// Add a participant, rejoin one who left, or change a role.
    ///
    /// Idempotent: asking for the membership the thread already has returns it unchanged
    /// (`false`) and writes nothing.
    ///
    /// # Errors
    /// Returns [`CollaborationError::Conversation`] when the thread is missing.
    pub async fn add_participant(
        &self,
        thread_id: &str,
        kind: ParticipantKind,
        principal_id: &str,
        role: ParticipantRole,
    ) -> Result<(Participant, bool), CollaborationError> {
        let identity = self.identity.clone();
        let thread_owned = thread_id.to_string();
        let principal = principal_id.to_string();
        let result = self
            .commit(move |tx, batch| {
                Box::pin(async move {
                    let thread = lock_thread_in(tx, &identity.tenant_id, &thread_owned).await?;
                    let current =
                        load_in(tx, &identity.tenant_id, &thread.id, kind, &principal).await?;
                    if current
                        .as_ref()
                        .is_some_and(|current| current.is_active() && current.role == role.as_str())
                    {
                        return Err(CollaborationError::Unchanged);
                    }
                    upsert_in(
                        tx, batch, &identity, &thread, kind, &principal, role, "added",
                    )
                    .await
                })
            })
            .await;
        match result {
            Ok(participant) => Ok((participant, true)),
            Err(CollaborationError::Unchanged) => {
                let participant = self
                    .participants(thread_id)
                    .await?
                    .into_iter()
                    .find(|participant| {
                        participant.kind == kind.as_str()
                            && participant.principal_id == principal_id
                    })
                    .ok_or_else(|| {
                        CollaborationError::NotFound(format!("participant {principal_id}"))
                    })?;
                Ok((participant, false))
            }
            Err(error) => Err(error),
        }
    }

    /// End a participation (`left_at`), refusing to leave a thread without an owner while
    /// other people remain in it.
    ///
    /// # Errors
    /// Returns [`CollaborationError::NotFound`] for someone who is not an active participant
    /// and [`CollaborationError::Conflict`] for the last owner.
    pub async fn remove_participant(
        &self,
        thread_id: &str,
        kind: ParticipantKind,
        principal_id: &str,
        reason: &str,
    ) -> Result<Participant, CollaborationError> {
        let identity = self.identity.clone();
        let thread_id = thread_id.to_string();
        let principal = principal_id.to_string();
        let reason = reason.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let thread = lock_thread_in(tx, &identity.tenant_id, &thread_id).await?;
                leave_in(tx, batch, &identity, &thread, kind, &principal, &reason).await
            })
        })
        .await
    }

    /// Hand the thread from its responsible teammate to `to_teammate`: the source leaves,
    /// the target joins, `thread.handed_off` and a system notice commit together.
    ///
    /// The mailbox delivery to the target's AgentThread is RUN-002's and follows the commit
    /// (the caller does it with the returned notice id, which makes it idempotent).
    ///
    /// # Errors
    /// Returns [`CollaborationError::Conflict`] when the thread has no responsible teammate,
    /// when the target already is it, or when the source still has live work in the thread
    /// (a run executing a turn, or parked on a person, an event, a timer or a child).
    pub async fn hand_off(
        &self,
        thread_id: &str,
        to_teammate: &str,
        to_name: &str,
        from_name_hint: Option<&str>,
    ) -> Result<HandOff, CollaborationError> {
        let identity = self.identity.clone();
        let thread_id = thread_id.to_string();
        let to = to_teammate.to_string();
        let to_name = to_name.to_string();
        let from_hint = from_name_hint.map(str::to_string);
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let thread = lock_thread_in(tx, &identity.tenant_id, &thread_id).await?;
                let rows = sqlx::query(&format!(
                    "SELECT {COLUMNS} FROM thread_participants WHERE tenant_id = $1 AND thread_id = $2 \
                     AND kind = 'teammate' AND left_at IS NULL ORDER BY joined_at DESC, id DESC LIMIT 1"
                ))
                .bind(&identity.tenant_id)
                .bind(&thread.id)
                .fetch_optional(&mut **tx)
                .await?;
                let Some(from) = rows.as_ref().map(participant_from_row) else {
                    return Err(CollaborationError::Conflict(format!(
                        "thread {} has no responsible teammate to hand off",
                        thread.id
                    )));
                };
                if from.principal_id == to {
                    return Err(CollaborationError::Conflict(format!(
                        "teammate {to} is already responsible for thread {}",
                        thread.id
                    )));
                }
                // The source's live work in this thread must be finished first: a run that
                // is executing a turn, or parked on a person, an event or a child, would
                // otherwise keep writing into a thread its teammate left. A RUNNING run
                // between turns is idle — a chat run stays RUNNING after its reply.
                let live: Option<(String, String)> = sqlx::query_as(
                    "SELECT r.id, r.status FROM runs r \
                     JOIN agent_threads a ON a.tenant_id = r.tenant_id AND a.id = r.agent_thread_id \
                     JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
                     WHERE r.tenant_id = $1 AND a.definition_id = $2 AND m.thread_id = $3 \
                       AND (r.status IN ('QUEUED', 'WAITING_APPROVAL', 'WAITING_QUESTION', 'WAITING_EVENT', \
                                         'WAITING_TIMER', 'WAITING_CHILD', 'WAITING_TAKEOVER', 'VERIFYING') \
                            OR (r.status = 'RUNNING' AND EXISTS (SELECT 1 FROM turns t \
                                WHERE t.tenant_id = r.tenant_id AND t.run_id = r.id AND t.status = 'active'))) \
                     ORDER BY r.created_at DESC LIMIT 1",
                )
                .bind(&identity.tenant_id)
                .bind(&from.principal_id)
                .bind(&thread.id)
                .fetch_optional(&mut **tx)
                .await?;
                if let Some((run_id, status)) = live {
                    return Err(CollaborationError::Conflict(format!(
                        "teammate {} still has run {run_id} ({status}) in thread {}; it must finish or be cancelled before a handoff",
                        from.principal_id, thread.id
                    )));
                }
                let from = leave_in(tx, batch, &identity, &thread, ParticipantKind::Teammate, &from.principal_id, "handoff").await?;
                let joined = upsert_in(tx, batch, &identity, &thread, ParticipantKind::Teammate, &to, ParticipantRole::Member, "handoff").await?;
                let from_name = from_hint.unwrap_or_else(|| from.principal_id.clone());
                let notice = conversation::insert_message_in(
                    tx,
                    batch,
                    &identity,
                    &thread,
                    NewMessage::system_notice(
                        &format!("{from_name} handed this thread to {to_name}."),
                        json!({
                            "kind": "handoff",
                            "from": { "kind": "teammate", "id": from.principal_id },
                            "to": { "kind": "teammate", "id": joined.principal_id },
                        }),
                    ),
                )
                .await?;
                thread_event_in(
                    tx,
                    batch,
                    &identity,
                    &thread,
                    "thread.handed_off",
                    json!({
                        "thread_id": thread.id,
                        "from": { "kind": "teammate", "id": from.principal_id },
                        "to": { "kind": "teammate", "id": joined.principal_id },
                        "notice_message_id": notice.id,
                    }),
                )
                .await?;
                Ok(HandOff {
                    from,
                    to: joined,
                    notice,
                })
            })
        })
        .await
    }

    /// The latest run a teammate had in a thread and the evidence it captured, which a
    /// handoff carries to the next teammate.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn last_run_evidence(
        &self,
        thread_id: &str,
        teammate_id: &str,
    ) -> Result<Option<(String, Vec<String>)>, CollaborationError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let run: Option<String> = sqlx::query_scalar(
            "SELECT r.id FROM runs r \
             JOIN agent_threads a ON a.tenant_id = r.tenant_id AND a.id = r.agent_thread_id \
             JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
             WHERE r.tenant_id = $1 AND a.definition_id = $2 AND m.thread_id = $3 \
             ORDER BY r.created_at DESC LIMIT 1",
        )
        .bind(&self.identity.tenant_id)
        .bind(teammate_id)
        .bind(thread_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(run) = run else {
            tx.commit().await?;
            return Ok(None);
        };
        let evidence: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM evidence WHERE tenant_id = $1 AND run_id = $2 ORDER BY captured_at, id",
        )
        .bind(&self.identity.tenant_id)
        .bind(&run)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some((run, evidence)))
    }

    async fn commit<T, F>(&self, mutation: F) -> Result<T, CollaborationError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxCollaborationFuture<'a, T>,
    {
        let rejection: Arc<Mutex<Option<CollaborationError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(&self.identity.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    match mutation(tx, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: COLLABORATION_OWNER,
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(error) => match rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                Some(error) => Err(error),
                None => Err(CollaborationError::Event(error)),
            },
        }
    }
}

async fn load_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    thread_id: &str,
    kind: ParticipantKind,
    principal_id: &str,
) -> Result<Option<Participant>, CollaborationError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM thread_participants \
         WHERE tenant_id = $1 AND thread_id = $2 AND kind = $3 AND principal_id = $4 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(thread_id)
    .bind(kind.as_str())
    .bind(principal_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(participant_from_row))
}

#[allow(clippy::too_many_arguments)]
async fn upsert_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    thread: &Thread,
    kind: ParticipantKind,
    principal_id: &str,
    role: ParticipantRole,
    reason: &str,
) -> Result<Participant, CollaborationError> {
    let id = format!("thp_{}", UlidGenerator::new().generate());
    let row = sqlx::query(&format!(
        "INSERT INTO thread_participants (id, tenant_id, thread_id, kind, principal_id, role) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (thread_id, kind, principal_id) DO UPDATE \
            SET role = EXCLUDED.role, left_at = NULL, \
                joined_at = CASE WHEN thread_participants.left_at IS NULL \
                                 THEN thread_participants.joined_at ELSE now() END \
         RETURNING {COLUMNS}"
    ))
    .bind(&id)
    .bind(&identity.tenant_id)
    .bind(&thread.id)
    .bind(kind.as_str())
    .bind(principal_id)
    .bind(role.as_str())
    .fetch_one(&mut **tx)
    .await?;
    let participant = participant_from_row(&row);
    thread_event_in(
        tx,
        batch,
        identity,
        thread,
        "thread.participant_added",
        json!({
            "thread_id": thread.id,
            "principal": { "kind": participant.kind, "id": participant.principal_id },
            "role": participant.role,
            "reason": reason,
        }),
    )
    .await?;
    Ok(participant)
}

async fn leave_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    thread: &Thread,
    kind: ParticipantKind,
    principal_id: &str,
    reason: &str,
) -> Result<Participant, CollaborationError> {
    let current = load_in(tx, &identity.tenant_id, &thread.id, kind, principal_id)
        .await?
        .filter(Participant::is_active)
        .ok_or_else(|| {
            CollaborationError::NotFound(format!(
                "{} {principal_id} is not an active participant of thread {}",
                kind.as_str(),
                thread.id
            ))
        })?;
    if current.role == "owner" {
        // The last owner may not leave people behind without one.
        let (owners, people): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE role = 'owner'), count(*) FROM thread_participants \
             WHERE tenant_id = $1 AND thread_id = $2 AND kind = 'user' AND left_at IS NULL \
               AND NOT (kind = $3 AND principal_id = $4)",
        )
        .bind(&identity.tenant_id)
        .bind(&thread.id)
        .bind(kind.as_str())
        .bind(principal_id)
        .fetch_one(&mut **tx)
        .await?;
        if owners == 0 && people > 0 {
            return Err(CollaborationError::Conflict(format!(
                "{principal_id} is the last owner of thread {}; make another participant an owner first",
                thread.id
            )));
        }
    }
    let row = sqlx::query(&format!(
        "UPDATE thread_participants SET left_at = now() \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(&identity.tenant_id)
    .bind(&current.id)
    .fetch_one(&mut **tx)
    .await?;
    let participant = participant_from_row(&row);
    thread_event_in(
        tx,
        batch,
        identity,
        thread,
        "thread.participant_removed",
        json!({
            "thread_id": thread.id,
            "principal": { "kind": participant.kind, "id": participant.principal_id },
            "role": participant.role,
            "reason": reason,
        }),
    )
    .await?;
    Ok(participant)
}

fn participant_from_row(row: &PgRow) -> Participant {
    Participant {
        id: row.get("id"),
        thread_id: row.get("thread_id"),
        kind: row.get("kind"),
        principal_id: row.get("principal_id"),
        role: row.get("role"),
        joined_at: row.get("joined_at"),
        left_at: row.get("left_at"),
    }
}
