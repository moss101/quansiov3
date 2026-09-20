//! Tenant and workspace memberships (APP-002's identity owner; DOMAIN.md §2, §14
//! `InviteMember`, `UpdateMembership`, `RemoveMember`, `GET /v1/members`).
//!
//! A membership is the only thing that gives a user a role, and roles are what RBAC
//! decides on, so every change here commits with its `member.*` RuntimeEvent and its
//! AuditEntry (DOMAIN.md §16) in one transaction. An invitation names an existing user
//! by email or provisions a new one the way onboarding does (`invited` until they sign
//! in); the last active owner of a tenant cannot be demoted, suspended or removed.

use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventError, EventType};
use serde_json::{json, Value};
use sqlx::{Postgres, Row, Transaction};

use super::auth::normalize_email;
use super::{IdentityError, IdentityStore, IDENTITY_OWNER};
use crate::audit::{append_in, AuditDraft};
use crate::control::schema;
use crate::policy::rbac::{TenantRole, WorkspaceRole};

/// Who is changing memberships.
#[derive(Debug, Clone)]
pub struct MemberAdmin {
    /// The acting user.
    pub user_id: String,
    /// Event/audit actor.
    pub actor: Actor,
    /// Request correlation.
    pub correlation_id: CorrelationId,
}

/// One user's standing in the tenant and its workspaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberView {
    /// `usr_` id.
    pub user_id: String,
    /// Sign-in address.
    pub email: String,
    /// Display name.
    pub display_name: String,
    /// `tm_` id.
    pub membership_id: String,
    /// Tenant role.
    pub role: String,
    /// `invited | active | suspended | removed`.
    pub status: String,
    /// Workspace memberships in this tenant.
    pub workspaces: Vec<WorkspaceMembershipView>,
}

/// One workspace membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMembershipView {
    /// `wm_` id.
    pub membership_id: String,
    /// `ws_` id.
    pub workspace_id: String,
    /// Workspace role.
    pub role: String,
    /// `invited | active | suspended | removed`.
    pub status: String,
}

impl MemberView {
    /// The wire shape.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "user": { "id": self.user_id, "primary_email": self.email, "display_name": self.display_name },
            "membership_id": self.membership_id,
            "role": self.role,
            "status": self.status,
            "workspaces": self.workspaces.iter().map(|membership| json!({
                "membership_id": membership.membership_id,
                "workspace_id": membership.workspace_id,
                "role": membership.role,
                "status": membership.status,
            })).collect::<Vec<_>>(),
        })
    }
}

/// An invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// Sign-in address of the invitee.
    pub email: String,
    /// Tenant role to hold.
    pub role: TenantRole,
    /// Optionally a workspace to join, with the role there.
    pub workspace: Option<(String, WorkspaceRole)>,
}

const STATUSES: [&str; 3] = ["invited", "active", "suspended"];

fn rejected(error: IdentityError) -> EventError {
    EventError::MutationRejected {
        owner: IDENTITY_OWNER,
        message: error.to_string(),
    }
}

impl IdentityStore {
    /// Every member of the tenant with their workspace memberships, or only the members of
    /// one workspace. Removed memberships are listed too: they are history, not absence.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn list_members(
        &self,
        tenant_id: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<MemberView>, IdentityError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let members = sqlx::query(
            "SELECT m.id AS membership_id, m.user_id, m.role, m.status, u.primary_email, u.display_name \
             FROM tenant_memberships m JOIN users u ON u.id = m.user_id \
             WHERE m.tenant_id = $1 \
               AND ($2::text IS NULL OR EXISTS (SELECT 1 FROM workspace_memberships wm \
                    WHERE wm.tenant_id = m.tenant_id AND wm.user_id = m.user_id AND wm.workspace_id = $2)) \
             ORDER BY m.created_at, m.id",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let workspaces = sqlx::query(
            "SELECT id, workspace_id, user_id, role, status FROM workspace_memberships \
             WHERE tenant_id = $1 ORDER BY created_at, id",
        )
        .bind(tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(members
            .iter()
            .map(|row| {
                let user_id: String = row.get("user_id");
                MemberView {
                    membership_id: row.get("membership_id"),
                    role: row.get("role"),
                    status: row.get("status"),
                    email: row.get("primary_email"),
                    display_name: row.get("display_name"),
                    workspaces: workspaces
                        .iter()
                        .filter(|membership| membership.get::<String, _>("user_id") == user_id)
                        .map(|membership| WorkspaceMembershipView {
                            membership_id: membership.get("id"),
                            workspace_id: membership.get("workspace_id"),
                            role: membership.get("role"),
                            status: membership.get("status"),
                        })
                        .collect(),
                    user_id,
                }
            })
            .collect())
    }

    /// Invite a user into the tenant (and optionally a workspace).
    ///
    /// # Errors
    /// [`IdentityError::Conflict`] when the user is already an active member,
    /// [`IdentityError::Schema`] for a malformed address or unknown workspace.
    pub async fn invite_member(
        &self,
        tenant_id: &str,
        by: &MemberAdmin,
        invitation: Invitation,
    ) -> Result<MemberView, IdentityError> {
        let email = normalize_email(&invitation.email)
            .map_err(|error| IdentityError::Schema(error.to_string()))?;
        let tenant = tenant_id.to_string();
        let by = by.clone();
        let outcome = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    schema::set_user_context(tx, &by.user_id)
                        .await
                        .map_err(|error| rejected(IdentityError::Schema(error.to_string())))?;
                    let existing: Option<(String, String)> = sqlx::query_as(
                        "SELECT id, display_name FROM users WHERE primary_email = $1",
                    )
                    .bind(&email)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let (user_id, display_name, provisioned) = match existing {
                        Some((id, name)) => (id, name, false),
                        None => {
                            let id = CanonicalId::generate(Prefix::User, &mut UlidGenerator::new())
                                .to_string();
                            let name = email.split('@').next().unwrap_or("member").to_string();
                            // The users policy admits an insert only for the acting user:
                            // the invitee is the actor of their own provisioning.
                            schema::set_user_context(tx, &id).await.map_err(|error| {
                                rejected(IdentityError::Schema(error.to_string()))
                            })?;
                            sqlx::query(
                                "INSERT INTO users (id, primary_email, display_name, auth_methods, settings) \
                                 VALUES ($1, $2, $3, $4, '{}'::jsonb)",
                            )
                            .bind(&id)
                            .bind(&email)
                            .bind(&name)
                            .bind(json!([
                                {"method": "email_magic_link"},
                                {"method": "oauth_google"},
                                {"method": "oauth_microsoft"},
                                {"method": "oauth_github"}
                            ]))
                            .execute(&mut **tx)
                            .await?;
                            schema::set_user_context(tx, &by.user_id).await.map_err(|error| {
                                rejected(IdentityError::Schema(error.to_string()))
                            })?;
                            (id, name, true)
                        }
                    };
                    let current: Option<(String, String)> = sqlx::query_as(
                        "SELECT id, status FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let status = if provisioned { "invited" } else { "active" };
                    let membership_id = match current {
                        Some((_, existing_status)) if existing_status != "removed" => {
                            return Err(rejected(IdentityError::Conflict(format!(
                                "{email} is already a member ({existing_status}); use UpdateMembership"
                            ))));
                        }
                        Some((id, _)) => {
                            sqlx::query(
                                "UPDATE tenant_memberships SET role = $3, status = $4, invited_by = $5, \
                                 updated_at = now() WHERE tenant_id = $1 AND id = $2",
                            )
                            .bind(&tenant)
                            .bind(&id)
                            .bind(invitation.role.as_str())
                            .bind(status)
                            .bind(&by.user_id)
                            .execute(&mut **tx)
                            .await?;
                            id
                        }
                        None => {
                            let id = format!("tm_{}", &user_id[4..]);
                            sqlx::query(
                                "INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status, invited_by) \
                                 VALUES ($1, $2, $3, $4, $5, $6)",
                            )
                            .bind(&id)
                            .bind(&tenant)
                            .bind(&user_id)
                            .bind(invitation.role.as_str())
                            .bind(status)
                            .bind(&by.user_id)
                            .execute(&mut **tx)
                            .await?;
                            id
                        }
                    };
                    let mut workspace_json = Value::Null;
                    if let Some((workspace_id, role)) = &invitation.workspace {
                        let known: Option<String> = sqlx::query_scalar(
                            "SELECT id FROM workspaces WHERE tenant_id = $1 AND id = $2",
                        )
                        .bind(&tenant)
                        .bind(workspace_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                        if known.is_none() {
                            return Err(rejected(IdentityError::Schema(format!(
                                "workspace {workspace_id} is not in this tenant"
                            ))));
                        }
                        let wm_id = format!("wm_{}", CanonicalId::generate(Prefix::Workspace, &mut UlidGenerator::new()).to_string().trim_start_matches("ws_"));
                        sqlx::query(
                            "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
                             VALUES ($1, $2, $3, $4, $5, $6) \
                             ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role, \
                             status = EXCLUDED.status, updated_at = now()",
                        )
                        .bind(&wm_id)
                        .bind(&tenant)
                        .bind(workspace_id)
                        .bind(&user_id)
                        .bind(role.as_str())
                        .bind(status)
                        .execute(&mut **tx)
                        .await?;
                        workspace_json = json!({ "workspace_id": workspace_id, "role": role.as_str() });
                    }
                    stage(tx, batch, &tenant, &by, &membership_id, "member.invited", json!({
                        "membership_id": membership_id,
                        "user_id": user_id,
                        "email": email,
                        "role": invitation.role.as_str(),
                        "status": status,
                        "provisioned": provisioned,
                        "workspace": workspace_json,
                    }))
                    .await?;
                    append_in(tx, &tenant, AuditDraft {
                        workspace_id: invitation.workspace.as_ref().map(|(id, _)| id.clone()),
                        actor: actor_json(&by.actor),
                        action: "InviteMember".to_string(),
                        target_ref: Some(format!("membership:{membership_id}")),
                        decision: Some("applied".to_string()),
                        reason: Some(format!("{email} as {}", invitation.role.as_str())),
                        correlation_id: Some(by.correlation_id.to_string()),
                    })
                    .await?;
                    Ok((user_id, display_name, membership_id))
                })
            })
            .await
            .map_err(unwrap_rejection)?;
        self.member(tenant_id, &outcome.2).await
    }

    /// Change a membership's role or status. `membership_id` is a `tm_` (tenant) or `wm_`
    /// (workspace) id; the role vocabulary is the scope's; status is `active | suspended`.
    ///
    /// # Errors
    /// [`IdentityError::NotFound`] for an unknown membership, [`IdentityError::Schema`] for
    /// a role or status outside the vocabulary, [`IdentityError::Conflict`] when the change
    /// would leave the tenant without an active owner.
    pub async fn update_membership(
        &self,
        tenant_id: &str,
        by: &MemberAdmin,
        membership_id: &str,
        role: Option<&str>,
        status: Option<&str>,
    ) -> Result<MemberView, IdentityError> {
        if let Some(status) = status {
            if !STATUSES.contains(&status) {
                return Err(IdentityError::Schema(format!(
                    "status must be one of {STATUSES:?}, not {status:?}"
                )));
            }
        }
        let tenant_scope = membership_id.starts_with("tm_");
        if !tenant_scope && !membership_id.starts_with("wm_") {
            return Err(IdentityError::Schema(
                "membership_id must be a tm_ or wm_ id".to_string(),
            ));
        }
        if let Some(role) = role {
            let known = if tenant_scope {
                TenantRole::parse(role).is_ok()
            } else {
                WorkspaceRole::parse(role).is_ok()
            };
            if !known {
                return Err(IdentityError::Schema(format!(
                    "{role:?} is not a {} role",
                    if tenant_scope { "tenant" } else { "workspace" }
                )));
            }
        }
        let tenant = tenant_id.to_string();
        let by = by.clone();
        let membership = membership_id.to_string();
        let role = role.map(str::to_string);
        let status = status.map(str::to_string);
        let user_id = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let table = if tenant_scope { "tenant_memberships" } else { "workspace_memberships" };
                    let current: Option<(String, String, String)> = sqlx::query_as(&format!(
                        "SELECT user_id, role, status FROM {table} WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
                    ))
                    .bind(&tenant)
                    .bind(&membership)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some((user_id, from_role, from_status)) = current else {
                        return Err(rejected(IdentityError::NotFound(format!(
                            "membership {membership} not found"
                        ))));
                    };
                    if from_status == "removed" {
                        return Err(rejected(IdentityError::Conflict(format!(
                            "membership {membership} was removed; invite again"
                        ))));
                    }
                    let next_role = role.clone().unwrap_or_else(|| from_role.clone());
                    let next_status = status.clone().unwrap_or_else(|| from_status.clone());
                    if tenant_scope {
                        guard_last_owner(tx, &tenant, &user_id, &next_role, &next_status).await?;
                    }
                    sqlx::query(&format!(
                        "UPDATE {table} SET role = $3, status = $4, updated_at = now() \
                         WHERE tenant_id = $1 AND id = $2"
                    ))
                    .bind(&tenant)
                    .bind(&membership)
                    .bind(&next_role)
                    .bind(&next_status)
                    .execute(&mut **tx)
                    .await?;
                    stage(tx, batch, &tenant, &by, &membership, "member.updated", json!({
                        "membership_id": membership,
                        "user_id": user_id,
                        "scope": if tenant_scope { "tenant" } else { "workspace" },
                        "from": { "role": from_role, "status": from_status },
                        "to": { "role": next_role, "status": next_status },
                    }))
                    .await?;
                    append_in(tx, &tenant, AuditDraft {
                        workspace_id: None,
                        actor: actor_json(&by.actor),
                        action: "UpdateMembership".to_string(),
                        target_ref: Some(format!("membership:{membership}")),
                        decision: Some("applied".to_string()),
                        reason: Some(format!("{from_role}/{from_status} -> {next_role}/{next_status}")),
                        correlation_id: Some(by.correlation_id.to_string()),
                    })
                    .await?;
                    Ok(user_id)
                })
            })
            .await
            .map_err(unwrap_rejection)?;
        self.member_of_user(tenant_id, &user_id).await
    }

    /// Remove a membership. A `tm_` removal also removes the user's workspace memberships
    /// in the tenant.
    ///
    /// # Errors
    /// [`IdentityError::NotFound`] for an unknown membership, [`IdentityError::Conflict`]
    /// for the last active owner.
    pub async fn remove_member(
        &self,
        tenant_id: &str,
        by: &MemberAdmin,
        membership_id: &str,
    ) -> Result<MemberView, IdentityError> {
        let tenant_scope = membership_id.starts_with("tm_");
        if !tenant_scope && !membership_id.starts_with("wm_") {
            return Err(IdentityError::Schema(
                "membership_id must be a tm_ or wm_ id".to_string(),
            ));
        }
        let tenant = tenant_id.to_string();
        let by = by.clone();
        let membership = membership_id.to_string();
        let user_id = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let table = if tenant_scope { "tenant_memberships" } else { "workspace_memberships" };
                    let current: Option<(String, String, String)> = sqlx::query_as(&format!(
                        "SELECT user_id, role, status FROM {table} WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
                    ))
                    .bind(&tenant)
                    .bind(&membership)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some((user_id, from_role, from_status)) = current else {
                        return Err(rejected(IdentityError::NotFound(format!(
                            "membership {membership} not found"
                        ))));
                    };
                    if from_status == "removed" {
                        return Err(rejected(IdentityError::Conflict(format!(
                            "membership {membership} is already removed"
                        ))));
                    }
                    if tenant_scope {
                        guard_last_owner(tx, &tenant, &user_id, &from_role, "removed").await?;
                        sqlx::query(
                            "UPDATE workspace_memberships SET status = 'removed', updated_at = now() \
                             WHERE tenant_id = $1 AND user_id = $2 AND status <> 'removed'",
                        )
                        .bind(&tenant)
                        .bind(&user_id)
                        .execute(&mut **tx)
                        .await?;
                    }
                    sqlx::query(&format!(
                        "UPDATE {table} SET status = 'removed', updated_at = now() WHERE tenant_id = $1 AND id = $2"
                    ))
                    .bind(&tenant)
                    .bind(&membership)
                    .execute(&mut **tx)
                    .await?;
                    stage(tx, batch, &tenant, &by, &membership, "member.removed", json!({
                        "membership_id": membership,
                        "user_id": user_id,
                        "scope": if tenant_scope { "tenant" } else { "workspace" },
                        "from": { "role": from_role, "status": from_status },
                    }))
                    .await?;
                    append_in(tx, &tenant, AuditDraft {
                        workspace_id: None,
                        actor: actor_json(&by.actor),
                        action: "RemoveMember".to_string(),
                        target_ref: Some(format!("membership:{membership}")),
                        decision: Some("applied".to_string()),
                        reason: Some(format!("was {from_role}/{from_status}")),
                        correlation_id: Some(by.correlation_id.to_string()),
                    })
                    .await?;
                    Ok(user_id)
                })
            })
            .await
            .map_err(unwrap_rejection)?;
        self.member_of_user(tenant_id, &user_id).await
    }

    async fn member(
        &self,
        tenant_id: &str,
        membership_id: &str,
    ) -> Result<MemberView, IdentityError> {
        self.list_members(tenant_id, None)
            .await?
            .into_iter()
            .find(|member| member.membership_id == membership_id)
            .ok_or_else(|| IdentityError::NotFound(format!("membership {membership_id} not found")))
    }

    async fn member_of_user(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<MemberView, IdentityError> {
        self.list_members(tenant_id, None)
            .await?
            .into_iter()
            .find(|member| member.user_id == user_id)
            .ok_or_else(|| IdentityError::NotFound(format!("user {user_id} has no membership")))
    }
}

/// Refuse a change that would leave the tenant without an active owner.
async fn guard_last_owner(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
    next_role: &str,
    next_status: &str,
) -> Result<(), EventError> {
    if next_role == "owner" && next_status == "active" {
        return Ok(());
    }
    let other_owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tenant_memberships WHERE tenant_id = $1 AND user_id <> $2 \
         AND role = 'owner' AND status = 'active'",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    let is_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2 \
         AND role = 'owner' AND status = 'active')",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    if is_owner && other_owners == 0 {
        return Err(rejected(IdentityError::Conflict(
            "the last active owner cannot be demoted, suspended or removed".to_string(),
        )));
    }
    Ok(())
}

fn actor_json(actor: &Actor) -> Value {
    serde_json::to_value(actor).unwrap_or_else(|_| json!({ "kind": "system", "id": "identity" }))
}

async fn stage(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut quansio_events::EventBatch,
    tenant_id: &str,
    by: &MemberAdmin,
    membership_id: &str,
    event_type: &str,
    payload: Value,
) -> Result<(), EventError> {
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 \
         AND aggregate_type = 'member' AND aggregate_id = $2",
    )
    .bind(tenant_id)
    .bind(membership_id)
    .fetch_one(&mut **tx)
    .await?;
    batch.emit(
        EventDraft::new(
            "member",
            membership_id,
            u64::try_from(existing).unwrap_or(0).saturating_add(1),
            EventType::parse(event_type)?,
            by.correlation_id,
            by.actor.clone(),
        )
        .with_payload(payload),
    );
    Ok(())
}

/// The typed refusal travels out of band as `MutationRejected`; give it back its type.
fn unwrap_rejection(error: EventError) -> IdentityError {
    match error {
        EventError::MutationRejected { message, .. } => {
            if let Some(rest) = message.strip_prefix("identity conflict: ") {
                IdentityError::Conflict(rest.to_string())
            } else if let Some(rest) = message.strip_prefix("identity not found: ") {
                IdentityError::NotFound(rest.to_string())
            } else if let Some(rest) = message.strip_prefix("identity schema: ") {
                IdentityError::Schema(rest.to_string())
            } else {
                IdentityError::Schema(message)
            }
        }
        other => IdentityError::Event(other),
    }
}
