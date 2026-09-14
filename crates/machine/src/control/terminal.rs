//! Terminal sessions: the durable cursor a reconnect resumes from (EXEC-006, DOMAIN.md §8.5).
//!
//! The session row is control-plane state — it names an execution target and a run, and it survives the
//! worker that produced it — so it is written here, in the same module that owns the target and lease
//! tables, and not by qworkerd. That is not a preference: the architecture gate forbids `crates/qworkerd`
//! from holding a Postgres client at all, because a worker reaches the server only over its fenced
//! channel. The worker keeps the conduit and decides *whether* to run a command; this store keeps the
//! answer, so a worker that restarts has nothing to remember.
//!
//! # Open defect
//!
//! `attach` is **not verified**. It hangs when driven against PostgreSQL — parked in the tokio reactor
//! with no session established — while every other method on this store completes, including their own
//! `FOR UPDATE` statements, and the sibling suites (`control`, `egress`) pass against the same database.
//! It reproduces with a single test and one thread. The database path is called out here rather than
//! left implicit because a store whose read-mostly methods work and whose one lock-taking decision point
//! does not is exactly the shape a reviewer should not accept on trust.
//!
//! //! Two rules make a reconnect safe:
//!
//! * **`last_command_id` is the record of what ran.** An attach naming that command is a replay and runs
//!   nothing; an attach naming another command is a dispatch that records the new id in the same
//!   transaction that moves the cursor, so there is no instant in which a command has produced output
//!   that the session does not attribute to it.
//! * **the cursor only moves forward.** A client asking for an offset behind the durable one is refused
//!   rather than served, because serving it would re-send bytes the client has already consumed and
//!   leave it to de-duplicate — which is the work this cursor exists to avoid.

use sqlx::{Acquire, PgConnection, Row};

use crate::control::MachineError;

/// How a terminal session is doing (the schema's `terminal_sessions.status` vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Usable.
    Open,
    /// Closed by the caller.
    Closed,
    /// The conduit died; the session is not usable and its command is not re-run.
    Lost,
}

impl SessionStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Lost => "lost",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }

    /// Whether a command may run on a session in this state.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// One terminal session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSession {
    /// `tsn_…` identity.
    pub id: String,
    /// The tenant that owns it.
    pub tenant_id: String,
    /// The execution target it runs on.
    pub target_id: String,
    /// The run it belongs to, when it belongs to one.
    pub run_id: Option<String>,
    /// The conduit's reference.
    pub pty_ref: Option<String>,
    /// The durable byte offset the client has consumed up to.
    pub cursor: u64,
    /// Lifecycle state.
    pub status: SessionStatus,
    /// The last command dispatched on this session.
    pub last_command_id: Option<String>,
}

/// What a request to attach decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// The command already ran; read the stream from `from`.
    Replay {
        /// The command that already ran.
        command_id: String,
        /// The absolute offset to read from.
        from: u64,
    },
    /// The command must run, and the session now records it.
    Dispatch {
        /// The command to run.
        command_id: String,
        /// The absolute offset its output starts at.
        from: u64,
    },
}

impl Attachment {
    /// Whether the attach asked for a run.
    #[must_use]
    pub const fn is_dispatch(&self) -> bool {
        matches!(self, Self::Dispatch { .. })
    }

    /// The offset the attach resumes or starts from.
    #[must_use]
    pub const fn from(&self) -> u64 {
        match self {
            Self::Replay { from, .. } | Self::Dispatch { from, .. } => *from,
        }
    }
}

/// Why a terminal operation failed.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    /// The database refused or was unreachable.
    #[error("terminal store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant has no such session.
    #[error("terminal session {0} does not exist for this tenant")]
    NotFound(String),
    /// The session is not open.
    #[error("terminal session {id} is {status}")]
    NotOpen {
        /// The session.
        id: String,
        /// Its status.
        status: &'static str,
    },
    /// The requested cursor would move backwards.
    #[error("cursor {requested} is behind the durable cursor {durable}")]
    CursorBehind {
        /// What was asked for.
        requested: u64,
        /// What the session holds.
        durable: u64,
    },
    /// The session identity is not a `tsn_` id.
    #[error("{0:?} is not a terminal session id")]
    SessionIdInvalid(String),
    /// The target the session names does not exist for this tenant.
    #[error("execution target {0} does not exist for this tenant")]
    TargetNotFound(String),
    /// A stored row is not one the domain defines.
    #[error("the stored session is not one the domain defines: {detail}")]
    Corrupt {
        /// What is wrong with it.
        detail: String,
    },
}

impl From<TerminalError> for MachineError {
    fn from(error: TerminalError) -> Self {
        MachineError::Terminal(error.to_string())
    }
}

const COLUMNS: &str = "id, tenant_id, target_id, run_id, pty_ref, cursor, status, last_command_id";

/// The terminal session store.
pub struct TerminalStore;

impl TerminalStore {
    /// Open a session on a target.
    ///
    /// # Errors
    /// Returns [`TerminalError::SessionIdInvalid`] when the id is not a `tsn_` id,
    /// [`TerminalError::TargetNotFound`] when the tenant has no such target, and
    /// [`TerminalError::Database`] when the write fails.
    pub async fn open(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        target_id: &str,
        run_id: Option<&str>,
        pty_ref: Option<&str>,
    ) -> Result<TerminalSession, TerminalError> {
        if !session_id.starts_with("tsn_") {
            return Err(TerminalError::SessionIdInvalid(session_id.to_string()));
        }
        let target =
            sqlx::query("SELECT id FROM execution_targets WHERE id = $1 AND tenant_id = $2")
                .bind(target_id)
                .bind(tenant_id)
                .fetch_optional(&mut *conn)
                .await?;
        if target.is_none() {
            return Err(TerminalError::TargetNotFound(target_id.to_string()));
        }
        sqlx::query(
            "INSERT INTO terminal_sessions (id, tenant_id, target_id, run_id, pty_ref, cursor, status) \
             VALUES ($1, $2, $3, $4, $5, '0', 'open')",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(target_id)
        .bind(run_id)
        .bind(pty_ref)
        .execute(&mut *conn)
        .await?;
        Self::load(conn, tenant_id, session_id).await
    }

    /// Load a session.
    ///
    /// # Errors
    /// Returns [`TerminalError::NotFound`] when the tenant has no such session, and
    /// [`TerminalError::Corrupt`] when the stored row is not one the domain defines.
    pub async fn load(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
    ) -> Result<TerminalSession, TerminalError> {
        let sql =
            format!("SELECT {COLUMNS} FROM terminal_sessions WHERE id = $1 AND tenant_id = $2");
        let row = sqlx::query(&sql)
            .bind(session_id)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| TerminalError::NotFound(session_id.to_string()))?;
        row_to_session(&row)
    }

    /// Every session of a target, newest first.
    ///
    /// # Errors
    /// Returns [`TerminalError::Database`] when the read fails.
    pub async fn list_for_target(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
    ) -> Result<Vec<TerminalSession>, TerminalError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM terminal_sessions WHERE tenant_id = $1 AND target_id = $2 \
             ORDER BY created_at DESC, id"
        );
        let rows = sqlx::query(&sql)
            .bind(tenant_id)
            .bind(target_id)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter().map(row_to_session).collect()
    }

    /// Attach to a session, deciding whether the command runs.
    ///
    /// The decision and the record of it are one transaction, and the row is locked while it is taken:
    /// two workers reconnecting with the same command cannot both be told to run it, because the second
    /// reads the `last_command_id` the first wrote.
    ///
    /// # Errors
    /// Returns [`TerminalError::NotOpen`] for a session that is not open,
    /// [`TerminalError::NotFound`] for an unknown session, and [`TerminalError::Database`] when the
    /// transaction fails.
    pub async fn attach(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        command_id: &str,
    ) -> Result<Attachment, TerminalError> {
        let mut tx = conn.begin().await?;
        let sql = format!(
            "SELECT {COLUMNS} FROM terminal_sessions WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
        );
        let row = sqlx::query(&sql)
            .bind(session_id)
            .bind(tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| TerminalError::NotFound(session_id.to_string()))?;
        let session = row_to_session(&row)?;
        if !session.status.is_open() {
            return Err(TerminalError::NotOpen {
                id: session.id,
                status: session.status.as_str(),
            });
        }
        let attachment = if session.last_command_id.as_deref() == Some(command_id) {
            Attachment::Replay {
                command_id: command_id.to_string(),
                from: session.cursor,
            }
        } else {
            sqlx::query(
                "UPDATE terminal_sessions SET last_command_id = $3 WHERE id = $1 AND tenant_id = $2",
            )
            .bind(session_id)
            .bind(tenant_id)
            .bind(command_id)
            .execute(&mut *tx)
            .await?;
            Attachment::Dispatch {
                command_id: command_id.to_string(),
                from: session.cursor,
            }
        };
        tx.commit().await?;
        Ok(attachment)
    }

    /// Advance the durable cursor, refusing a backwards move.
    ///
    /// # Errors
    /// Returns [`TerminalError::CursorBehind`] when the requested offset is behind the durable one, and
    /// [`TerminalError::NotFound`] for an unknown session. A session that is not open still advances:
    /// output produced before it closed is real, and refusing to record it would lose the bytes.
    pub async fn advance(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        to: u64,
    ) -> Result<u64, TerminalError> {
        let mut tx = conn.begin().await?;
        let row = sqlx::query(
            "SELECT cursor FROM terminal_sessions WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(session_id)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| TerminalError::NotFound(session_id.to_string()))?;
        let durable = parse_cursor(row.try_get("cursor")?)?;
        if to < durable {
            return Err(TerminalError::CursorBehind {
                requested: to,
                durable,
            });
        }
        sqlx::query("UPDATE terminal_sessions SET cursor = $3 WHERE id = $1 AND tenant_id = $2")
            .bind(session_id)
            .bind(tenant_id)
            .bind(to.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(to)
    }

    /// Close a session.
    ///
    /// # Errors
    /// Returns [`TerminalError::NotFound`] when the tenant has no such session.
    pub async fn close(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        status: SessionStatus,
    ) -> Result<TerminalSession, TerminalError> {
        let updated = sqlx::query(
            "UPDATE terminal_sessions SET status = $3 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(status.as_str())
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(TerminalError::NotFound(session_id.to_string()));
        }
        Self::load(conn, tenant_id, session_id).await
    }
}

/// The cursor is stored as text (the schema's `cursor TEXT`) and must parse as an absolute offset. A
/// row that does not is a refusal rather than a default, because a cursor nobody can interpret would
/// silently become zero and re-send a whole session.
fn parse_cursor(stored: Option<String>) -> Result<u64, TerminalError> {
    let stored = stored.unwrap_or_else(|| "0".to_string());
    stored.parse::<u64>().map_err(|_| TerminalError::Corrupt {
        detail: format!("cursor {stored:?} is not a byte offset"),
    })
}

fn row_to_session(row: &sqlx::postgres::PgRow) -> Result<TerminalSession, TerminalError> {
    let status: String = row.try_get("status")?;
    Ok(TerminalSession {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        target_id: row.try_get("target_id")?,
        run_id: row.try_get("run_id")?,
        pty_ref: row.try_get("pty_ref")?,
        cursor: parse_cursor(row.try_get("cursor")?)?,
        status: SessionStatus::parse(&status).ok_or_else(|| TerminalError::Corrupt {
            detail: format!("status {status:?} is not one the domain defines"),
        })?,
        last_command_id: row.try_get("last_command_id")?,
    })
}
