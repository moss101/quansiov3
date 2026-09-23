//! Conversation persistence: threads and messages (APP-001 command facade, APP-004, APP-009).
//!
//! Canonical owner for `threads` and `messages`. The public API records the command and
//! then calls this module; it never writes these tables itself. Every mutation commits
//! with its `thread.*` RuntimeEvent.
//!
//! Three rules hold for every message (DOMAIN.md §3.2):
//!
//! * **`seq` is assigned under the thread's row lock.** Concurrent posts to one thread are
//!   serialized on the thread row, so each gets a distinct, gap-free sequence number in
//!   commit order, and the `thread.message_posted` events commit in the same order.
//! * **thread events carry the thread's own version.** `aggregate_version` continues from
//!   the thread's committed events and from what the same transaction already staged.
//! * **attention is announced, not stored.** A message that mentions someone emits
//!   `thread.attention_changed`; a user's own message after being mentioned emits the
//!   clearing change. The state itself is derived from the rows (APP-009).
//!
//! The `*_in` functions run inside a caller's transaction so the collaboration owner
//! (`control::collaboration`, which owns `thread_participants`) can create a thread with
//! its first participant, or post a handoff notice with the participant change, atomically
//! — each owner still writing only its own table.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_events::{EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::control::schema;
use crate::runtime::state_machine::RuntimeIdentity;

/// Repository path used to attribute a rejected mutation.
const CONVERSATION_OWNER: &str = "crates/server/src/control/conversation";

/// The most emoji text a reaction may carry.
pub const MAX_REACTION_BYTES: usize = 32;

type BoxConversationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ConversationError>> + Send + 'a>>;

/// Failures from the conversation owner.
#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    /// The event store refused the commit.
    #[error("conversation event store: {0}")]
    Event(#[from] EventError),
    /// PostgreSQL rejected a statement.
    #[error("conversation database error: {0}")]
    Database(#[from] sqlx::Error),
    /// A supplied identity is not canonical.
    #[error("conversation schema: {0}")]
    Schema(String),
    /// The named thread is not visible in this tenant.
    #[error("thread {0} was not found")]
    NotFound(String),
    /// The named message is not visible in this tenant (or was deleted).
    #[error("message {0} was not found")]
    MessageNotFound(String),
    /// A reaction change raced another and there was nothing left to change.
    #[error("the reaction is already in the requested state")]
    Unchanged,
}

impl ConversationError {
    /// DOMAIN.md §15 code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) | Self::Schema(_) => "VALIDATION_BOUNDS",
            Self::MessageNotFound(_) => "NOT_FOUND",
            Self::Unchanged => "CONFLICT_STATE",
            Self::Event(_) | Self::Database(_) => "INTERNAL",
        }
    }
}

/// A persisted thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// `thr_` identity.
    pub id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Thread kind.
    pub kind: String,
    /// Optional title.
    pub title: Option<String>,
}

/// A thread as the list projection shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSummary {
    /// `thr_` identity.
    pub id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Thread kind.
    pub kind: String,
    /// Optional title.
    pub title: Option<String>,
    /// Last activity.
    pub last_activity_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Non-deleted messages.
    pub message_count: i64,
}

/// A persisted message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// `msg_` identity.
    pub id: String,
    /// Thread the message belongs to.
    pub thread_id: String,
    /// Workspace the message belongs to.
    pub workspace_id: String,
    /// Thread-monotonic sequence.
    pub seq: i64,
    /// `user`, `agent` or `system`.
    pub author_kind: String,
    /// Author principal.
    pub author_id: String,
    /// Run that produced an agent message.
    pub run_id: Option<String>,
    /// Turn that produced an agent message.
    pub turn_id: Option<String>,
    /// Content blocks.
    pub content_blocks: Value,
    /// Attached artifact ids (`art_`), in the order the author attached them.
    pub attachments: Vec<String>,
    /// `[{kind: user|teammate, id}]`.
    pub mentions: Value,
    /// `[{emoji, user_id}]`.
    pub reactions: Value,
    /// When the message was written.
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Who a message mentions (DOMAIN.md §3.2 `mentions[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// `user` or `teammate`.
    pub kind: String,
    /// `usr_…` or `agt_…`.
    pub id: String,
}

impl Mention {
    fn to_json(&self) -> Value {
        json!({ "kind": self.kind, "id": self.id })
    }
}

/// A message to insert.
#[derive(Debug, Clone)]
pub struct NewMessage {
    /// `user`, `agent` or `system`.
    pub author_kind: &'static str,
    /// Author principal.
    pub author_id: String,
    /// Content blocks.
    pub content_blocks: Value,
    /// Attached artifact ids.
    pub attachments: Vec<String>,
    /// Who the message mentions.
    pub mentions: Vec<Mention>,
    /// Run that produced an agent message.
    pub run_id: Option<String>,
    /// Turn that produced an agent message.
    pub turn_id: Option<String>,
}

impl NewMessage {
    /// A plain text message from a user.
    #[must_use]
    pub fn user_text(author_id: impl Into<String>, content: &str) -> Self {
        Self {
            author_kind: "user",
            author_id: author_id.into(),
            content_blocks: json!([{ "type": "text", "text": content }]),
            attachments: Vec::new(),
            mentions: Vec::new(),
            run_id: None,
            turn_id: None,
        }
    }

    /// A system notice (DOMAIN.md §3.2 `system_notice` block) with its structured detail.
    #[must_use]
    pub fn system_notice(text: &str, notice: Value) -> Self {
        Self {
            author_kind: "system",
            author_id: "system".to_string(),
            content_blocks: json!([{ "type": "system_notice", "text": text, "notice": notice }]),
            attachments: Vec::new(),
            mentions: Vec::new(),
            run_id: None,
            turn_id: None,
        }
    }
}

/// Result of posting a message, possibly creating the thread.
#[derive(Debug, Clone, PartialEq)]
pub struct PostedMessage {
    /// Thread the message landed in.
    pub thread: Thread,
    /// The new message.
    pub message: Message,
}

/// Conversation owner for one tenant.
#[derive(Debug, Clone)]
pub struct ConversationStore {
    events: EventStore,
    identity: RuntimeIdentity,
}

const MESSAGE_COLUMNS: &str = "id, thread_id, workspace_id, seq, author_kind, author_id, run_id, \
                               turn_id, content_blocks, attachments, mentions, reactions, created_at";

impl ConversationStore {
    /// Bind the store to one tenant identity.
    ///
    /// # Errors
    /// Returns [`ConversationError::Schema`] when the tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, ConversationError> {
        schema::validate_tenant_id(&identity.tenant_id)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        Ok(Self {
            events: EventStore::new(pool),
            identity,
        })
    }

    /// The identity this store writes under.
    #[must_use]
    pub fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    /// Load a thread in this tenant.
    ///
    /// # Errors
    /// Returns [`ConversationError::NotFound`] when the thread is not visible.
    pub async fn get_thread(&self, thread_id: &str) -> Result<Thread, ConversationError> {
        CanonicalId::parse_typed(thread_id, Prefix::Thread)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let row = sqlx::query(
            "SELECT id, workspace_id, kind, title FROM threads \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.identity.tenant_id)
        .bind(thread_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.map(|row| thread_from_row(&row))
            .ok_or_else(|| ConversationError::NotFound(thread_id.to_string()))
    }

    /// Load one message that is not deleted.
    ///
    /// # Errors
    /// Returns [`ConversationError::MessageNotFound`] when it is not visible.
    pub async fn load_message(&self, message_id: &str) -> Result<Message, ConversationError> {
        CanonicalId::parse_typed(message_id, Prefix::Message)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL"
        ))
        .bind(&self.identity.tenant_id)
        .bind(message_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.map(|row| message_from_row(&row))
            .ok_or_else(|| ConversationError::MessageNotFound(message_id.to_string()))
    }

    /// The first message of a thread whose content blocks contain `fragment` (a JSONB
    /// containment match). A routine fire finds its own message by the `fire_key` in its
    /// notice, which is what lets a resumed fire continue instead of posting twice.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn find_by_block(
        &self,
        thread_id: &str,
        fragment: &Value,
    ) -> Result<Option<Message>, ConversationError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE tenant_id = $1 AND thread_id = $2 \
             AND content_blocks @> $3::jsonb ORDER BY seq LIMIT 1"
        ))
        .bind(&self.identity.tenant_id)
        .bind(thread_id)
        .bind(fragment)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.as_ref().map(message_from_row))
    }

    /// List messages in a thread, oldest first.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn list_messages(&self, thread_id: &str) -> Result<Vec<Message>, ConversationError> {
        CanonicalId::parse_typed(thread_id, Prefix::Thread)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE tenant_id = $1 AND thread_id = $2 \
             AND deleted_at IS NULL ORDER BY seq"
        ))
        .bind(&self.identity.tenant_id)
        .bind(thread_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.iter().map(message_from_row).collect())
    }

    /// The workspace's threads, most recently active first (APP-004 thread list).
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn list_threads(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ThreadSummary>, ConversationError> {
        CanonicalId::parse_typed(workspace_id, Prefix::Workspace)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let rows = sqlx::query(
            "SELECT t.id, t.workspace_id, t.kind, t.title, t.last_activity_at, \
                    (SELECT count(*) FROM messages m WHERE m.thread_id = t.id AND m.deleted_at IS NULL) AS message_count \
             FROM threads t WHERE t.tenant_id = $1 AND t.workspace_id = $2 AND t.archived_at IS NULL \
             ORDER BY t.last_activity_at DESC NULLS LAST, t.created_at DESC LIMIT 200",
        )
        .bind(&self.identity.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|row| ThreadSummary {
                id: row.get("id"),
                workspace_id: row.get("workspace_id"),
                kind: row.get("kind"),
                title: row.get("title"),
                last_activity_at: row.get("last_activity_at"),
                message_count: row.get("message_count"),
            })
            .collect())
    }

    /// Create a thread.
    ///
    /// # Errors
    /// Returns a schema or database error.
    pub async fn create_thread(
        &self,
        workspace_id: &str,
        kind: &str,
        title: Option<String>,
    ) -> Result<Thread, ConversationError> {
        let identity = self.identity.clone();
        let workspace = workspace_id.to_string();
        let kind = kind.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                create_thread_in(tx, batch, &identity, &workspace, &kind, title).await
            })
        })
        .await
    }

    /// Post a message, creating a direct thread when none is named.
    ///
    /// # Errors
    /// Returns [`ConversationError::NotFound`] when a named thread is missing.
    pub async fn post_message(
        &self,
        workspace_id: &str,
        thread_id: Option<&str>,
        author_id: &str,
        content: &str,
    ) -> Result<PostedMessage, ConversationError> {
        self.post_message_with_attachments(workspace_id, thread_id, author_id, content, &[])
            .await
    }

    /// Post a message carrying artifact attachments (APP-004: upload → Artifact → message).
    ///
    /// Every attachment must be an `art_` id of an artifact in `workspace_id` that is not
    /// deleted; otherwise nothing is posted.
    ///
    /// # Errors
    /// Returns [`ConversationError::Schema`] for an unknown or foreign attachment.
    pub async fn post_message_with_attachments(
        &self,
        workspace_id: &str,
        thread_id: Option<&str>,
        author_id: &str,
        content: &str,
        attachments: &[String],
    ) -> Result<PostedMessage, ConversationError> {
        self.check_attachments(workspace_id, attachments).await?;
        let thread = match thread_id {
            Some(id) => self.get_thread(id).await?,
            None => self.create_thread(workspace_id, "direct", None).await?,
        };
        let mut new = NewMessage::user_text(author_id, content);
        new.attachments = attachments.to_vec();
        let message = self.insert_message(&thread, new).await?;
        Ok(PostedMessage { thread, message })
    }

    /// Refuse attachments that are not live artifacts of the workspace.
    ///
    /// # Errors
    /// Returns [`ConversationError::Schema`] for an unknown or foreign attachment.
    pub async fn check_attachments(
        &self,
        workspace_id: &str,
        attachments: &[String],
    ) -> Result<(), ConversationError> {
        CanonicalId::parse_typed(workspace_id, Prefix::Workspace)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        for artifact_id in attachments {
            if !artifact_id.starts_with("art_") {
                return Err(ConversationError::Schema(format!(
                    "attachment {artifact_id} is not an artifact id"
                )));
            }
        }
        if attachments.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.identity.tenant_id)
            .await?;
        let known: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM artifacts WHERE tenant_id = $1 AND workspace_id = $2 \
             AND id = ANY($3) AND deleted_at IS NULL",
        )
        .bind(&self.identity.tenant_id)
        .bind(workspace_id)
        .bind(attachments)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let known: std::collections::HashSet<String> = known.into_iter().map(|(id,)| id).collect();
        if let Some(missing) = attachments.iter().find(|id| !known.contains(*id)) {
            return Err(ConversationError::Schema(format!(
                "attachment {missing} is not an artifact of workspace {workspace_id}"
            )));
        }
        Ok(())
    }

    /// Post the agent's reply to a thread (APP-004): the durable assistant message a run's
    /// turn produced, stamped with the run and turn so the inspector can find it.
    ///
    /// # Errors
    /// Returns [`ConversationError::NotFound`] when the thread is missing.
    pub async fn post_agent_message(
        &self,
        thread_id: &str,
        agent_id: &str,
        content: &str,
        run_id: &str,
        turn_id: &str,
    ) -> Result<Message, ConversationError> {
        let thread = self.get_thread(thread_id).await?;
        self.insert_message(
            &thread,
            NewMessage {
                author_kind: "agent",
                author_id: agent_id.to_string(),
                content_blocks: json!([{ "type": "text", "text": content }]),
                attachments: Vec::new(),
                mentions: Vec::new(),
                run_id: Some(run_id.to_string()),
                turn_id: Some(turn_id.to_string()),
            },
        )
        .await
    }

    /// Insert one message into an existing thread in its own transaction.
    ///
    /// # Errors
    /// Returns [`ConversationError::NotFound`] when the thread disappeared.
    pub async fn insert_message(
        &self,
        thread: &Thread,
        new: NewMessage,
    ) -> Result<Message, ConversationError> {
        let identity = self.identity.clone();
        let thread_id = thread.id.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let thread = lock_thread_in(tx, &identity.tenant_id, &thread_id).await?;
                insert_message_in(tx, batch, &identity, &thread, new).await
            })
        })
        .await
    }

    /// Add or remove one user's reaction (DOMAIN.md §3.2 `reactions[] ({emoji, user_id})`).
    ///
    /// Idempotent: asking for the state the message is already in changes nothing and emits
    /// nothing, and the second value is `false`. A change emits `thread.reaction_added` or
    /// `thread.reaction_removed` with the message.
    ///
    /// # Errors
    /// Returns [`ConversationError::MessageNotFound`] for a missing or deleted message and
    /// [`ConversationError::Schema`] for an emoji that is empty, too long or not printable.
    pub async fn react(
        &self,
        message_id: &str,
        user_id: &str,
        emoji: &str,
        remove: bool,
    ) -> Result<(Message, bool), ConversationError> {
        validate_emoji(emoji)?;
        let current = self.load_message(message_id).await?;
        if next_reactions(&current.reactions, user_id, emoji, remove).is_none() {
            return Ok((current, false));
        }
        let identity = self.identity.clone();
        let message_id_owned = message_id.to_string();
        let user = user_id.to_string();
        let emoji_owned = emoji.to_string();
        let result = self
            .commit(move |tx, batch| {
                Box::pin(async move {
                    let row = sqlx::query(&format!(
                        "SELECT {MESSAGE_COLUMNS} FROM messages \
                         WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE"
                    ))
                    .bind(&identity.tenant_id)
                    .bind(&message_id_owned)
                    .fetch_optional(&mut **tx)
                    .await?
                    .ok_or_else(|| ConversationError::MessageNotFound(message_id_owned.clone()))?;
                    let mut message = message_from_row(&row);
                    let Some(reactions) =
                        next_reactions(&message.reactions, &user, &emoji_owned, remove)
                    else {
                        return Err(ConversationError::Unchanged);
                    };
                    sqlx::query(
                        "UPDATE messages SET reactions = $3, updated_at = now() \
                         WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&identity.tenant_id)
                    .bind(&message.id)
                    .bind(&reactions)
                    .execute(&mut **tx)
                    .await?;
                    message.reactions = reactions;
                    let thread =
                        lock_thread_in(tx, &identity.tenant_id, &message.thread_id).await?;
                    thread_event_in(
                        tx,
                        batch,
                        &identity,
                        &thread,
                        if remove {
                            "thread.reaction_removed"
                        } else {
                            "thread.reaction_added"
                        },
                        json!({
                            "thread_id": thread.id,
                            "message_id": message.id,
                            "seq": message.seq,
                            "emoji": emoji_owned,
                            "user_id": user,
                        }),
                    )
                    .await?;
                    Ok(message)
                })
            })
            .await;
        match result {
            Ok(message) => Ok((message, true)),
            Err(ConversationError::Unchanged) => Ok((self.load_message(message_id).await?, false)),
            Err(error) => Err(error),
        }
    }

    async fn commit<T, F>(&self, mutation: F) -> Result<T, ConversationError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxConversationFuture<'a, T>,
    {
        let rejection: Arc<Mutex<Option<ConversationError>>> = Arc::new(Mutex::new(None));
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
                                owner: CONVERSATION_OWNER,
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
                None => Err(ConversationError::Event(error)),
            },
        }
    }
}

// ------------------------------------------------------------------ in-transaction owners

/// Create a thread inside the caller's transaction and stage `thread.created`.
///
/// # Errors
/// Returns [`ConversationError::Schema`] for a non-canonical workspace id.
pub async fn create_thread_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    workspace_id: &str,
    kind: &str,
    title: Option<String>,
) -> Result<Thread, ConversationError> {
    CanonicalId::parse_typed(workspace_id, Prefix::Workspace)
        .map_err(|error| ConversationError::Schema(error.to_string()))?;
    let kind = match kind {
        "direct" | "group" | "objective" => kind.to_string(),
        _ => "direct".to_string(),
    };
    let id = CanonicalId::generate(Prefix::Thread, &mut UlidGenerator::new()).to_string();
    sqlx::query(
        "INSERT INTO threads (id, tenant_id, workspace_id, kind, title) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(&identity.tenant_id)
    .bind(workspace_id)
    .bind(&kind)
    .bind(&title)
    .execute(&mut **tx)
    .await?;
    let thread = Thread {
        id,
        workspace_id: workspace_id.to_string(),
        kind,
        title,
    };
    thread_event_in(
        tx,
        batch,
        identity,
        &thread,
        "thread.created",
        json!({
            "thread_id": thread.id,
            "workspace_id": thread.workspace_id,
            "kind": thread.kind,
        }),
    )
    .await?;
    Ok(thread)
}

/// Lock a thread row for the rest of the caller's transaction.
///
/// Every message insert takes this lock first, which is what serializes `seq` and the
/// thread's event versions.
///
/// # Errors
/// Returns [`ConversationError::NotFound`] when the thread is not visible.
pub async fn lock_thread_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    thread_id: &str,
) -> Result<Thread, ConversationError> {
    CanonicalId::parse_typed(thread_id, Prefix::Thread)
        .map_err(|error| ConversationError::Schema(error.to_string()))?;
    let row = sqlx::query(
        "SELECT id, workspace_id, kind, title FROM threads \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| ConversationError::NotFound(thread_id.to_string()))?;
    Ok(thread_from_row(&row))
}

/// Insert a message into a thread the caller has locked ([`lock_thread_in`]) and stage its
/// events: `thread.message_posted`, one `thread.attention_changed` per mention, and the
/// clearing change when a mentioned user answers.
///
/// # Errors
/// Returns a database error.
pub async fn insert_message_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    thread: &Thread,
    new: NewMessage,
) -> Result<Message, ConversationError> {
    let tenant_id = identity.tenant_id.clone();
    let next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE tenant_id = $1 AND thread_id = $2",
    )
    .bind(&tenant_id)
    .bind(&thread.id)
    .fetch_one(&mut **tx)
    .await?;
    // A user answering after being mentioned clears their attention: were they mentioned
    // after their own last message?
    let clears_attention = if new.author_kind == "user" {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM messages m WHERE m.tenant_id = $1 AND m.thread_id = $2 \
                 AND m.deleted_at IS NULL AND m.mentions @> $4::jsonb \
                 AND m.seq > COALESCE((SELECT MAX(o.seq) FROM messages o WHERE o.tenant_id = $1 \
                     AND o.thread_id = $2 AND o.author_kind = 'user' AND o.author_id = $3), 0))",
        )
        .bind(&tenant_id)
        .bind(&thread.id)
        .bind(&new.author_id)
        .bind(json!([{ "kind": "user", "id": new.author_id }]))
        .fetch_one(&mut **tx)
        .await?
    } else {
        false
    };
    let id = CanonicalId::generate(Prefix::Message, &mut UlidGenerator::new()).to_string();
    let mentions = Value::Array(new.mentions.iter().map(Mention::to_json).collect());
    let row = sqlx::query(&format!(
        "INSERT INTO messages (id, tenant_id, workspace_id, thread_id, seq, author_kind, author_id, \
         run_id, turn_id, content_blocks, attachments, mentions) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING {MESSAGE_COLUMNS}"
    ))
    .bind(&id)
    .bind(&tenant_id)
    .bind(&thread.workspace_id)
    .bind(&thread.id)
    .bind(next)
    .bind(new.author_kind)
    .bind(&new.author_id)
    .bind(&new.run_id)
    .bind(&new.turn_id)
    .bind(&new.content_blocks)
    .bind(json!(new.attachments))
    .bind(&mentions)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE threads SET last_activity_at = now(), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&tenant_id)
    .bind(&thread.id)
    .execute(&mut **tx)
    .await?;
    let message = message_from_row(&row);
    thread_event_in(
        tx,
        batch,
        identity,
        thread,
        "thread.message_posted",
        json!({
            "thread_id": message.thread_id,
            "message_id": message.id,
            "seq": message.seq,
            "workspace_id": message.workspace_id,
            "author_kind": message.author_kind,
            "author_id": message.author_id,
            "run_id": message.run_id,
            "mentions": message.mentions,
        }),
    )
    .await?;
    for mention in &new.mentions {
        thread_event_in(
            tx,
            batch,
            identity,
            thread,
            "thread.attention_changed",
            json!({
                "thread_id": thread.id,
                "principal": mention.to_json(),
                "attention": "mentioned",
                "message_id": message.id,
                "seq": message.seq,
            }),
        )
        .await?;
    }
    if clears_attention {
        thread_event_in(
            tx,
            batch,
            identity,
            thread,
            "thread.attention_changed",
            json!({
                "thread_id": thread.id,
                "principal": { "kind": "user", "id": message.author_id },
                "attention": "none",
                "message_id": message.id,
                "seq": message.seq,
            }),
        )
        .await?;
    }
    Ok(message)
}

/// Stage one `thread.*` event whose version continues the thread's committed and staged
/// versions.
///
/// # Errors
/// Returns a database error, or [`ConversationError::Event`] for a malformed type.
pub async fn thread_event_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    thread: &Thread,
    event_type: &str,
    payload: Value,
) -> Result<(), ConversationError> {
    let event_type = EventType::parse(event_type)?;
    let committed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(aggregate_version), 0) FROM runtime_events \
         WHERE tenant_id = $1 AND aggregate_type = 'thread' AND aggregate_id = $2",
    )
    .bind(&identity.tenant_id)
    .bind(&thread.id)
    .fetch_one(&mut **tx)
    .await?;
    let staged = batch.staged_version("thread", &thread.id).unwrap_or(0);
    let version = u64::try_from(committed).unwrap_or(0).max(staged) + 1;
    let mut draft = EventDraft::new(
        "thread",
        thread.id.clone(),
        version,
        event_type,
        identity.correlation_id,
        identity.actor.clone(),
    )
    .with_workspace(thread.workspace_id.clone())
    .with_payload(payload);
    if let Some(command_id) = identity.command_id {
        draft = draft.with_command_id(command_id);
    }
    batch.emit(draft);
    Ok(())
}

// ------------------------------------------------------------------ reactions

/// Refuse a reaction that is empty, too long, or carries whitespace or control characters.
///
/// # Errors
/// Returns [`ConversationError::Schema`].
pub fn validate_emoji(emoji: &str) -> Result<(), ConversationError> {
    if emoji.is_empty()
        || emoji.len() > MAX_REACTION_BYTES
        || emoji.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ConversationError::Schema(format!(
            "a reaction is one emoji of at most {MAX_REACTION_BYTES} bytes without spaces"
        )));
    }
    Ok(())
}

/// The reactions after the change, or `None` when the message is already in that state.
#[must_use]
pub fn next_reactions(current: &Value, user_id: &str, emoji: &str, remove: bool) -> Option<Value> {
    let mut reactions: Vec<Value> = current.as_array().cloned().unwrap_or_default();
    let matches = |reaction: &Value| {
        reaction.get("emoji").and_then(Value::as_str) == Some(emoji)
            && reaction.get("user_id").and_then(Value::as_str) == Some(user_id)
    };
    let present = reactions.iter().any(matches);
    match (present, remove) {
        (true, false) | (false, true) => None,
        (false, false) => {
            reactions.push(json!({ "emoji": emoji, "user_id": user_id }));
            Some(Value::Array(reactions))
        }
        (true, true) => {
            reactions.retain(|reaction| !matches(reaction));
            Some(Value::Array(reactions))
        }
    }
}

fn thread_from_row(row: &sqlx::postgres::PgRow) -> Thread {
    Thread {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        kind: row.get("kind"),
        title: row.get("title"),
    }
}

fn message_from_row(row: &sqlx::postgres::PgRow) -> Message {
    let attachments: Value = row.get("attachments");
    Message {
        id: row.get("id"),
        thread_id: row.get("thread_id"),
        workspace_id: row.get("workspace_id"),
        seq: row.get("seq"),
        author_kind: row.get("author_kind"),
        author_id: row.get("author_id"),
        run_id: row.get("run_id"),
        turn_id: row.get("turn_id"),
        content_blocks: row.get("content_blocks"),
        attachments: attachments
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        mentions: row.get("mentions"),
        reactions: row.get("reactions"),
        created_at: row.get("created_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_are_idempotent_per_user_and_emoji() {
        let empty = json!([]);
        let added = next_reactions(&empty, "usr_a", "👍", false).expect("added");
        assert_eq!(added, json!([{ "emoji": "👍", "user_id": "usr_a" }]));
        assert!(next_reactions(&added, "usr_a", "👍", false).is_none());
        let both = next_reactions(&added, "usr_b", "👍", false).expect("second user");
        assert_eq!(both.as_array().map(Vec::len), Some(2));
        let removed = next_reactions(&both, "usr_a", "👍", true).expect("removed");
        assert_eq!(removed, json!([{ "emoji": "👍", "user_id": "usr_b" }]));
        assert!(next_reactions(&removed, "usr_a", "👍", true).is_none());
    }

    #[test]
    fn a_reaction_is_one_short_printable_token() {
        assert!(validate_emoji("🎉").is_ok());
        assert!(validate_emoji("+1").is_ok());
        for bad in ["", " ", "a b", "\u{7}", &"x".repeat(MAX_REACTION_BYTES + 1)] {
            assert!(validate_emoji(bad).is_err(), "{bad:?}");
        }
    }
}
