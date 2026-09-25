//! Role-based authorization at the API boundary (APP-012, DOMAIN.md §2).
//!
//! The server decides once, for every client — desktop, web, CLI — what a session user
//! may do: the user's tenant role and workspace role are read from the membership rows
//! (APP-002's identity owner) and RUN-006's [`rbac::check`] is applied to the action
//! family a handler names. A refusal is `SCOPE_FORBIDDEN` with the typed reason; a user
//! with no membership at the scope holds no role, and no role is a denial. A page may
//! mirror these rules to decide what to *offer*; it cannot widen them. A service principal is
//! decided by its explicit command-family scopes before its command reaches a handler.

use serde_json::json;
use sqlx::PgPool;

use super::{ApiError, ApiErrorCode, TenantScope};
use crate::control::schema;
use crate::policy::rbac::{self, ActionFamily, ActorRoles, TenantRole, WorkspaceRole};

/// The roles a session user holds at a scope, as the membership rows say them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authorization {
    /// Resolved roles (both may be absent).
    pub roles: ActorRoles,
}

impl Authorization {
    /// Resolve the acting user's roles: the tenant role from `tenant_memberships` and, when
    /// a workspace is named, the workspace role from `workspace_memberships`. Only `active`
    /// memberships count; a call without an acting user holds no role.
    ///
    /// # Errors
    /// Returns [`ApiErrorCode::Internal`] when the membership rows cannot be read.
    pub async fn resolve(
        pool: &PgPool,
        scope: &TenantScope,
        workspace_id: Option<&str>,
    ) -> Result<Self, ApiError> {
        if scope.principal.is_some() {
            // A service principal reaches a handler only through the command surface, after its
            // explicit scopes admitted the command's family (OPS-001): the family is the grant, so
            // inside it the principal holds the administrator's roles. `Trust` (approvals) is
            // never grantable, so no principal decides an approval.
            return Ok(Self {
                roles: ActorRoles::new(Some(TenantRole::Admin), Some(WorkspaceRole::Admin)),
            });
        }
        let Some(user_id) = scope.user_id.as_deref() else {
            return Ok(Self {
                roles: ActorRoles::default(),
            });
        };
        let mut tx = pool.begin().await.map_err(internal)?;
        schema::set_tenant_context(&mut tx, &scope.tenant_id)
            .await
            .map_err(internal)?;
        let tenant: Option<String> = sqlx::query_scalar(
            "SELECT role FROM tenant_memberships \
             WHERE tenant_id = $1 AND user_id = $2 AND status = 'active'",
        )
        .bind(&scope.tenant_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(internal)?;
        let workspace: Option<String> = match workspace_id {
            Some(workspace_id) => sqlx::query_scalar(
                "SELECT role FROM workspace_memberships \
                 WHERE tenant_id = $1 AND workspace_id = $2 AND user_id = $3 AND status = 'active'",
            )
            .bind(&scope.tenant_id)
            .bind(workspace_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?,
            None => None,
        };
        tx.commit().await.map_err(internal)?;
        Ok(Self {
            roles: ActorRoles::new(
                tenant.and_then(|role| TenantRole::parse(&role).ok()),
                workspace.and_then(|role| WorkspaceRole::parse(&role).ok()),
            ),
        })
    }

    /// Refuse unless the roles allow `action`.
    ///
    /// # Errors
    /// Returns [`ApiErrorCode::ScopeForbidden`] with the typed RBAC reason in `details`.
    pub fn require(self, action: ActionFamily, correlation: &str) -> Result<Self, ApiError> {
        match rbac::check(self.roles, action) {
            Ok(()) => Ok(self),
            Err(failure) => Err(ApiError::new(
                ApiErrorCode::ScopeForbidden,
                format!(
                    "{} is not allowed for this user's role ({})",
                    describe(action),
                    failure.as_str()
                ),
                correlation,
            )
            .with_details(json!({
                "kind": "rbac",
                "action": describe(action),
                "reason": failure.as_str(),
                "tenant_role": self.roles.tenant.map(TenantRole::as_str),
                "workspace_role": self.roles.workspace.map(WorkspaceRole::as_str),
            }))),
        }
    }

    /// Resolve and require in one step.
    ///
    /// # Errors
    /// As [`Self::resolve`] and [`Self::require`].
    pub async fn check(
        pool: &PgPool,
        scope: &TenantScope,
        workspace_id: Option<&str>,
        action: ActionFamily,
        correlation: &str,
    ) -> Result<Self, ApiError> {
        Self::resolve(pool, scope, workspace_id)
            .await?
            .require(action, correlation)
    }
}

/// The action family's wire name (what a refusal names).
#[must_use]
pub fn describe(action: ActionFamily) -> String {
    match action {
        ActionFamily::Read => "read".to_string(),
        ActionFamily::AuthorWork => "author_work".to_string(),
        ActionFamily::ApproveEffect => "approve_effect".to_string(),
        ActionFamily::ManageWorkspace => "manage_workspace".to_string(),
        ActionFamily::ExecuteEffect { tier } => format!("execute_effect(tier {tier})"),
        ActionFamily::AdminOps => "admin_ops".to_string(),
        ActionFamily::ManageTenant => "manage_tenant".to_string(),
        ActionFamily::ViewUsage => "view_usage".to_string(),
    }
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "rbac")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_role_is_a_denial_and_the_refusal_is_typed() {
        let none = Authorization {
            roles: ActorRoles::default(),
        };
        let refused = none
            .require(ActionFamily::ManageTenant, "c")
            .expect_err("no role");
        assert_eq!(refused.code(), Some(ApiErrorCode::ScopeForbidden));
        assert_eq!(refused.status().as_u16(), 403);
        let viewer = Authorization {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Viewer)),
        };
        assert!(viewer.require(ActionFamily::ApproveEffect, "c").is_err());
        assert!(viewer.require(ActionFamily::Read, "c").is_ok());
        let owner = Authorization {
            roles: ActorRoles::new(Some(TenantRole::Owner), None),
        };
        // Tenant owner/admin inherit workspace admin everywhere (DOMAIN.md §2).
        assert!(owner.require(ActionFamily::ManageWorkspace, "c").is_ok());
        assert!(owner.require(ActionFamily::ApproveEffect, "c").is_ok());
        assert!(owner.require(ActionFamily::ViewUsage, "c").is_ok());
        let billing = Authorization {
            roles: ActorRoles::new(Some(TenantRole::Billing), None),
        };
        assert!(billing.require(ActionFamily::ViewUsage, "c").is_ok());
        assert!(billing.require(ActionFamily::ManageTenant, "c").is_err());
    }
}
