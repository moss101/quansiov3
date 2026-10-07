//! Who may be served what, asked again on every request (DOMAIN.md §10.4 F).
//!
//! A preview scope is a capability URL, and a capability URL outlives the moment it was issued. So
//! nothing about the scope is trusted after it is issued: every request re-reads the run, the acting
//! member, the workspace and the execution target's root through [`PreviewAuthority`], and a scope
//! whose answer changed is closed at that request.

use std::path::PathBuf;

use async_trait::async_trait;
use sqlx::PgPool;

use crate::control::schema;

/// Why a scope no longer serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lapse {
    /// Its time ran out.
    Expired,
    /// It was closed.
    Closed,
    /// The run it was opened in ended.
    SessionEnded,
    /// The member it acts for is no longer an active member.
    MemberRemoved,
    /// The workspace or the execution target is gone or is another one.
    WorkspaceChanged,
    /// The workspace root's path or generation changed.
    RootsChanged,
    /// Nothing more is known.
    Unknown,
}

impl Lapse {
    /// The reason as DOMAIN.md spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Closed => "closed",
            Self::SessionEnded => "session_ended",
            Self::MemberRemoved => "member_removed",
            Self::WorkspaceChanged => "workspace_changed",
            Self::RootsChanged => "roots_changed",
            Self::Unknown => "unknown",
        }
    }
}

/// What a scope is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The tenant.
    pub tenant_id: String,
    /// The workspace.
    pub workspace_id: String,
    /// The run (the session) the scope was opened in.
    pub run_id: String,
    /// The member the run acts for, when there is one.
    pub acting_user_id: Option<String>,
}

/// What the authority says now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The workspace root on this machine, canonical.
    pub workspace_root: PathBuf,
    /// Names the root and the target generation: a scope is only valid while it is unchanged.
    pub generation: String,
}

/// Why the authority did not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    /// The binding no longer holds.
    Lapsed(Lapse),
    /// There is nothing to serve (no root is reported), whatever the binding.
    Refused(String),
    /// The authority could not be asked; nothing is decided.
    Unavailable(String),
}

/// Re-resolves a scope's authority.
#[async_trait]
pub trait PreviewAuthority: Send + Sync {
    /// The workspace root a scope bound like this may serve from, now.
    ///
    /// # Errors
    /// [`AuthorityError`].
    async fn resolve(&self, binding: &Binding) -> Result<Resolved, AuthorityError>;
}

/// The authority backed by the database: the run, the membership, the workspace and the execution
/// target the run is on. It only reads.
#[derive(Clone)]
pub struct DbPreviewAuthority {
    pool: PgPool,
}

impl std::fmt::Debug for DbPreviewAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbPreviewAuthority").finish_non_exhaustive()
    }
}

impl DbPreviewAuthority {
    /// An authority over the database.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn unavailable(error: &impl std::fmt::Display) -> AuthorityError {
    AuthorityError::Unavailable(error.to_string())
}

const TERMINAL_RUN: &[&str] = &["SUCCEEDED", "FAILED", "CANCELLED", "BLOCKED_UNRECOVERABLE"];
const GONE_TARGET: &[&str] = &["DESTROYED", "FAILED"];

#[async_trait]
impl PreviewAuthority for DbPreviewAuthority {
    async fn resolve(&self, binding: &Binding) -> Result<Resolved, AuthorityError> {
        // The tenant context is transaction-local: every statement runs inside this transaction.
        let mut tx = self.pool.begin().await.map_err(|e| unavailable(&e))?;
        schema::set_tenant_context(&mut tx, &binding.tenant_id)
            .await
            .map_err(|e| unavailable(&e))?;
        let run: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT status, workspace_id, execution_target_id FROM runs WHERE id = $1 AND tenant_id = $2",
        )
        .bind(&binding.run_id)
        .bind(&binding.tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| unavailable(&e))?;
        let Some((status, run_workspace, target_id)) = run else {
            return Err(AuthorityError::Lapsed(Lapse::SessionEnded));
        };
        if TERMINAL_RUN.contains(&status.as_str()) {
            return Err(AuthorityError::Lapsed(Lapse::SessionEnded));
        }
        let workspace: Option<String> =
            sqlx::query_scalar("SELECT id FROM workspaces WHERE id = $1 AND tenant_id = $2")
                .bind(&binding.workspace_id)
                .bind(&binding.tenant_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| unavailable(&e))?;
        if workspace.is_none() || run_workspace != binding.workspace_id {
            return Err(AuthorityError::Lapsed(Lapse::WorkspaceChanged));
        }
        if let Some(user) = &binding.acting_user_id {
            let member: Option<String> = sqlx::query_scalar(
                "SELECT status FROM workspace_memberships \
                 WHERE tenant_id = $1 AND workspace_id = $2 AND user_id = $3",
            )
            .bind(&binding.tenant_id)
            .bind(&binding.workspace_id)
            .bind(user)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| unavailable(&e))?;
            if member.as_deref() != Some("active") {
                return Err(AuthorityError::Lapsed(Lapse::MemberRemoved));
            }
        }
        let Some(target_id) = target_id else {
            return Err(AuthorityError::Refused(
                "this run has no execution target, so there is no workspace root to serve from"
                    .to_string(),
            ));
        };
        let target: Option<(String, i64, String, Option<String>)> = sqlx::query_as(
            "SELECT status, generation, workspace_id, resources ->> 'workspace_root' \
             FROM execution_targets WHERE id = $1 AND tenant_id = $2",
        )
        .bind(&target_id)
        .bind(&binding.tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| unavailable(&e))?;
        tx.commit().await.map_err(|e| unavailable(&e))?;
        let Some((target_status, generation, target_workspace, root)) = target else {
            return Err(AuthorityError::Lapsed(Lapse::WorkspaceChanged));
        };
        if GONE_TARGET.contains(&target_status.as_str()) || target_workspace != binding.workspace_id
        {
            return Err(AuthorityError::Lapsed(Lapse::WorkspaceChanged));
        }
        let Some(root) = root.filter(|root| !root.trim().is_empty()) else {
            return Err(AuthorityError::Refused(
                "the execution target reports no workspace root".to_string(),
            ));
        };
        let canonical = tokio::fs::canonicalize(&root)
            .await
            .map_err(|_| AuthorityError::Lapsed(Lapse::WorkspaceChanged))?;
        Ok(Resolved {
            generation: format!("{target_id}#{generation}#{}", canonical.display()),
            workspace_root: canonical,
        })
    }
}
