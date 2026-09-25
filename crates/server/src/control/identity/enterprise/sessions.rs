//! Session revocation and a user's own sessions (OPS-001, DOMAIN.md §2).
//!
//! Every deprovisioning path revokes through [`revoke_in`], inside the transaction that changes
//! the membership, so the membership change and the revocation commit together. An access token
//! is checked against its session row on every request (`AuthStore::authenticate`), so the next
//! request after the commit is refused. `RevokeUserSessions` does the same for a tenant
//! administrator. A user can list and end their own sessions, one row per client (desktop, web,
//! cli) and rotation family.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};

use super::{audit_in, stage_member_in, Acting, EnterpriseError, EnterpriseIdentity};
use crate::control::identity::auth::SessionScope;

/// Revoke every live session `user_id` holds in `tenant_id` inside `tx`; returns how many.
///
/// # Errors
/// Returns the database error.
pub async fn revoke_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let revoked = sqlx::query(
        "UPDATE sessions SET revoked_at = now(), revoked_reason = $3 \
         WHERE tenant_id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(revoked.rows_affected())
}

/// A session row as its owner sees it.
type OwnSessionRow = (
    String,
    String,
    String,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

impl EnterpriseIdentity {
    /// `RevokeUserSessions {user_id}`: end every session the user holds in the tenant.
    ///
    /// # Errors
    /// [`EnterpriseError::NotFound`] when the user has no membership in the tenant.
    pub async fn revoke_user_sessions(
        &self,
        tenant_id: &str,
        acting: &Acting,
        user_id: &str,
    ) -> Result<Value, EnterpriseError> {
        let tenant = tenant_id.to_string();
        let user = user_id.to_string();
        let acting = acting.clone();
        let revoked = self
            .events()
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let membership: Option<String> = sqlx::query_scalar(
                        "SELECT id FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2",
                    )
                    .bind(&tenant)
                    .bind(&user)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some(membership) = membership else {
                        return Err(super::rejected(EnterpriseError::NotFound(format!(
                            "user {user} has no membership in this tenant"
                        ))));
                    };
                    let revoked =
                        revoke_in(tx, &tenant, &user, "revoked by an administrator").await?;
                    stage_member_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        &membership,
                        "member.sessions_revoked",
                        json!({ "membership_id": membership, "user_id": user, "revoked": revoked }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        "RevokeUserSessions",
                        &format!("user:{user}"),
                        "applied",
                        &format!("{revoked} session(s) revoked"),
                    )
                    .await?;
                    Ok(revoked)
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        Ok(json!({ "user_id": user_id, "revoked_sessions": revoked }))
    }

    /// `GET /v1/auth/sessions`: the caller's live sessions in the tenant, newest first.
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn own_sessions(&self, scope: &SessionScope) -> Result<Vec<Value>, EnterpriseError> {
        let mut tx = self.auth_transaction(Some(&scope.tenant_id)).await?;
        let current_family: Option<String> =
            sqlx::query_scalar("SELECT family_id FROM sessions WHERE id = $1")
                .bind(&scope.session_id)
                .fetch_optional(&mut *tx)
                .await?;
        let rows: Vec<OwnSessionRow> = sqlx::query_as(
            "SELECT id, family_id, client, auth_method, created_at, last_used_at FROM sessions \
             WHERE tenant_id = $1 AND user_id = $2 AND revoked_at IS NULL AND rotated_at IS NULL \
             AND refresh_expires_at > now() ORDER BY created_at DESC",
        )
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|(id, family, client, method, created_at, last_used_at)| {
                json!({
                    "session_id": id,
                    "client": client,
                    "auth_method": method,
                    "created_at": created_at,
                    "last_used_at": last_used_at,
                    "current": current_family.as_deref() == Some(family.as_str()),
                })
            })
            .collect())
    }

    /// `POST /v1/auth/sessions/{session_id}/revoke`: end one of the caller's own sessions (its
    /// whole rotation family).
    ///
    /// # Errors
    /// [`EnterpriseError::NotFound`] when the session is not the caller's in this tenant.
    pub async fn revoke_own_session(
        &self,
        scope: &SessionScope,
        session_id: &str,
    ) -> Result<Value, EnterpriseError> {
        let mut tx = self.auth_transaction(Some(&scope.tenant_id)).await?;
        let family: Option<String> = sqlx::query_scalar(
            "SELECT family_id FROM sessions WHERE id = $1 AND tenant_id = $2 AND user_id = $3",
        )
        .bind(session_id)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(family) = family else {
            return Err(EnterpriseError::NotFound(format!(
                "session {session_id} is not one of yours"
            )));
        };
        let revoked = sqlx::query(
            "UPDATE sessions SET revoked_at = now(), revoked_reason = 'revoked by its user' \
             WHERE family_id = $1 AND revoked_at IS NULL",
        )
        .bind(&family)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(json!({ "session_id": session_id, "revoked": revoked.rows_affected() > 0 }))
    }
}
