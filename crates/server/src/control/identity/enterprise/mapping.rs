//! External groups → internal roles (OPS-001, DOMAIN.md §2).
//!
//! The mapping rows are the only source of an SSO or SCIM user's roles. A client never sends a
//! role this module reads, and no id_token claim other than the configured groups claim is
//! consulted. A scope that appears in any mapping row is IdP-managed:
//! - for the tenant, the membership's role is the highest mapped one, or `member` when no group
//!   matches;
//! - for a workspace, the membership has the highest mapped role, or is removed when no group
//!   maps the user there.
//!
//! A scope no row mentions keeps whatever an administrator set by hand. `owner` is never mapped,
//! and an owner's memberships are never changed by a mapping.

use std::collections::{BTreeMap, BTreeSet};

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};

use crate::policy::rbac::{TenantRole, WorkspaceRole};

/// One mapping row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// The external group as the IdP names it (id_token claim value, SCIM displayName or externalId).
    pub external_group: String,
    /// `""` for the tenant row, else the `ws_` id.
    pub scope: String,
    /// The tenant role a tenant row grants.
    pub tenant_role: Option<TenantRole>,
    /// The workspace role a workspace row grants.
    pub workspace_role: Option<WorkspaceRole>,
}

/// What the mappings say for one user's groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// The tenant role, when any tenant row exists (the tenant role is IdP-managed).
    pub tenant_role: Option<TenantRole>,
    /// Every workspace some row mentions, with the role the groups earn there (`None`: no access).
    pub workspaces: BTreeMap<String, Option<WorkspaceRole>>,
}

const fn tenant_rank(role: TenantRole) -> u8 {
    match role {
        TenantRole::Owner => 4,
        TenantRole::Admin => 3,
        TenantRole::Billing => 2,
        TenantRole::Member => 1,
    }
}

const fn workspace_rank(role: WorkspaceRole) -> u8 {
    match role {
        WorkspaceRole::Admin => 4,
        WorkspaceRole::Approver => 3,
        WorkspaceRole::Editor => 2,
        WorkspaceRole::Viewer => 1,
    }
}

/// Resolve `groups` against `mappings`: the highest role wins in each scope.
#[must_use]
pub fn resolve(mappings: &[Mapping], groups: &[String]) -> Resolved {
    let held: BTreeSet<&str> = groups.iter().map(String::as_str).collect();
    let mut resolved = Resolved::default();
    let mut tenant_managed = false;
    let mut tenant_best: Option<TenantRole> = None;
    for mapping in mappings {
        let matched = held.contains(mapping.external_group.as_str());
        if mapping.scope.is_empty() {
            tenant_managed = true;
            if let (true, Some(role)) = (matched, mapping.tenant_role) {
                if role != TenantRole::Owner
                    && tenant_best.is_none_or(|best| tenant_rank(role) > tenant_rank(best))
                {
                    tenant_best = Some(role);
                }
            }
        } else {
            let entry = resolved
                .workspaces
                .entry(mapping.scope.clone())
                .or_insert(None);
            if let (true, Some(role)) = (matched, mapping.workspace_role) {
                if entry.is_none_or(|best| workspace_rank(role) > workspace_rank(best)) {
                    *entry = Some(role);
                }
            }
        }
    }
    if tenant_managed {
        resolved.tenant_role = Some(tenant_best.unwrap_or(TenantRole::Member));
    }
    resolved
}

/// Read the tenant's mapping rows inside `tx` (tenant context set).
///
/// # Errors
/// Returns the database error.
pub async fn load_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
) -> Result<Vec<Mapping>, sqlx::Error> {
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT external_group, scope, tenant_role, workspace_role FROM sso_group_mappings \
         WHERE tenant_id = $1 ORDER BY external_group, scope",
    )
    .bind(tenant_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(external_group, scope, tenant_role, workspace_role)| Mapping {
                external_group,
                scope,
                tenant_role: tenant_role.and_then(|role| TenantRole::parse(&role).ok()),
                workspace_role: workspace_role.and_then(|role| WorkspaceRole::parse(&role).ok()),
            },
        )
        .collect())
}

/// The groups that count for `user_id` in the tenant: its SCIM groups (display names and
/// external ids) when the tenant provisions groups, otherwise the groups its last SSO sign-in
/// asserted.
///
/// # Errors
/// Returns the database error.
pub async fn groups_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let provisions_groups: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM directory_groups WHERE tenant_id = $1)")
            .bind(tenant_id)
            .fetch_one(&mut **tx)
            .await?;
    if provisions_groups {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT g.display_name, g.external_id FROM directory_group_members m \
             JOIN directory_groups g ON g.id = m.group_id \
             WHERE m.tenant_id = $1 AND m.user_id = $2",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_all(&mut **tx)
        .await?;
        return Ok(rows
            .into_iter()
            .flat_map(|(name, external)| std::iter::once(name).chain(external))
            .collect());
    }
    let asserted: Option<Value> = sqlx::query_scalar(
        "SELECT sso_groups FROM external_identities WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(asserted
        .as_ref()
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// A fresh membership id (`tm_` / `wm_` share the ULID space of canonical ids).
#[must_use]
pub fn membership_id(prefix: &str) -> String {
    let id = CanonicalId::generate(Prefix::User, &mut UlidGenerator::new()).to_string();
    format!("{prefix}{}", &id[4..])
}

/// Apply `resolved` to the user's memberships inside `tx`. The tenant membership must already
/// exist; an owner is left untouched. `default_workspace` gives a user with no workspace at all a
/// `viewer` membership there when no mapping manages that workspace. Returns the changes made.
///
/// # Errors
/// Returns the database error.
pub async fn apply_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
    resolved: &Resolved,
    default_workspace: Option<&str>,
) -> Result<Vec<Value>, sqlx::Error> {
    let mut changes = Vec::new();
    let current: Option<(String, String)> = sqlx::query_as(
        "SELECT role, status FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((role, status)) = current else {
        return Ok(changes);
    };
    if role == "owner" || status != "active" {
        return Ok(changes);
    }
    if let Some(mapped) = resolved.tenant_role {
        if mapped.as_str() != role {
            sqlx::query(
                "UPDATE tenant_memberships SET role = $3, updated_at = now() \
                 WHERE tenant_id = $1 AND user_id = $2",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(mapped.as_str())
            .execute(&mut **tx)
            .await?;
            changes.push(json!({ "scope": "tenant", "from": role, "to": mapped.as_str() }));
        }
    }
    for (workspace_id, mapped) in &resolved.workspaces {
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM workspaces WHERE tenant_id = $1 AND id = $2)",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .fetch_one(&mut **tx)
        .await?;
        if !known {
            continue;
        }
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT role, status FROM workspace_memberships \
             WHERE tenant_id = $1 AND workspace_id = $2 AND user_id = $3 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await?;
        match (mapped, existing) {
            (Some(role), Some((from_role, from_status)))
                if from_role == role.as_str() && from_status == "active" => {}
            (Some(role), existing) => {
                sqlx::query(
                    "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
                     VALUES ($1, $2, $3, $4, $5, 'active') \
                     ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role, \
                     status = 'active', updated_at = now()",
                )
                .bind(membership_id("wm_"))
                .bind(tenant_id)
                .bind(workspace_id)
                .bind(user_id)
                .bind(role.as_str())
                .execute(&mut **tx)
                .await?;
                changes.push(json!({
                    "scope": workspace_id,
                    "from": existing.map(|(role, status)| format!("{role}/{status}")),
                    "to": role.as_str(),
                }));
            }
            (None, Some((from_role, from_status))) if from_status != "removed" => {
                sqlx::query(
                    "UPDATE workspace_memberships SET status = 'removed', updated_at = now() \
                     WHERE tenant_id = $1 AND workspace_id = $2 AND user_id = $3",
                )
                .bind(tenant_id)
                .bind(workspace_id)
                .bind(user_id)
                .execute(&mut **tx)
                .await?;
                changes.push(json!({
                    "scope": workspace_id,
                    "from": format!("{from_role}/{from_status}"),
                    "to": "removed",
                }));
            }
            (None, _) => {}
        }
    }
    if let Some(workspace_id) = default_workspace {
        let has_any: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM workspace_memberships \
             WHERE tenant_id = $1 AND user_id = $2 AND status = 'active')",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await?;
        if !has_any && !resolved.workspaces.contains_key(workspace_id) {
            let inserted = sqlx::query(
                "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
                 SELECT $1, $2, $3, $4, 'viewer', 'active' \
                 WHERE EXISTS (SELECT 1 FROM workspaces WHERE tenant_id = $2 AND id = $3) \
                 ON CONFLICT (workspace_id, user_id) DO NOTHING",
            )
            .bind(membership_id("wm_"))
            .bind(tenant_id)
            .bind(workspace_id)
            .bind(user_id)
            .execute(&mut **tx)
            .await?;
            if inserted.rows_affected() > 0 {
                changes.push(json!({ "scope": workspace_id, "from": null, "to": "viewer" }));
            }
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(group: &str, role: TenantRole) -> Mapping {
        Mapping {
            external_group: group.into(),
            scope: String::new(),
            tenant_role: Some(role),
            workspace_role: None,
        }
    }

    fn workspace(group: &str, ws: &str, role: WorkspaceRole) -> Mapping {
        Mapping {
            external_group: group.into(),
            scope: ws.into(),
            tenant_role: None,
            workspace_role: Some(role),
        }
    }

    fn groups(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn the_highest_mapped_role_wins_and_unmatched_scopes_lose_access() {
        let mappings = vec![
            tenant("staff", TenantRole::Member),
            tenant("finance", TenantRole::Billing),
            tenant("it-admins", TenantRole::Admin),
            workspace("eng", "ws_A", WorkspaceRole::Editor),
            workspace("approvers", "ws_A", WorkspaceRole::Approver),
            workspace("eng", "ws_B", WorkspaceRole::Viewer),
        ];
        let resolved = resolve(
            &mappings,
            &groups(&["finance", "staff", "eng", "approvers"]),
        );
        assert_eq!(resolved.tenant_role, Some(TenantRole::Billing));
        assert_eq!(resolved.workspaces["ws_A"], Some(WorkspaceRole::Approver));
        assert_eq!(resolved.workspaces["ws_B"], Some(WorkspaceRole::Viewer));

        let resolved = resolve(&mappings, &groups(&["unrelated"]));
        assert_eq!(
            resolved.tenant_role,
            Some(TenantRole::Member),
            "managed tenant, no match"
        );
        assert_eq!(
            resolved.workspaces["ws_A"], None,
            "a managed workspace with no match"
        );
    }

    #[test]
    fn owner_is_never_mapped_and_unmanaged_scopes_are_left_alone() {
        let resolved = resolve(&[tenant("x", TenantRole::Owner)], &groups(&["x"]));
        assert_eq!(resolved.tenant_role, Some(TenantRole::Member));
        let resolved = resolve(
            &[workspace("eng", "ws_A", WorkspaceRole::Admin)],
            &groups(&["eng"]),
        );
        assert_eq!(
            resolved.tenant_role, None,
            "no tenant rows: the tenant role is not managed"
        );
        assert!(resolve(&[], &groups(&["eng"])).workspaces.is_empty());
    }
}
