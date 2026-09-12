//! Checkpoint metadata and generation ownership (CORE-006, DOMAIN.md §5.8).
//!
//! A checkpoint is a recoverable snapshot reference of workspace files, a browser session
//! or a terminal. This module owns the *metadata* and the ownership rules; the bytes live
//! in object storage and are written by the execution fabric (EXEC-*). Recovery may only
//! restore a checkpoint that belongs to the run's current generation and has not expired,
//! so a stale controller cannot roll a workspace back under a newer one.

use sqlx::postgres::PgConnection;
use sqlx::Row;

use crate::control::schema::{set_tenant_context_conn, SchemaError};

/// What a checkpoint captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointKind {
    /// Workspace files inside the execution target.
    WorkspaceFiles,
    /// Browser session (profile, tabs, cookies) — never the desktop process.
    BrowserSession,
    /// Terminal state and byte cursor.
    Terminal,
    /// Everything the target holds.
    Full,
}

impl CheckpointKind {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceFiles => "workspace_files",
            Self::BrowserSession => "browser_session",
            Self::Terminal => "terminal",
            Self::Full => "full",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "workspace_files" => Some(Self::WorkspaceFiles),
            "browser_session" => Some(Self::BrowserSession),
            "terminal" => Some(Self::Terminal),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// Checkpoint metadata row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// Checkpoint id (`ckp_…`).
    pub id: String,
    /// Execution target the snapshot belongs to.
    pub execution_target_id: String,
    /// Run that created it, when applicable.
    pub run_id: Option<String>,
    /// What was captured.
    pub kind: CheckpointKind,
    /// Object-storage reference (key + digest).
    pub storage_ref: String,
    /// Generation that created it.
    pub generation: i64,
    /// Snapshot size in bytes.
    pub size_bytes: i64,
    /// Restore policy marker (for example `manual_only`, `auto_on_failure`).
    pub restore_policy: Option<String>,
    /// RFC 3339 expiry.
    pub expires_at: Option<String>,
}

/// Request to record a checkpoint.
#[derive(Debug, Clone)]
pub struct NewCheckpoint {
    /// Checkpoint id.
    pub id: String,
    /// Execution target the snapshot belongs to.
    pub execution_target_id: String,
    /// Run that created it.
    pub run_id: Option<String>,
    /// What was captured.
    pub kind: CheckpointKind,
    /// Object-storage reference.
    pub storage_ref: String,
    /// Generation that created it.
    pub generation: i64,
    /// Snapshot size in bytes.
    pub size_bytes: i64,
    /// Restore policy marker.
    pub restore_policy: Option<String>,
    /// RFC 3339 expiry.
    pub expires_at: Option<String>,
}

/// Checkpoint errors.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    /// The database rejected the operation.
    #[error("checkpoint store: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant context could not be established.
    #[error("checkpoint store: {0}")]
    Schema(#[from] SchemaError),
    /// The checkpoint does not exist.
    #[error("checkpoint {0} not found")]
    NotFound(String),
    /// The checkpoint was produced by a superseded generation.
    #[error("checkpoint {id} belongs to generation {checkpoint_generation}, current is {current}")]
    StaleGeneration {
        /// Checkpoint id.
        id: String,
        /// Generation that created the checkpoint.
        checkpoint_generation: i64,
        /// Generation the caller currently holds.
        current: i64,
    },
    /// The checkpoint expired.
    #[error("checkpoint {0} expired")]
    Expired(String),
    /// The stored kind is not a canonical kind.
    #[error("unknown checkpoint kind: {0}")]
    UnknownKind(String),
}

/// Durable checkpoint metadata store.
pub struct CheckpointStore;

impl CheckpointStore {
    /// Record a checkpoint.
    ///
    /// # Errors
    /// Returns an error when the write fails.
    pub async fn create(
        conn: &mut PgConnection,
        tenant_id: &str,
        checkpoint: &NewCheckpoint,
    ) -> Result<(), CheckpointError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        sqlx::query(
            "INSERT INTO checkpoints (id, tenant_id, execution_target_id, run_id, kind, storage_ref, \
             generation, size_bytes, restore_policy, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::timestamptz)",
        )
        .bind(&checkpoint.id)
        .bind(tenant_id)
        .bind(&checkpoint.execution_target_id)
        .bind(&checkpoint.run_id)
        .bind(checkpoint.kind.as_str())
        .bind(&checkpoint.storage_ref)
        .bind(checkpoint.generation)
        .bind(checkpoint.size_bytes)
        .bind(&checkpoint.restore_policy)
        .bind(&checkpoint.expires_at)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// List the checkpoints of a target, newest first.
    ///
    /// # Errors
    /// Returns an error when the query fails or a stored kind is unknown.
    pub async fn list(
        conn: &mut PgConnection,
        tenant_id: &str,
        execution_target_id: &str,
    ) -> Result<Vec<Checkpoint>, CheckpointError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let rows = sqlx::query(
            "SELECT id, execution_target_id, run_id, kind, storage_ref, generation, size_bytes, \
             restore_policy, expires_at::text AS expires_at \
             FROM checkpoints WHERE execution_target_id = $1 ORDER BY created_at DESC",
        )
        .bind(execution_target_id)
        .fetch_all(&mut *conn)
        .await?;
        let mut checkpoints = Vec::with_capacity(rows.len());
        for row in rows {
            let kind: String = row.try_get("kind")?;
            checkpoints.push(Checkpoint {
                id: row.try_get("id")?,
                execution_target_id: row.try_get("execution_target_id")?,
                run_id: row.try_get("run_id")?,
                kind: CheckpointKind::parse(&kind).ok_or(CheckpointError::UnknownKind(kind))?,
                storage_ref: row.try_get("storage_ref")?,
                generation: row.try_get("generation")?,
                size_bytes: row.try_get("size_bytes")?,
                restore_policy: row.try_get("restore_policy")?,
                expires_at: row.try_get("expires_at")?,
            });
        }
        Ok(checkpoints)
    }

    /// Validate that a checkpoint may be restored by a controller at `current_generation`.
    ///
    /// # Errors
    /// Returns [`CheckpointError::StaleGeneration`] when the snapshot was produced by an
    /// older generation, [`CheckpointError::Expired`] when its retention has elapsed and
    /// [`CheckpointError::NotFound`] when it does not exist for this tenant.
    pub async fn validate_restore(
        conn: &mut PgConnection,
        tenant_id: &str,
        checkpoint_id: &str,
        current_generation: i64,
        now: &str,
    ) -> Result<Checkpoint, CheckpointError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let row = sqlx::query(
            "SELECT id, execution_target_id, run_id, kind, storage_ref, generation, size_bytes, \
             restore_policy, expires_at::text AS expires_at \
             FROM checkpoints WHERE id = $1",
        )
        .bind(checkpoint_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| CheckpointError::NotFound(checkpoint_id.to_string()))?;
        let kind: String = row.try_get("kind")?;
        let checkpoint = Checkpoint {
            id: row.try_get("id")?,
            execution_target_id: row.try_get("execution_target_id")?,
            run_id: row.try_get("run_id")?,
            kind: CheckpointKind::parse(&kind).ok_or(CheckpointError::UnknownKind(kind))?,
            storage_ref: row.try_get("storage_ref")?,
            generation: row.try_get("generation")?,
            size_bytes: row.try_get("size_bytes")?,
            restore_policy: row.try_get("restore_policy")?,
            expires_at: row.try_get("expires_at")?,
        };
        if checkpoint.generation > current_generation {
            // A checkpoint from a *newer* generation cannot be restored by an older
            // controller: it would roll the workspace forward under a fenced holder.
            return Err(CheckpointError::StaleGeneration {
                id: checkpoint.id.clone(),
                checkpoint_generation: checkpoint.generation,
                current: current_generation,
            });
        }
        if let Some(expires_at) = &checkpoint.expires_at {
            if expires_at.as_str() < now {
                return Err(CheckpointError::Expired(checkpoint.id.clone()));
            }
        }
        Ok(checkpoint)
    }
}
