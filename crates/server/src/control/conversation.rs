//! Conversation persistence: threads and messages (APP-001 command facade).
//!
//! Canonical owner for `threads` and `messages`. The public API records the command and
//! then calls this module; it never writes these tables itself. Every mutation commits
//! with its `thread.*` RuntimeEvent.

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
}

impl ConversationError {
    /// DOMAIN.md §15 code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) | Self::Schema(_) => "VALIDATION_BOUNDS",
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
    /// Author principal.
    pub author_id: String,
    /// Content blocks.
    pub content_blocks: Value,
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
        row.map(|row| Thread {
            id: row.get("id"),
            workspace_id: row.get("workspace_id"),
            kind: row.get("kind"),
            title: row.get("title"),
        })
        .ok_or_else(|| ConversationError::NotFound(thread_id.to_string()))
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
        let rows = sqlx::query(
            "SELECT id, thread_id, workspace_id, seq, author_id, content_blocks \
             FROM messages WHERE tenant_id = $1 AND thread_id = $2 \
             AND deleted_at IS NULL ORDER BY seq",
        )
        .bind(&self.identity.tenant_id)
        .bind(thread_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|row| Message {
                id: row.get("id"),
                thread_id: row.get("thread_id"),
                workspace_id: row.get("workspace_id"),
                seq: row.get("seq"),
                author_id: row.get("author_id"),
                content_blocks: row.get("content_blocks"),
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
        CanonicalId::parse_typed(workspace_id, Prefix::Workspace)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        let kind = match kind {
            "direct" | "group" | "objective" => kind.to_string(),
            _ => "direct".to_string(),
        };
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let workspace = workspace_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let id =
                    CanonicalId::generate(Prefix::Thread, &mut UlidGenerator::new()).to_string();
                sqlx::query(
                    "INSERT INTO threads (id, tenant_id, workspace_id, kind, title) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(&id)
                .bind(&tenant_id)
                .bind(&workspace)
                .bind(&kind)
                .bind(&title)
                .execute(&mut **tx)
                .await?;
                let thread = Thread {
                    id: id.clone(),
                    workspace_id: workspace.clone(),
                    kind,
                    title,
                };
                emit_thread_event(
                    &identity,
                    batch,
                    &thread,
                    "thread.created",
                    json!({
                        "thread_id": thread.id,
                        "workspace_id": thread.workspace_id,
                        "kind": thread.kind,
                    }),
                )?;
                Ok(thread)
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
        CanonicalId::parse_typed(workspace_id, Prefix::Workspace)
            .map_err(|error| ConversationError::Schema(error.to_string()))?;
        let thread = match thread_id {
            Some(id) => self.get_thread(id).await?,
            None => self.create_thread(workspace_id, "direct", None).await?,
        };
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let author = author_id.to_string();
        let blocks = json!([{ "type": "text", "text": content }]);
        let thread_for_insert = thread.clone();
        let message = self
            .commit(move |tx, batch| {
                Box::pin(async move {
                    let next: i64 = sqlx::query_scalar(
                        "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages \
                         WHERE tenant_id = $1 AND thread_id = $2",
                    )
                    .bind(&tenant_id)
                    .bind(&thread_for_insert.id)
                    .fetch_one(&mut **tx)
                    .await?;
                    let id = CanonicalId::generate(Prefix::Message, &mut UlidGenerator::new())
                        .to_string();
                    sqlx::query(
                        "INSERT INTO messages (id, tenant_id, workspace_id, thread_id, seq, \
                         author_kind, author_id, content_blocks) \
                         VALUES ($1, $2, $3, $4, $5, 'user', $6, $7)",
                    )
                    .bind(&id)
                    .bind(&tenant_id)
                    .bind(&thread_for_insert.workspace_id)
                    .bind(&thread_for_insert.id)
                    .bind(next)
                    .bind(&author)
                    .bind(&blocks)
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query(
                        "UPDATE threads SET last_activity_at = now(), updated_at = now() \
                         WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&tenant_id)
                    .bind(&thread_for_insert.id)
                    .execute(&mut **tx)
                    .await?;
                    let message = Message {
                        id: id.clone(),
                        thread_id: thread_for_insert.id.clone(),
                        workspace_id: thread_for_insert.workspace_id.clone(),
                        seq: next,
                        author_id: author,
                        content_blocks: blocks.clone(),
                    };
                    emit_thread_event(
                        &identity,
                        batch,
                        &thread_for_insert,
                        "thread.message_posted",
                        json!({
                            "thread_id": message.thread_id,
                            "message_id": message.id,
                            "seq": message.seq,
                            "workspace_id": message.workspace_id,
                        }),
                    )?;
                    Ok(message)
                })
            })
            .await?;
        Ok(PostedMessage { thread, message })
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

fn emit_thread_event(
    identity: &RuntimeIdentity,
    batch: &mut EventBatch,
    thread: &Thread,
    event_type: &str,
    payload: Value,
) -> Result<(), ConversationError> {
    let event_type = EventType::parse(event_type)?;
    let version = u64::try_from(batch.len() + 1).unwrap_or(1);
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
