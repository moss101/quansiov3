//! Notifications and preferences (DOMAIN.md §13.3).
//!
//! A notification belongs to one user in one workspace and carries a `kind`, a `ref` to
//! what it is about, the channels the user's preference selected when it was made, and the
//! channels it has been delivered on. Preferences are per user × workspace × kind; without
//! a row a kind goes to `in_app` and `desktop` (email is opt-in), and an empty list mutes
//! the kind. Every change emits a `notification.*` event naming the user, which the stream
//! delivers to that user only.

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

/// The kinds DOMAIN.md §13.3 names.
pub const NOTIFICATION_KINDS: [&str; 8] = [
    "needs_approval",
    "needs_answer",
    "blocked",
    "completed",
    "failed",
    "mention",
    "attention",
    "system",
];

/// The channels a person may choose.
pub const NOTIFICATION_CHANNELS: [&str; 3] = ["in_app", "desktop", "email"];

/// The channels a kind goes to when the person has not chosen.
pub const DEFAULT_CHANNELS: [&str; 2] = ["in_app", "desktop"];

const COLUMNS: &str = "id, workspace_id, user_id, kind, title, ref, channels, channels_delivered, \
                       read_at, created_at, source_event_id";

/// Why a notification command was refused.
#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    /// The event store or database refused.
    #[error("notification store: {0}")]
    Event(#[from] EventError),
    /// PostgreSQL rejected a statement.
    #[error("notification database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The request is malformed.
    #[error("notification schema: {0}")]
    Validation(String),
    /// No such notification for this user.
    #[error("notification {0} was not found")]
    NotFound(String),
}

impl NotifyError {
    /// DOMAIN.md §15 code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Event(_) | Self::Database(_) => "INTERNAL",
        }
    }
}

/// One notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// `ntf_…`.
    pub id: String,
    /// Workspace.
    pub workspace_id: String,
    /// Recipient.
    pub user_id: String,
    /// Kind.
    pub kind: String,
    /// One line for the person.
    pub title: Option<String>,
    /// What it is about.
    pub reference: Value,
    /// Channels selected.
    pub channels: Value,
    /// Channels delivered.
    pub channels_delivered: Value,
    /// When the person read it.
    pub read_at: Option<DateTime<Utc>>,
    /// When it was made.
    pub created_at: DateTime<Utc>,
    /// The RuntimeEvent it was made from.
    pub source_event_id: Option<String>,
}

impl Notification {
    /// The projection's spelling.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "workspace_id": self.workspace_id,
            "user_id": self.user_id,
            "kind": self.kind,
            "title": self.title,
            "ref": self.reference,
            "channels": self.channels,
            "channels_delivered": self.channels_delivered,
            "read_at": self.read_at,
            "created_at": self.created_at,
        })
    }
}

/// What a notification is made from.
#[derive(Debug, Clone)]
pub struct NewNotification {
    /// Workspace.
    pub workspace_id: String,
    /// Recipient.
    pub user_id: String,
    /// Kind (one of [`NOTIFICATION_KINDS`]).
    pub kind: &'static str,
    /// One line for the person.
    pub title: String,
    /// What it is about.
    pub reference: Value,
    /// The RuntimeEvent it comes from (the dedupe key with user and kind).
    pub source_event_id: Option<String>,
}

/// Refuse channels that are not channels, and kinds that are not kinds.
///
/// # Errors
/// Returns [`NotifyError::Validation`].
pub fn validate_preference(kind: &str, channels: &[String]) -> Result<(), NotifyError> {
    if !NOTIFICATION_KINDS.contains(&kind) {
        return Err(NotifyError::Validation(format!(
            "{kind:?} is not a notification kind"
        )));
    }
    for channel in channels {
        if !NOTIFICATION_CHANNELS.contains(&channel.as_str()) {
            return Err(NotifyError::Validation(format!(
                "{channel:?} is not a channel (in_app, desktop, email)"
            )));
        }
    }
    Ok(())
}

/// The channels a user's preference selects for a kind, read inside a transaction.
///
/// # Errors
/// Returns a database error.
pub async fn channels_for(
    conn: &mut PgConnection,
    tenant_id: &str,
    workspace_id: &str,
    user_id: &str,
    kind: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let chosen: Option<Value> = sqlx::query_scalar(
        "SELECT channels FROM notification_preferences \
         WHERE tenant_id = $1 AND workspace_id = $2 AND user_id = $3 AND kind = $4",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(user_id)
    .bind(kind)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match chosen {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|channel| NOTIFICATION_CHANNELS.contains(channel))
            .map(str::to_string)
            .collect(),
        _ => DEFAULT_CHANNELS
            .iter()
            .map(|channel| (*channel).to_string())
            .collect(),
    })
}

/// Make one notification inside the caller's transaction: nothing when the user muted the
/// kind or the same event already notified them of it; otherwise the row, a pending email
/// delivery when email is selected, and `notification.created`.
///
/// # Errors
/// Returns a database or event error.
pub async fn create_in(
    conn: &mut PgConnection,
    batch: &mut EventBatch,
    tenant_id: &str,
    new: &NewNotification,
) -> Result<Option<Notification>, EventError> {
    let channels = channels_for(conn, tenant_id, &new.workspace_id, &new.user_id, new.kind).await?;
    if channels.is_empty() {
        return Ok(None);
    }
    let delivered: Vec<&str> = channels
        .iter()
        .map(String::as_str)
        .filter(|channel| *channel == "in_app" || *channel == "desktop")
        .collect();
    let id = CanonicalId::generate(Prefix::Notification, &mut UlidGenerator::new()).to_string();
    let row = sqlx::query(&format!(
        "INSERT INTO notifications (id, tenant_id, workspace_id, user_id, kind, title, ref, channels, \
                                    channels_delivered, source_event_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT DO NOTHING RETURNING {COLUMNS}"
    ))
    .bind(&id)
    .bind(tenant_id)
    .bind(&new.workspace_id)
    .bind(&new.user_id)
    .bind(new.kind)
    .bind(&new.title)
    .bind(&new.reference)
    .bind(json!(channels))
    .bind(json!(delivered))
    .bind(&new.source_event_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let notification = notification_from_row(&row)?;
    if channels.iter().any(|channel| channel == "email") {
        sqlx::query(
            "INSERT INTO notification_deliveries (id, tenant_id, notification_id, channel) \
             VALUES ($1, $2, $3, 'email') ON CONFLICT DO NOTHING",
        )
        .bind(format!("ndl_{}", UlidGenerator::new().generate()))
        .bind(tenant_id)
        .bind(&notification.id)
        .execute(&mut *conn)
        .await?;
    }
    batch.emit(
        EventDraft::new(
            "notification",
            notification.id.clone(),
            1,
            EventType::parse("notification.created")?,
            CorrelationId::generate(&mut UlidGenerator::new()),
            Actor::system("notifier"),
        )
        .with_workspace(notification.workspace_id.clone())
        .with_payload(json!({
            "notification_id": notification.id,
            "user_id": notification.user_id,
            "kind": notification.kind,
            "title": notification.title,
            "ref": notification.reference,
            "channels": notification.channels,
            "desktop": channels.iter().any(|channel| channel == "desktop"),
        })),
    );
    Ok(Some(notification))
}

/// The notification owner for one tenant.
#[derive(Debug, Clone)]
pub struct NotificationStore {
    events: EventStore,
    tenant_id: String,
}

impl NotificationStore {
    /// A store over the pool for one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// A user's notifications in a workspace, newest first.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn list(
        &self,
        workspace_id: &str,
        user_id: &str,
        unread_only: bool,
        limit: i64,
    ) -> Result<Vec<Notification>, NotifyError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM notifications WHERE tenant_id = $1 AND workspace_id = $2 \
             AND user_id = $3 AND ($4 = FALSE OR read_at IS NULL) \
             ORDER BY created_at DESC, id DESC LIMIT $5"
        ))
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .bind(user_id)
        .bind(unread_only)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .iter()
            .map(notification_from_row)
            .collect::<Result<_, _>>()?)
    }

    /// Every kind's effective channels for a user in a workspace, and whether it was chosen.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn preferences(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, Vec<String>, bool)>, NotifyError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(
            "SELECT kind, channels FROM notification_preferences \
             WHERE tenant_id = $1 AND workspace_id = $2 AND user_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(NOTIFICATION_KINDS
            .iter()
            .map(|kind| {
                let chosen = rows
                    .iter()
                    .find(|row| row.get::<String, _>("kind") == *kind);
                match chosen {
                    Some(row) => (
                        (*kind).to_string(),
                        row.get::<Value, _>("channels")
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        true,
                    ),
                    None => (
                        (*kind).to_string(),
                        DEFAULT_CHANNELS
                            .iter()
                            .map(|channel| (*channel).to_string())
                            .collect(),
                        false,
                    ),
                }
            })
            .collect())
    }

    /// Set a user's channels for one kind (`UpdateNotificationPreferences`).
    ///
    /// # Errors
    /// Returns [`NotifyError::Validation`] for an unknown kind or channel.
    pub async fn set_preference(
        &self,
        workspace_id: &str,
        user_id: &str,
        kind: &str,
        channels: Vec<String>,
    ) -> Result<Vec<String>, NotifyError> {
        validate_preference(kind, &channels)?;
        let mut unique: Vec<String> = Vec::new();
        for channel in channels {
            if !unique.contains(&channel) {
                unique.push(channel);
            }
        }
        let tenant = self.tenant_id.clone();
        let workspace = workspace_id.to_string();
        let user = user_id.to_string();
        let kind = kind.to_string();
        let stored = unique.clone();
        self.events
            .commit_mutation(&self.tenant_id, move |conn, batch| {
                Box::pin(async move {
                    let id = format!("ntp_{}", UlidGenerator::new().generate());
                    sqlx::query(
                        "INSERT INTO notification_preferences (id, tenant_id, workspace_id, user_id, kind, channels) \
                         VALUES ($1, $2, $3, $4, $5, $6) \
                         ON CONFLICT (workspace_id, user_id, kind) DO UPDATE SET channels = EXCLUDED.channels",
                    )
                    .bind(&id)
                    .bind(&tenant)
                    .bind(&workspace)
                    .bind(&user)
                    .bind(&kind)
                    .bind(json!(stored))
                    .execute(&mut *conn)
                    .await?;
                    batch.emit(
                        EventDraft::new(
                            "notification",
                            format!("preferences:{user}:{workspace}"),
                            1,
                            EventType::parse("notification.preferences_updated")?,
                            CorrelationId::generate(&mut UlidGenerator::new()),
                            Actor::user(user.clone()),
                        )
                        .with_workspace(workspace.clone())
                        .with_payload(json!({ "user_id": user, "kind": kind, "channels": stored })),
                    );
                    Ok(())
                })
            })
            .await?;
        Ok(unique)
    }

    /// Mark one of the user's notifications read (`MarkNotificationRead`); marking it again
    /// changes nothing.
    ///
    /// # Errors
    /// Returns [`NotifyError::NotFound`] for another user's or a missing notification.
    pub async fn mark_read(
        &self,
        notification_id: &str,
        user_id: &str,
    ) -> Result<Notification, NotifyError> {
        let current = self.get(notification_id, user_id).await?;
        if current.read_at.is_some() {
            return Ok(current);
        }
        let tenant = self.tenant_id.clone();
        let id = notification_id.to_string();
        let user = user_id.to_string();
        let row = self
            .events
            .commit_mutation(&self.tenant_id, move |conn, batch| {
                Box::pin(async move {
                    let row = sqlx::query(&format!(
                        "UPDATE notifications SET read_at = COALESCE(read_at, now()) \
                         WHERE tenant_id = $1 AND id = $2 AND user_id = $3 RETURNING {COLUMNS}"
                    ))
                    .bind(&tenant)
                    .bind(&id)
                    .bind(&user)
                    .fetch_one(&mut *conn)
                    .await?;
                    let workspace: String = row.try_get("workspace_id")?;
                    batch.emit(
                        EventDraft::new(
                            "notification",
                            id.clone(),
                            2,
                            EventType::parse("notification.read")?,
                            CorrelationId::generate(&mut UlidGenerator::new()),
                            Actor::user(user.clone()),
                        )
                        .with_workspace(workspace)
                        .with_payload(json!({ "notification_id": id, "user_id": user })),
                    );
                    Ok(row)
                })
            })
            .await?;
        Ok(notification_from_row(&row)?)
    }

    /// One notification of a user.
    ///
    /// # Errors
    /// Returns [`NotifyError::NotFound`].
    pub async fn get(
        &self,
        notification_id: &str,
        user_id: &str,
    ) -> Result<Notification, NotifyError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM notifications WHERE tenant_id = $1 AND id = $2 AND user_id = $3"
        ))
        .bind(&self.tenant_id)
        .bind(notification_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref()
            .map(notification_from_row)
            .transpose()?
            .ok_or_else(|| NotifyError::NotFound(notification_id.to_string()))
    }
}

fn notification_from_row(row: &PgRow) -> Result<Notification, sqlx::Error> {
    Ok(Notification {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        user_id: row.try_get("user_id")?,
        kind: row.try_get("kind")?,
        title: row.try_get("title")?,
        reference: row.try_get("ref")?,
        channels: row.try_get("channels")?,
        channels_delivered: row.try_get("channels_delivered")?,
        read_at: row.try_get("read_at")?,
        created_at: row.try_get("created_at")?,
        source_event_id: row.try_get("source_event_id")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preference_names_a_kind_and_channels() {
        assert!(validate_preference("mention", &["in_app".into(), "email".into()]).is_ok());
        assert!(
            validate_preference("mention", &[]).is_ok(),
            "an empty list mutes the kind"
        );
        assert!(validate_preference("gossip", &["in_app".into()]).is_err());
        assert!(validate_preference("mention", &["sms".into()]).is_err());
    }
}
