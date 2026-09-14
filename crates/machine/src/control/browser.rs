//! Browser sessions: the durable row a takeover and a resume read (EXEC-009, DOMAIN.md §8.4).
//!
//! `browser_sessions` is control-plane state — it names an execution target and a run and it outlives the
//! worker driving it — so it is written here, beside the target and lease tables, and not by qworkerd.
//! That is not a preference: the architecture gate forbids `crates/qworkerd` a Postgres client at all,
//! because a worker reaches the server only over its fenced channel.
//!
//! The rules §8.4 states are enforced at this boundary rather than left to the worker, so two callers
//! cannot disagree about who holds the browser:
//!
//! * **a takeover fences agent input and does not open a second session.** Control moves to the user, the
//!   status becomes `paused_takeover`, and the row — the tab, the profile, the page — is the same row. A
//!   takeover of a session that is not active is refused rather than queued.
//! * **a handback only returns what a person holds.** It is refused unless the user holds control, so it
//!   cannot be used to resume a session that policy paused.
//! * **the tab list follows the page.** Recording a page updates `current_url` and upserts the tab in one
//!   transaction, so the two cannot drift apart.

use serde_json::{json, Value};
use sqlx::{Acquire, PgConnection, Row};

use crate::control::MachineError;

/// Who is driving a session (the schema's `browser_sessions.control_holder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlHolder {
    /// The agent.
    Agent,
    /// The user, after a takeover.
    User,
    /// Nobody: paused by policy, or closed.
    Nobody,
}

impl ControlHolder {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::Nobody => "none",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            "none" => Some(Self::Nobody),
            _ => None,
        }
    }
}

/// A session's lifecycle (the schema's `browser_sessions.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Usable.
    Active,
    /// The user holds it.
    PausedTakeover,
    /// Policy paused it.
    PausedPolicy,
    /// Closed.
    Closed,
}

impl SessionStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::PausedTakeover => "paused_takeover",
            Self::PausedPolicy => "paused_policy",
            Self::Closed => "closed",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "paused_takeover" => Some(Self::PausedTakeover),
            "paused_policy" => Some(Self::PausedPolicy),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// One tab the session has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// The CDP target id.
    pub target_id: String,
    /// Its url.
    pub url: String,
    /// Its title.
    pub title: String,
}

/// A browser session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSession {
    /// `bsn_…` identity.
    pub id: String,
    /// The tenant that owns it.
    pub tenant_id: String,
    /// The execution target it runs on.
    pub target_id: String,
    /// The run it belongs to, when it belongs to one.
    pub run_id: Option<String>,
    /// The persistent profile it uses.
    pub profile_ref: Option<String>,
    /// Who is driving.
    pub control_holder: ControlHolder,
    /// When control last changed hands.
    pub control_since: Option<String>,
    /// The page it is on.
    pub current_url: Option<String>,
    /// Its open tabs.
    pub tabs: Vec<Tab>,
    /// Whether frames are streamed, and under what reference.
    pub screencast: Value,
    /// The checkpoint it was last saved from.
    pub checkpoint_id: Option<String>,
    /// Its lifecycle state.
    pub status: SessionStatus,
}

impl BrowserSession {
    /// Whether the agent may drive this session.
    #[must_use]
    pub const fn agent_may_drive(&self) -> bool {
        matches!(self.status, SessionStatus::Active)
            && matches!(self.control_holder, ControlHolder::Agent)
    }
}

/// Why a browser-session operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BrowserSessionError {
    /// The database refused or was unreachable.
    #[error("browser session store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant has no such session.
    #[error("browser session {0} does not exist for this tenant")]
    NotFound(String),
    /// The target the session names does not exist for this tenant.
    #[error("execution target {0} does not exist for this tenant")]
    TargetNotFound(String),
    /// The session identity is not a `bsn_` id.
    #[error("{0:?} is not a browser session id")]
    SessionIdInvalid(String),
    /// The session is not in a state that allows the operation.
    #[error("browser session {id} is {status} and cannot {attempted}")]
    NotAllowed {
        /// The session.
        id: String,
        /// Its state.
        status: &'static str,
        /// What was attempted.
        attempted: &'static str,
    },
    /// A handback was attempted by someone who does not hold the session.
    #[error("browser session {id} is held by {holder}, so it cannot be handed back")]
    NotHeld {
        /// The session.
        id: String,
        /// Who holds it.
        holder: &'static str,
    },
    /// A stored row is not one the domain defines.
    #[error("the stored session is not one the domain defines: {detail}")]
    Corrupt {
        /// What is wrong with it.
        detail: String,
    },
}

impl From<BrowserSessionError> for MachineError {
    fn from(error: BrowserSessionError) -> Self {
        MachineError::Browser(error.to_string())
    }
}

const COLUMNS: &str = "id, tenant_id, target_id, run_id, profile_ref, control_holder, \
                       to_char(control_since AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS control_since, \
                       current_url, tabs, screencast, checkpoint_id, status";

/// The browser session store.
pub struct BrowserSessionStore;

impl BrowserSessionStore {
    /// Open a session on a target, with the agent holding control.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::SessionIdInvalid`] for an id that is not a `bsn_` id,
    /// [`BrowserSessionError::TargetNotFound`] when the tenant has no such target.
    pub async fn open(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        target_id: &str,
        run_id: Option<&str>,
        profile_ref: Option<&str>,
    ) -> Result<BrowserSession, BrowserSessionError> {
        if !session_id.starts_with("bsn_") {
            return Err(BrowserSessionError::SessionIdInvalid(
                session_id.to_string(),
            ));
        }
        let target =
            sqlx::query("SELECT id FROM execution_targets WHERE id = $1 AND tenant_id = $2")
                .bind(target_id)
                .bind(tenant_id)
                .fetch_optional(&mut *conn)
                .await?;
        if target.is_none() {
            return Err(BrowserSessionError::TargetNotFound(target_id.to_string()));
        }
        sqlx::query(
            "INSERT INTO browser_sessions (id, tenant_id, target_id, run_id, profile_ref, \
                                           control_holder, status) \
             VALUES ($1, $2, $3, $4, $5, 'agent', 'active')",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(target_id)
        .bind(run_id)
        .bind(profile_ref)
        .execute(&mut *conn)
        .await?;
        Self::load(conn, tenant_id, session_id).await
    }

    /// Load a session.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotFound`] when the tenant has no such session, and
    /// [`BrowserSessionError::Corrupt`] when the stored row is not one the domain defines.
    pub async fn load(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let sql =
            format!("SELECT {COLUMNS} FROM browser_sessions WHERE id = $1 AND tenant_id = $2");
        let row = sqlx::query(&sql)
            .bind(session_id)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| BrowserSessionError::NotFound(session_id.to_string()))?;
        row_to_session(&row)
    }

    /// Every session of a target, newest first.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::Database`] when the read fails.
    pub async fn list_for_target(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
    ) -> Result<Vec<BrowserSession>, BrowserSessionError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM browser_sessions WHERE tenant_id = $1 AND target_id = $2 \
             ORDER BY created_at DESC, id"
        );
        let rows = sqlx::query(&sql)
            .bind(tenant_id)
            .bind(target_id)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter().map(row_to_session).collect()
    }

    /// Record the page the session is on, keeping the tab list in step.
    ///
    /// One transaction, and the row is locked while it is read: the tab upsert and the current url cannot
    /// then disagree.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotFound`] for an unknown session.
    pub async fn record_page(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        target_id: &str,
        url: &str,
        title: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let mut tx = conn.begin().await?;
        let sql = format!(
            "SELECT {COLUMNS} FROM browser_sessions WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
        );
        let row = sqlx::query(&sql)
            .bind(session_id)
            .bind(tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| BrowserSessionError::NotFound(session_id.to_string()))?;
        let mut session = row_to_session(&row)?;
        session.current_url = Some(url.to_string());
        match session
            .tabs
            .iter_mut()
            .find(|tab| tab.target_id == target_id)
        {
            Some(tab) => {
                tab.url = url.to_string();
                tab.title = title.to_string();
            }
            None => session.tabs.push(Tab {
                target_id: target_id.to_string(),
                url: url.to_string(),
                title: title.to_string(),
            }),
        }
        let tabs = Value::Array(
            session
                .tabs
                .iter()
                .map(|tab| {
                    json!({
                        "target_id": tab.target_id,
                        "url": tab.url,
                        "title": tab.title,
                    })
                })
                .collect(),
        );
        sqlx::query(
            "UPDATE browser_sessions SET current_url = $3, tabs = $4 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(url)
        .bind(&tabs)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    /// Give control to the user, fencing agent input.
    ///
    /// The session is paused, not duplicated: §8.4 is explicit that a takeover never creates a second
    /// session, so the tab and the profile stay.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotAllowed`] for a session that is not active.
    pub async fn request_takeover(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        at: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let session = Self::load(conn, tenant_id, session_id).await?;
        if !matches!(session.status, SessionStatus::Active) {
            return Err(BrowserSessionError::NotAllowed {
                id: session.id,
                status: session.status.as_str(),
                attempted: "be taken over",
            });
        }
        Self::set_control(
            conn,
            tenant_id,
            session_id,
            ControlHolder::User,
            SessionStatus::PausedTakeover,
            at,
        )
        .await
    }

    /// Return control to the agent.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotHeld`] when a person does not hold the session, so a handback
    /// cannot resume a session that policy paused.
    pub async fn handback(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        at: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let session = Self::load(conn, tenant_id, session_id).await?;
        if !matches!(session.control_holder, ControlHolder::User) {
            return Err(BrowserSessionError::NotHeld {
                id: session.id,
                holder: session.control_holder.as_str(),
            });
        }
        Self::set_control(
            conn,
            tenant_id,
            session_id,
            ControlHolder::Agent,
            SessionStatus::Active,
            at,
        )
        .await
    }

    /// Pause the session for a policy reason. No handback resumes it: policy decides.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotFound`] for an unknown session.
    pub async fn pause_for_policy(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        at: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        Self::set_control(
            conn,
            tenant_id,
            session_id,
            ControlHolder::Nobody,
            SessionStatus::PausedPolicy,
            at,
        )
        .await
    }

    /// Close the session.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotFound`] for an unknown session.
    pub async fn close(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        at: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let updated = sqlx::query(
            "UPDATE browser_sessions SET status = 'closed', control_holder = 'none', \
                                        control_since = $3::timestamptz, screencast = '{}'::jsonb \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(at)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(BrowserSessionError::NotFound(session_id.to_string()));
        }
        Self::load(conn, tenant_id, session_id).await
    }

    /// Note the checkpoint a session was saved from, so a resume knows where it was.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotFound`] for an unknown session.
    pub async fn attach_checkpoint(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let updated = sqlx::query(
            "UPDATE browser_sessions SET checkpoint_id = $3 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(checkpoint_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(BrowserSessionError::NotFound(session_id.to_string()));
        }
        Self::load(conn, tenant_id, session_id).await
    }

    async fn set_control(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        holder: ControlHolder,
        status: SessionStatus,
        at: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let updated = sqlx::query(
            "UPDATE browser_sessions SET control_holder = $3, status = $4, \
                                        control_since = $5::timestamptz \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(holder.as_str())
        .bind(status.as_str())
        .bind(at)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(BrowserSessionError::NotFound(session_id.to_string()));
        }
        Self::load(conn, tenant_id, session_id).await
    }
}

fn row_to_session(row: &sqlx::postgres::PgRow) -> Result<BrowserSession, BrowserSessionError> {
    let holder: String = row.try_get("control_holder")?;
    let status: String = row.try_get("status")?;
    let tabs: Value = row.try_get("tabs")?;
    let tabs = tabs
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|tab| {
                    Some(Tab {
                        target_id: tab.get("target_id")?.as_str()?.to_string(),
                        url: tab.get("url")?.as_str()?.to_string(),
                        title: tab
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(BrowserSession {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        target_id: row.try_get("target_id")?,
        run_id: row.try_get("run_id")?,
        profile_ref: row.try_get("profile_ref")?,
        control_holder: ControlHolder::parse(&holder).ok_or_else(|| {
            BrowserSessionError::Corrupt {
                detail: format!("control_holder {holder:?} is not one the domain defines"),
            }
        })?,
        control_since: row.try_get("control_since")?,
        current_url: row.try_get("current_url")?,
        tabs,
        screencast: row.try_get("screencast")?,
        checkpoint_id: row.try_get("checkpoint_id")?,
        status: SessionStatus::parse(&status).ok_or_else(|| BrowserSessionError::Corrupt {
            detail: format!("status {status:?} is not one the domain defines"),
        })?,
    })
}
