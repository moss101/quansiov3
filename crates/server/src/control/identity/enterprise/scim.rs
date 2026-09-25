//! SCIM 2.0 provisioning (OPS-001, DOMAIN.md §2; RFC 7643/7644, the subset IdPs use).
//!
//! **Caller.** The tenant's IdP, as a service principal holding `Provisioning`. It sees only the
//! users with a membership in its tenant.
//!
//! **Users.**
//! - The resource `id` is the `usr_` id and `userName` is the sign-in address. `userName` cannot
//!   be changed through SCIM, because the account may belong to other tenants too.
//! - `active=false` suspends the tenant membership and the user's workspace memberships and
//!   revokes every session the user holds in the tenant, in one transaction with the
//!   `member.updated` event and the audit entry. The next request with any of the user's tokens is
//!   refused.
//! - `DELETE` removes the membership the same way.
//! - `active=true` restores the membership and recomputes the mapped roles.
//! - The last active owner cannot be deprovisioned.
//!
//! **Groups** (`dgr_`) carry members. Every membership change recomputes the affected members'
//! roles from the tenant's mappings: once a tenant provisions groups, SCIM is the source of groups.
//!
//! Attributes this product does not keep (titles, phone numbers, addresses) are accepted and
//! ignored, as SCIM allows; the ones it keeps are validated.

use chrono::{DateTime, SecondsFormat, Utc};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};

use super::mapping;
use super::principals::PrincipalScope;
use super::sessions::revoke_in;
use super::{
    audit_in, create_user_in, rejected, stage_in, stage_member_in, user_by_email_in, Acting,
    EnterpriseError, EnterpriseIdentity,
};
use crate::control::identity::auth::normalize_email;

const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
const LIST_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
const ERROR_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:Error";

/// A SCIM error response (RFC 7644 §3.12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimError {
    /// HTTP status.
    pub status: u16,
    /// `scimType`, when one applies.
    pub scim_type: Option<&'static str>,
    /// Human-readable detail.
    pub detail: String,
}

impl ScimError {
    fn new(status: u16, scim_type: Option<&'static str>, detail: impl Into<String>) -> Self {
        Self {
            status,
            scim_type,
            detail: detail.into(),
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self::new(400, Some("invalidValue"), detail)
    }

    /// The response body.
    #[must_use]
    pub fn body(&self) -> Value {
        let mut body = json!({
            "schemas": [ERROR_SCHEMA],
            "status": self.status.to_string(),
            "detail": self.detail,
        });
        if let Some(scim_type) = self.scim_type {
            body["scimType"] = json!(scim_type);
        }
        body
    }
}

impl From<EnterpriseError> for ScimError {
    fn from(error: EnterpriseError) -> Self {
        match error {
            EnterpriseError::Invalid(detail) => Self::invalid(detail),
            EnterpriseError::NotFound(detail) => Self::new(404, None, detail),
            EnterpriseError::Conflict(detail) => Self::new(409, Some("uniqueness"), detail),
            EnterpriseError::Forbidden(detail) => Self::new(403, None, detail),
            EnterpriseError::Unauthenticated(detail) => Self::new(401, None, detail),
            other => Self::new(500, None, other.to_string()),
        }
    }
}

/// A SCIM success: status and body (`None` for 204).
#[derive(Debug, Clone, PartialEq)]
pub struct ScimReply {
    /// HTTP status.
    pub status: u16,
    /// Body.
    pub body: Option<Value>,
}

impl ScimReply {
    fn ok(body: Value) -> Self {
        Self {
            status: 200,
            body: Some(body),
        }
    }
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `"True"`/`"False"` as Entra ID sends them, or a JSON boolean.
fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) if text.eq_ignore_ascii_case("true") => Some(true),
        Value::String(text) if text.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// `attr eq "value"`, the one filter form IdPs rely on.
fn equality_filter(filter: &str) -> Result<(String, String), ScimError> {
    let invalid = || {
        ScimError::new(
            400,
            Some("invalidFilter"),
            format!("unsupported filter {filter:?}"),
        )
    };
    let (attribute, rest) = filter
        .trim()
        .split_once(char::is_whitespace)
        .ok_or_else(invalid)?;
    let (operator, value) = rest
        .trim()
        .split_once(char::is_whitespace)
        .ok_or_else(invalid)?;
    if !operator.eq_ignore_ascii_case("eq") {
        return Err(invalid());
    }
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .ok_or_else(invalid)?
        .replace("\\\"", "\u{0}");
    // One quoted value: a quote left inside means a compound filter.
    if value.contains('"') {
        return Err(invalid());
    }
    Ok((attribute.to_ascii_lowercase(), value.replace('\u{0}', "\"")))
}

/// The page of a list: `startIndex` is 1-based.
fn page(start_index: Option<usize>, count: Option<usize>, max: usize) -> (usize, usize) {
    (
        start_index.unwrap_or(1).max(1),
        count.unwrap_or(max).min(max),
    )
}

fn list_response(total: usize, start: usize, resources: Vec<Value>) -> Value {
    json!({
        "schemas": [LIST_SCHEMA],
        "totalResults": total,
        "startIndex": start,
        "itemsPerPage": resources.len(),
        "Resources": resources,
    })
}

/// Fields of a User resource this product keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct UserChange {
    user_name: Option<String>,
    display_name: Option<String>,
    external_id: Option<Option<String>>,
    active: Option<bool>,
}

fn display_name_of(body: &Value) -> Option<String> {
    body.get("displayName")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            body.pointer("/name/formatted")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            let given = body.pointer("/name/givenName").and_then(Value::as_str);
            let family = body.pointer("/name/familyName").and_then(Value::as_str);
            match (given, family) {
                (None, None) => None,
                (given, family) => Some(
                    [given, family]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            }
        })
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn user_change_of(body: &Value) -> Result<UserChange, ScimError> {
    let mut change = UserChange {
        user_name: body
            .get("userName")
            .and_then(Value::as_str)
            .map(str::to_string),
        display_name: display_name_of(body),
        ..UserChange::default()
    };
    if let Some(external) = body.get("externalId") {
        change.external_id = Some(external.as_str().map(str::to_string));
    }
    if let Some(active) = body.get("active") {
        change.active =
            Some(boolean(active).ok_or_else(|| ScimError::invalid("active must be a boolean"))?);
    }
    Ok(change)
}

/// Fold PATCH operations into one change (RFC 7644 §3.5.2).
fn patch_change(body: &Value) -> Result<UserChange, ScimError> {
    let operations = body
        .get("Operations")
        .and_then(Value::as_array)
        .ok_or_else(|| ScimError::invalid("a PATCH carries Operations"))?;
    let mut change = UserChange::default();
    for operation in operations {
        let op = operation
            .get("op")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| ScimError::invalid("an operation needs op"))?;
        let value = operation.get("value").cloned().unwrap_or(Value::Null);
        match operation.get("path").and_then(Value::as_str) {
            None => {
                if op == "remove" {
                    return Err(ScimError::new(400, Some("noTarget"), "remove needs a path"));
                }
                let partial = user_change_of(&value)?;
                change.user_name = partial.user_name.or(change.user_name);
                change.display_name = partial.display_name.or(change.display_name);
                change.external_id = partial.external_id.or(change.external_id);
                change.active = partial.active.or(change.active);
            }
            Some(path) => match (path.to_ascii_lowercase().as_str(), op.as_str()) {
                ("active", "add" | "replace") => {
                    change.active = Some(
                        boolean(&value)
                            .ok_or_else(|| ScimError::invalid("active must be a boolean"))?,
                    );
                }
                ("displayname", "add" | "replace") => {
                    change.display_name = value.as_str().map(str::to_string);
                }
                ("externalid", "add" | "replace") => {
                    change.external_id = Some(value.as_str().map(str::to_string));
                }
                ("externalid", "remove") => change.external_id = Some(None),
                ("username", "add" | "replace") => {
                    change.user_name = value.as_str().map(str::to_string);
                }
                ("active" | "username", "remove") => {
                    return Err(ScimError::new(
                        400,
                        Some("mutability"),
                        format!("{path} cannot be removed"),
                    ));
                }
                // Attributes this product does not keep.
                _ => {}
            },
        }
    }
    Ok(change)
}

/// Member ids named by a Group body or operation value.
fn member_ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("value").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Group changes folded from a body or PATCH operations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GroupChange {
    display_name: Option<String>,
    external_id: Option<Option<String>>,
    replace_members: Option<Vec<String>>,
    add_members: Vec<String>,
    remove_members: Vec<String>,
}

fn group_patch(body: &Value) -> Result<GroupChange, ScimError> {
    let operations = body
        .get("Operations")
        .and_then(Value::as_array)
        .ok_or_else(|| ScimError::invalid("a PATCH carries Operations"))?;
    let mut change = GroupChange::default();
    for operation in operations {
        let op = operation
            .get("op")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| ScimError::invalid("an operation needs op"))?;
        let value = operation.get("value").cloned().unwrap_or(Value::Null);
        let path = operation.get("path").and_then(Value::as_str).unwrap_or("");
        let lower = path.to_ascii_lowercase();
        match (op.as_str(), lower.as_str()) {
            ("add", "members") => change.add_members.extend(member_ids(&value)),
            ("replace", "members") => change.replace_members = Some(member_ids(&value)),
            ("remove", "members") if value.is_array() => {
                change.remove_members.extend(member_ids(&value))
            }
            ("remove", "members") => change.replace_members = Some(Vec::new()),
            ("remove", filtered) if filtered.starts_with("members[") => {
                // members[value eq "usr_…"]
                let inner = path
                    .split_once('[')
                    .and_then(|(_, rest)| rest.strip_suffix(']'))
                    .ok_or_else(|| {
                        ScimError::new(
                            400,
                            Some("invalidPath"),
                            format!("unsupported path {path:?}"),
                        )
                    })?;
                let (attribute, id) = equality_filter(inner)?;
                if attribute != "value" {
                    return Err(ScimError::new(
                        400,
                        Some("invalidPath"),
                        format!("unsupported path {path:?}"),
                    ));
                }
                change.remove_members.push(id);
            }
            ("add" | "replace", "displayname") => {
                change.display_name = value.as_str().map(str::to_string)
            }
            ("add" | "replace", "externalid") => {
                change.external_id = Some(value.as_str().map(str::to_string))
            }
            ("add" | "replace", "") => {
                if let Some(name) = value.get("displayName").and_then(Value::as_str) {
                    change.display_name = Some(name.to_string());
                }
                if let Some(external) = value.get("externalId") {
                    change.external_id = Some(external.as_str().map(str::to_string));
                }
                if let Some(members) = value.get("members") {
                    if op == "add" {
                        change.add_members.extend(member_ids(members));
                    } else {
                        change.replace_members = Some(member_ids(members));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(change)
}

type UserRow = (
    String,
    String,
    String,
    DateTime<Utc>,
    DateTime<Utc>,
    String,
    Option<String>,
);

/// Read one User resource inside `tx`; `None` when the user has no live membership here.
async fn user_resource_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user_id: &str,
) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<UserRow> = sqlx::query_as(
        "SELECT u.id, u.primary_email, u.display_name, m.created_at, GREATEST(m.updated_at, u.updated_at), \
         m.status, e.external_id FROM tenant_memberships m JOIN users u ON u.id = m.user_id \
         LEFT JOIN external_identities e ON e.tenant_id = m.tenant_id AND e.user_id = m.user_id \
         WHERE m.tenant_id = $1 AND m.user_id = $2 AND m.status <> 'removed'",
    )
    .bind(tenant)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((id, email, name, created, modified, status, external_id)) = row else {
        return Ok(None);
    };
    let groups: Vec<(String, String)> = sqlx::query_as(
        "SELECT g.id, g.display_name FROM directory_group_members m JOIN directory_groups g ON g.id = m.group_id \
         WHERE m.tenant_id = $1 AND m.user_id = $2 ORDER BY g.display_name",
    )
    .bind(tenant)
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut resource = json!({
        "schemas": [USER_SCHEMA],
        "id": id,
        "userName": email,
        "displayName": name,
        "name": { "formatted": name },
        "emails": [{ "value": email, "type": "work", "primary": true }],
        "active": status == "active" || status == "invited",
        "groups": groups.iter().map(|(gid, gname)| json!({
            "value": gid, "display": gname, "$ref": format!("/scim/v2/Groups/{gid}"),
        })).collect::<Vec<_>>(),
        "meta": {
            "resourceType": "User",
            "created": rfc3339(created),
            "lastModified": rfc3339(modified),
            "location": format!("/scim/v2/Users/{id}"),
        },
    });
    if let Some(external_id) = external_id {
        resource["externalId"] = json!(external_id);
    }
    Ok(Some(resource))
}

type GroupRow = (String, String, Option<String>, DateTime<Utc>, DateTime<Utc>);

async fn group_resource_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    group_id: &str,
) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<GroupRow> = sqlx::query_as(
        "SELECT id, display_name, external_id, created_at, updated_at FROM directory_groups \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(group_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((id, name, external_id, created, modified)) = row else {
        return Ok(None);
    };
    let members: Vec<(String, String)> = sqlx::query_as(
        "SELECT u.id, u.primary_email FROM directory_group_members m JOIN users u ON u.id = m.user_id \
         WHERE m.tenant_id = $1 AND m.group_id = $2 ORDER BY u.primary_email",
    )
    .bind(tenant)
    .bind(group_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut resource = json!({
        "schemas": [GROUP_SCHEMA],
        "id": id,
        "displayName": name,
        "members": members.iter().map(|(uid, email)| json!({
            "value": uid, "display": email, "$ref": format!("/scim/v2/Users/{uid}"),
        })).collect::<Vec<_>>(),
        "meta": {
            "resourceType": "Group",
            "created": rfc3339(created),
            "lastModified": rfc3339(modified),
            "location": format!("/scim/v2/Groups/{id}"),
        },
    });
    if let Some(external_id) = external_id {
        resource["externalId"] = json!(external_id);
    }
    Ok(Some(resource))
}

/// The membership row of a SCIM-visible user, locked.
async fn live_membership_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
    user_id: &str,
) -> Result<(String, String, String), quansio_events::EventError> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, role, status FROM tenant_memberships \
         WHERE tenant_id = $1 AND user_id = $2 AND status <> 'removed' FOR UPDATE",
    )
    .bind(tenant)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.ok_or_else(|| {
        rejected(EnterpriseError::NotFound(format!(
            "user {user_id} is not provisioned here"
        )))
    })
}

/// Refuse to deprovision the tenant's last active owner.
async fn guard_owner_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
    user_id: &str,
    role: &str,
) -> Result<(), quansio_events::EventError> {
    if role != "owner" {
        return Ok(());
    }
    let others: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tenant_memberships WHERE tenant_id = $1 AND user_id <> $2 \
         AND role = 'owner' AND status = 'active'",
    )
    .bind(tenant)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    if others == 0 {
        return Err(rejected(EnterpriseError::Conflict(
            "the last active owner cannot be deprovisioned".to_string(),
        )));
    }
    Ok(())
}

/// Suspend or restore a membership; returns the change for the event.
async fn set_active_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
    user_id: &str,
    role: &str,
    active: bool,
    default_workspace: Option<&str>,
) -> Result<Value, quansio_events::EventError> {
    if active {
        sqlx::query(
            "UPDATE tenant_memberships SET status = 'active', updated_at = now() \
             WHERE tenant_id = $1 AND user_id = $2",
        )
        .bind(tenant)
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "UPDATE workspace_memberships SET status = 'active', updated_at = now() \
             WHERE tenant_id = $1 AND user_id = $2 AND status = 'suspended'",
        )
        .bind(tenant)
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
        let changes = recompute_in(tx, tenant, user_id, default_workspace).await?;
        return Ok(json!({ "status": "active", "role_changes": changes }));
    }
    guard_owner_in(tx, tenant, user_id, role).await?;
    sqlx::query(
        "UPDATE tenant_memberships SET status = 'suspended', updated_at = now() \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE workspace_memberships SET status = 'suspended', updated_at = now() \
         WHERE tenant_id = $1 AND user_id = $2 AND status IN ('active', 'invited')",
    )
    .bind(tenant)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    let revoked = revoke_in(tx, tenant, user_id, "deprovisioned by the directory").await?;
    Ok(json!({ "status": "suspended", "revoked_sessions": revoked }))
}

/// Recompute one member's mapped roles.
async fn recompute_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
    user_id: &str,
    default_workspace: Option<&str>,
) -> Result<Vec<Value>, sqlx::Error> {
    let mappings = mapping::load_in(tx, tenant).await?;
    let groups = mapping::groups_in(tx, tenant, user_id).await?;
    let resolved = mapping::resolve(&mappings, &groups);
    mapping::apply_in(tx, tenant, user_id, &resolved, default_workspace).await
}

async fn default_workspace_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT default_workspace_id FROM sso_connections WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_optional(&mut **tx)
    .await
    .map(Option::flatten)
}

/// Recompute the roles of `users` after a group change and stage one event per changed member.
async fn recompute_members_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut quansio_events::EventBatch,
    tenant: &str,
    acting: &Acting,
    users: &[String],
) -> Result<(), quansio_events::EventError> {
    let mut seen = std::collections::BTreeSet::new();
    for user_id in users {
        if !seen.insert(user_id.clone()) {
            continue;
        }
        let membership: Option<(String, String)> = sqlx::query_as(
            "SELECT id, status FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2",
        )
        .bind(tenant)
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some((membership_id, status)) = membership else {
            continue;
        };
        if status != "active" {
            continue;
        }
        let changes = recompute_in(tx, tenant, user_id, None).await?;
        if !changes.is_empty() {
            stage_member_in(
                tx,
                batch,
                tenant,
                acting,
                &membership_id,
                "member.updated",
                json!({ "membership_id": membership_id, "user_id": user_id, "source": "scim", "changes": changes }),
            )
            .await?;
        }
    }
    Ok(())
}

/// Every id in `ids` must be a user provisioned in the tenant.
async fn check_members_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
    ids: &[String],
) -> Result<(), quansio_events::EventError> {
    for id in ids {
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2 \
             AND status <> 'removed')",
        )
        .bind(tenant)
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
        if !known {
            return Err(rejected(EnterpriseError::Invalid(format!(
                "member {id} is not a user provisioned in this tenant"
            ))));
        }
    }
    Ok(())
}

impl EnterpriseIdentity {
    /// `GET /scim/v2/ServiceProviderConfig`.
    #[must_use]
    pub fn scim_service_provider_config(&self) -> Value {
        json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],
            "patch": { "supported": true },
            "bulk": { "supported": false, "maxOperations": 0, "maxPayloadSize": 0 },
            "filter": { "supported": true, "maxResults": self.config.scim_max_results },
            "changePassword": { "supported": false },
            "sort": { "supported": false },
            "etag": { "supported": false },
            "authenticationSchemes": [{
                "type": "oauthbearertoken",
                "name": "Service principal key",
                "description": "A qsk_ key of a service principal holding the Provisioning scope",
            }],
        })
    }

    async fn user_reply(
        &self,
        tenant: &str,
        user_id: &str,
        status: u16,
    ) -> Result<ScimReply, ScimError> {
        let mut tx = self.tenant_transaction(tenant).await?;
        let resource = user_resource_in(&mut tx, tenant, user_id)
            .await
            .map_err(EnterpriseError::from)?;
        tx.commit().await.map_err(EnterpriseError::from)?;
        let resource = resource.ok_or_else(|| {
            ScimError::new(404, None, format!("user {user_id} is not provisioned here"))
        })?;
        Ok(ScimReply {
            status,
            body: Some(resource),
        })
    }

    async fn group_reply(
        &self,
        tenant: &str,
        group_id: &str,
        status: u16,
    ) -> Result<ScimReply, ScimError> {
        let mut tx = self.tenant_transaction(tenant).await?;
        let resource = group_resource_in(&mut tx, tenant, group_id)
            .await
            .map_err(EnterpriseError::from)?;
        tx.commit().await.map_err(EnterpriseError::from)?;
        let resource = resource
            .ok_or_else(|| ScimError::new(404, None, format!("group {group_id} does not exist")))?;
        Ok(ScimReply {
            status,
            body: Some(resource),
        })
    }

    /// `GET /scim/v2/Users[?filter=][&startIndex=][&count=]`.
    ///
    /// # Errors
    /// [`ScimError`] 400 `invalidFilter` for a filter other than `attr eq "value"`.
    pub async fn scim_list_users(
        &self,
        principal: &PrincipalScope,
        filter: Option<&str>,
        start_index: Option<usize>,
        count: Option<usize>,
    ) -> Result<ScimReply, ScimError> {
        let tenant = principal.tenant_id.as_str();
        let (column, value) = match filter {
            None => (None, String::new()),
            Some(filter) => {
                let (attribute, value) = equality_filter(filter)?;
                let column = match attribute.as_str() {
                    "username" | "emails.value" | "emails" => "u.primary_email",
                    "externalid" => "e.external_id",
                    "id" => "u.id",
                    _ => {
                        return Err(ScimError::new(
                            400,
                            Some("invalidFilter"),
                            format!("cannot filter on {attribute}"),
                        ))
                    }
                };
                let value = if column == "u.primary_email" {
                    value.to_ascii_lowercase()
                } else {
                    value
                };
                (Some(column), value)
            }
        };
        let (start, count) = page(start_index, count, self.config.scim_max_results);
        let condition = column.map_or_else(String::new, |column| format!(" AND {column} = $2"));
        let mut tx = self.tenant_transaction(tenant).await?;
        let ids: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT u.id FROM tenant_memberships m JOIN users u ON u.id = m.user_id \
             LEFT JOIN external_identities e ON e.tenant_id = m.tenant_id AND e.user_id = m.user_id \
             WHERE m.tenant_id = $1 AND m.status <> 'removed'{condition} ORDER BY u.primary_email"
        ))
        .bind(tenant)
        .bind(&value)
        .fetch_all(&mut *tx)
        .await
        .map_err(EnterpriseError::from)?;
        let mut resources = Vec::new();
        for id in ids.iter().skip(start - 1).take(count) {
            if let Some(resource) = user_resource_in(&mut tx, tenant, id)
                .await
                .map_err(EnterpriseError::from)?
            {
                resources.push(resource);
            }
        }
        tx.commit().await.map_err(EnterpriseError::from)?;
        Ok(ScimReply::ok(list_response(ids.len(), start, resources)))
    }

    /// `GET /scim/v2/Users/{id}`.
    ///
    /// # Errors
    /// [`ScimError`] 404 for a user not provisioned here.
    pub async fn scim_get_user(
        &self,
        principal: &PrincipalScope,
        id: &str,
    ) -> Result<ScimReply, ScimError> {
        self.user_reply(&principal.tenant_id, id, 200).await
    }

    /// `POST /scim/v2/Users`: provision (or link an existing account to) a member.
    ///
    /// # Errors
    /// [`ScimError`] 400 for a bad userName, 409 `uniqueness` when already provisioned.
    pub async fn scim_create_user(
        &self,
        principal: &PrincipalScope,
        body: &Value,
    ) -> Result<ScimReply, ScimError> {
        let change = user_change_of(body)?;
        let email = change
            .user_name
            .as_deref()
            .ok_or_else(|| ScimError::invalid("userName is required"))
            .and_then(|name| {
                normalize_email(name)
                    .map_err(|_| ScimError::invalid("userName must be an email address"))
            })?;
        let display_name = change
            .display_name
            .clone()
            .unwrap_or_else(|| email.split('@').next().unwrap_or("member").to_string());
        let external_id = change.external_id.clone().flatten();
        let active = change.active.unwrap_or(true);
        let tenant = principal.tenant_id.clone();
        let acting = Acting::service(&principal.id);
        let user_id = self
            .events()
            .commit_mutation_tx(&principal.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    if let Some(external_id) = &external_id {
                        let taken: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM external_identities WHERE tenant_id = $1 AND external_id = $2)",
                        )
                        .bind(&tenant)
                        .bind(external_id)
                        .fetch_one(&mut **tx)
                        .await?;
                        if taken {
                            return Err(rejected(EnterpriseError::Conflict(format!(
                                "externalId {external_id} is already provisioned"
                            ))));
                        }
                    }
                    let (user_id, origin) = match user_by_email_in(tx, &email).await? {
                        Some(user_id) => {
                            let member: Option<String> = sqlx::query_scalar(
                                "SELECT status FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2",
                            )
                            .bind(&tenant)
                            .bind(&user_id)
                            .fetch_optional(&mut **tx)
                            .await?;
                            if member.as_deref().is_some_and(|status| status != "removed") {
                                return Err(rejected(EnterpriseError::Conflict(format!(
                                    "{email} is already provisioned"
                                ))));
                            }
                            (user_id, "linked")
                        }
                        None => (create_user_in(tx, &email, &display_name, "sso").await?, "provisioned"),
                    };
                    sqlx::query(
                        "INSERT INTO external_identities (tenant_id, user_id, external_id, origin) \
                         VALUES ($1, $2, $3, $4) ON CONFLICT (tenant_id, user_id) \
                         DO UPDATE SET external_id = EXCLUDED.external_id",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .bind(&external_id)
                    .bind(origin)
                    .execute(&mut **tx)
                    .await?;
                    let status = if active { "active" } else { "suspended" };
                    let membership_id: String = sqlx::query_scalar(
                        "INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) \
                         VALUES ($1, $2, $3, 'member', $4) ON CONFLICT (tenant_id, user_id) \
                         DO UPDATE SET role = 'member', status = EXCLUDED.status, updated_at = now() \
                         RETURNING id",
                    )
                    .bind(mapping::membership_id("tm_"))
                    .bind(&tenant)
                    .bind(&user_id)
                    .bind(status)
                    .fetch_one(&mut **tx)
                    .await?;
                    let changes = if active {
                        let default_workspace = default_workspace_in(tx, &tenant).await?;
                        recompute_in(tx, &tenant, &user_id, default_workspace.as_deref()).await?
                    } else {
                        Vec::new()
                    };
                    stage_member_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        &membership_id,
                        "member.provisioned",
                        json!({ "membership_id": membership_id, "user_id": user_id, "source": "scim",
                                "origin": origin, "status": status, "changes": changes }),
                    )
                    .await?;
                    audit_in(tx, &tenant, &acting, "scim.create_user", &format!("user:{user_id}"), "applied",
                             &format!("{origin} as {status}"))
                        .await?;
                    Ok(user_id)
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        self.user_reply(&principal.tenant_id, &user_id, 201).await
    }

    /// `PUT /scim/v2/Users/{id}` and `PATCH /scim/v2/Users/{id}`.
    async fn scim_change_user(
        &self,
        principal: &PrincipalScope,
        id: &str,
        change: UserChange,
        operation: &'static str,
    ) -> Result<ScimReply, ScimError> {
        let tenant = principal.tenant_id.clone();
        let acting = Acting::service(&principal.id);
        let user_id = id.to_string();
        self.events()
            .commit_mutation_tx(&principal.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let (membership_id, role, status) = live_membership_in(tx, &tenant, &user_id).await?;
                    let (email, origin): (String, Option<String>) = sqlx::query_as(
                        "SELECT u.primary_email, e.origin FROM users u LEFT JOIN external_identities e \
                         ON e.tenant_id = $1 AND e.user_id = u.id WHERE u.id = $2",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .fetch_one(&mut **tx)
                    .await?;
                    if let Some(user_name) = &change.user_name {
                        if !user_name.trim().eq_ignore_ascii_case(&email) {
                            return Err(rejected(EnterpriseError::Invalid(
                                "userName cannot be changed through SCIM".to_string(),
                            )));
                        }
                    }
                    let mut detail = serde_json::Map::new();
                    if let (Some(name), Some("provisioned")) = (&change.display_name, origin.as_deref()) {
                        // Only an account this tenant created is renamed; a linked account's name is its own.
                        sqlx::query("UPDATE users SET display_name = $2, updated_at = now() WHERE id = $1")
                            .bind(&user_id)
                            .bind(name)
                            .execute(&mut **tx)
                            .await?;
                        detail.insert("display_name".into(), json!(name));
                    }
                    if let Some(external_id) = &change.external_id {
                        if let Some(value) = external_id {
                            let taken: bool = sqlx::query_scalar(
                                "SELECT EXISTS (SELECT 1 FROM external_identities WHERE tenant_id = $1 \
                                 AND external_id = $2 AND user_id <> $3)",
                            )
                            .bind(&tenant)
                            .bind(value)
                            .bind(&user_id)
                            .fetch_one(&mut **tx)
                            .await?;
                            if taken {
                                return Err(rejected(EnterpriseError::Conflict(format!(
                                    "externalId {value} belongs to another user"
                                ))));
                            }
                        }
                        sqlx::query(
                            "INSERT INTO external_identities (tenant_id, user_id, external_id, origin) \
                             VALUES ($1, $2, $3, 'linked') ON CONFLICT (tenant_id, user_id) \
                             DO UPDATE SET external_id = EXCLUDED.external_id",
                        )
                        .bind(&tenant)
                        .bind(&user_id)
                        .bind(external_id)
                        .execute(&mut **tx)
                        .await?;
                        detail.insert("external_id".into(), json!(external_id));
                    }
                    let is_active = status == "active" || status == "invited";
                    if let Some(active) = change.active.filter(|active| *active != is_active) {
                        let default_workspace = default_workspace_in(tx, &tenant).await?;
                        let outcome =
                            set_active_in(tx, &tenant, &user_id, &role, active, default_workspace.as_deref()).await?;
                        detail.insert("active".into(), outcome);
                    }
                    if detail.is_empty() {
                        return Err(rejected(EnterpriseError::Unchanged));
                    }
                    {
                        stage_member_in(
                            tx,
                            batch,
                            &tenant,
                            &acting,
                            &membership_id,
                            "member.updated",
                            json!({ "membership_id": membership_id, "user_id": user_id, "source": "scim", "change": detail }),
                        )
                        .await?;
                        audit_in(tx, &tenant, &acting, operation, &format!("user:{user_id}"), "applied",
                                 &Value::Object(detail).to_string())
                            .await?;
                    }
                    Ok(())
                })
            })
            .await
            .map_err(super::unwrap_rejection)
            .or_else(|error| match error {
                EnterpriseError::Unchanged => Ok(()),
                other => Err(other),
            })?;
        self.user_reply(&principal.tenant_id, id, 200).await
    }

    /// `PUT /scim/v2/Users/{id}`.
    ///
    /// # Errors
    /// [`ScimError`] 404 for a user not provisioned here, 400 for a changed userName.
    pub async fn scim_replace_user(
        &self,
        principal: &PrincipalScope,
        id: &str,
        body: &Value,
    ) -> Result<ScimReply, ScimError> {
        let mut change = user_change_of(body)?;
        change.active = Some(change.active.unwrap_or(true));
        if change.external_id.is_none() {
            change.external_id = Some(None);
        }
        self.scim_change_user(principal, id, change, "scim.replace_user")
            .await
    }

    /// `PATCH /scim/v2/Users/{id}`.
    ///
    /// # Errors
    /// [`ScimError`] 404 for a user not provisioned here, 400 for malformed operations.
    pub async fn scim_patch_user(
        &self,
        principal: &PrincipalScope,
        id: &str,
        body: &Value,
    ) -> Result<ScimReply, ScimError> {
        let change = patch_change(body)?;
        self.scim_change_user(principal, id, change, "scim.patch_user")
            .await
    }

    /// `DELETE /scim/v2/Users/{id}`: remove the membership and revoke the user's sessions here.
    ///
    /// # Errors
    /// [`ScimError`] 404 for a user not provisioned here, 409 for the last owner.
    pub async fn scim_delete_user(
        &self,
        principal: &PrincipalScope,
        id: &str,
    ) -> Result<ScimReply, ScimError> {
        let tenant = principal.tenant_id.clone();
        let acting = Acting::service(&principal.id);
        let user_id = id.to_string();
        self.events()
            .commit_mutation_tx(&principal.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let (membership_id, role, _) = live_membership_in(tx, &tenant, &user_id).await?;
                    guard_owner_in(tx, &tenant, &user_id, &role).await?;
                    sqlx::query(
                        "UPDATE tenant_memberships SET status = 'removed', updated_at = now() \
                         WHERE tenant_id = $1 AND user_id = $2",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query(
                        "UPDATE workspace_memberships SET status = 'removed', updated_at = now() \
                         WHERE tenant_id = $1 AND user_id = $2 AND status <> 'removed'",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query("DELETE FROM directory_group_members WHERE tenant_id = $1 AND user_id = $2")
                        .bind(&tenant)
                        .bind(&user_id)
                        .execute(&mut **tx)
                        .await?;
                    sqlx::query("DELETE FROM external_identities WHERE tenant_id = $1 AND user_id = $2")
                        .bind(&tenant)
                        .bind(&user_id)
                        .execute(&mut **tx)
                        .await?;
                    let revoked = revoke_in(tx, &tenant, &user_id, "deleted by the directory").await?;
                    stage_member_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        &membership_id,
                        "member.removed",
                        json!({ "membership_id": membership_id, "user_id": user_id, "source": "scim",
                                "revoked_sessions": revoked }),
                    )
                    .await?;
                    audit_in(tx, &tenant, &acting, "scim.delete_user", &format!("user:{user_id}"), "applied",
                             &format!("{revoked} session(s) revoked"))
                        .await?;
                    Ok(())
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        Ok(ScimReply {
            status: 204,
            body: None,
        })
    }

    /// `GET /scim/v2/Groups[?filter=][&startIndex=][&count=]`.
    ///
    /// # Errors
    /// [`ScimError`] 400 `invalidFilter` for an unsupported filter.
    pub async fn scim_list_groups(
        &self,
        principal: &PrincipalScope,
        filter: Option<&str>,
        start_index: Option<usize>,
        count: Option<usize>,
    ) -> Result<ScimReply, ScimError> {
        let tenant = principal.tenant_id.as_str();
        let (column, value) = match filter {
            None => (None, String::new()),
            Some(filter) => {
                let (attribute, value) = equality_filter(filter)?;
                let column = match attribute.as_str() {
                    "displayname" => "display_name",
                    "externalid" => "external_id",
                    "id" => "id",
                    _ => {
                        return Err(ScimError::new(
                            400,
                            Some("invalidFilter"),
                            format!("cannot filter on {attribute}"),
                        ))
                    }
                };
                (Some(column), value)
            }
        };
        let (start, count) = page(start_index, count, self.config.scim_max_results);
        let condition = column.map_or_else(String::new, |column| format!(" AND {column} = $2"));
        let mut tx = self.tenant_transaction(tenant).await?;
        let ids: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT id FROM directory_groups WHERE tenant_id = $1{condition} ORDER BY display_name"
        ))
        .bind(tenant)
        .bind(&value)
        .fetch_all(&mut *tx)
        .await
        .map_err(EnterpriseError::from)?;
        let mut resources = Vec::new();
        for id in ids.iter().skip(start - 1).take(count) {
            if let Some(resource) = group_resource_in(&mut tx, tenant, id)
                .await
                .map_err(EnterpriseError::from)?
            {
                resources.push(resource);
            }
        }
        tx.commit().await.map_err(EnterpriseError::from)?;
        Ok(ScimReply::ok(list_response(ids.len(), start, resources)))
    }

    /// `GET /scim/v2/Groups/{id}`.
    ///
    /// # Errors
    /// [`ScimError`] 404 for an unknown group.
    pub async fn scim_get_group(
        &self,
        principal: &PrincipalScope,
        id: &str,
    ) -> Result<ScimReply, ScimError> {
        self.group_reply(&principal.tenant_id, id, 200).await
    }

    /// `POST /scim/v2/Groups`.
    ///
    /// # Errors
    /// [`ScimError`] 400 for a missing name or unknown member, 409 for a taken name.
    pub async fn scim_create_group(
        &self,
        principal: &PrincipalScope,
        body: &Value,
    ) -> Result<ScimReply, ScimError> {
        let name = body
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 512)
            .ok_or_else(|| ScimError::invalid("displayName is required"))?
            .to_string();
        let external_id = body
            .get("externalId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let members = body.get("members").map(member_ids).unwrap_or_default();
        let group_id =
            CanonicalId::generate(Prefix::DirectoryGroup, &mut UlidGenerator::new()).to_string();
        let tenant = principal.tenant_id.clone();
        let acting = Acting::service(&principal.id);
        let id = group_id.clone();
        self.events()
            .commit_mutation_tx(&principal.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let taken: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM directory_groups WHERE tenant_id = $1 AND display_name = $2)",
                    )
                    .bind(&tenant)
                    .bind(&name)
                    .fetch_one(&mut **tx)
                    .await?;
                    if taken {
                        return Err(rejected(EnterpriseError::Conflict(format!("group {name:?} already exists"))));
                    }
                    check_members_in(tx, &tenant, &members).await?;
                    sqlx::query(
                        "INSERT INTO directory_groups (id, tenant_id, display_name, external_id) VALUES ($1, $2, $3, $4)",
                    )
                    .bind(&id)
                    .bind(&tenant)
                    .bind(&name)
                    .bind(&external_id)
                    .execute(&mut **tx)
                    .await?;
                    for member in &members {
                        sqlx::query(
                            "INSERT INTO directory_group_members (tenant_id, group_id, user_id) VALUES ($1, $2, $3) \
                             ON CONFLICT DO NOTHING",
                        )
                        .bind(&tenant)
                        .bind(&id)
                        .bind(member)
                        .execute(&mut **tx)
                        .await?;
                    }
                    // The first provisioned group makes SCIM the source of groups for every member.
                    let everyone: Vec<String> = sqlx::query_scalar(
                        "SELECT user_id FROM tenant_memberships WHERE tenant_id = $1 AND status = 'active'",
                    )
                    .bind(&tenant)
                    .fetch_all(&mut **tx)
                    .await?;
                    recompute_members_in(tx, batch, &tenant, &acting, &everyone).await?;
                    stage_in(tx, batch, &tenant, &acting, ("directory_group", &id), "tenant.directory_group_created",
                             json!({ "group_id": id, "display_name": name, "members": members }))
                        .await?;
                    audit_in(tx, &tenant, &acting, "scim.create_group", &format!("group:{id}"), "applied",
                             &format!("{name} with {} member(s)", members.len()))
                        .await?;
                    Ok(())
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        self.group_reply(&principal.tenant_id, &group_id, 201).await
    }

    async fn scim_change_group(
        &self,
        principal: &PrincipalScope,
        id: &str,
        change: GroupChange,
        operation: &'static str,
    ) -> Result<ScimReply, ScimError> {
        let tenant = principal.tenant_id.clone();
        let acting = Acting::service(&principal.id);
        let group_id = id.to_string();
        self.events()
            .commit_mutation_tx(&principal.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let exists: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM directory_groups WHERE tenant_id = $1 AND id = $2)",
                    )
                    .bind(&tenant)
                    .bind(&group_id)
                    .fetch_one(&mut **tx)
                    .await?;
                    if !exists {
                        return Err(rejected(EnterpriseError::NotFound(format!("group {group_id} does not exist"))));
                    }
                    if let Some(name) = &change.display_name {
                        let name = name.trim();
                        if name.is_empty() {
                            return Err(rejected(EnterpriseError::Invalid("displayName cannot be empty".to_string())));
                        }
                        let taken: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM directory_groups WHERE tenant_id = $1 AND display_name = $2 AND id <> $3)",
                        )
                        .bind(&tenant)
                        .bind(name)
                        .bind(&group_id)
                        .fetch_one(&mut **tx)
                        .await?;
                        if taken {
                            return Err(rejected(EnterpriseError::Conflict(format!("group {name:?} already exists"))));
                        }
                        sqlx::query("UPDATE directory_groups SET display_name = $3 WHERE tenant_id = $1 AND id = $2")
                            .bind(&tenant)
                            .bind(&group_id)
                            .bind(name)
                            .execute(&mut **tx)
                            .await?;
                    }
                    if let Some(external_id) = &change.external_id {
                        sqlx::query("UPDATE directory_groups SET external_id = $3 WHERE tenant_id = $1 AND id = $2")
                            .bind(&tenant)
                            .bind(&group_id)
                            .bind(external_id)
                            .execute(&mut **tx)
                            .await?;
                    }
                    let mut touched: Vec<String> = sqlx::query_scalar(
                        "SELECT user_id FROM directory_group_members WHERE tenant_id = $1 AND group_id = $2",
                    )
                    .bind(&tenant)
                    .bind(&group_id)
                    .fetch_all(&mut **tx)
                    .await?;
                    if let Some(members) = &change.replace_members {
                        check_members_in(tx, &tenant, members).await?;
                        sqlx::query("DELETE FROM directory_group_members WHERE tenant_id = $1 AND group_id = $2")
                            .bind(&tenant)
                            .bind(&group_id)
                            .execute(&mut **tx)
                            .await?;
                        for member in members {
                            sqlx::query(
                                "INSERT INTO directory_group_members (tenant_id, group_id, user_id) VALUES ($1, $2, $3) \
                                 ON CONFLICT DO NOTHING",
                            )
                            .bind(&tenant)
                            .bind(&group_id)
                            .bind(member)
                            .execute(&mut **tx)
                            .await?;
                        }
                        touched.extend(members.iter().cloned());
                    }
                    check_members_in(tx, &tenant, &change.add_members).await?;
                    for member in &change.add_members {
                        sqlx::query(
                            "INSERT INTO directory_group_members (tenant_id, group_id, user_id) VALUES ($1, $2, $3) \
                             ON CONFLICT DO NOTHING",
                        )
                        .bind(&tenant)
                        .bind(&group_id)
                        .bind(member)
                        .execute(&mut **tx)
                        .await?;
                    }
                    touched.extend(change.add_members.iter().cloned());
                    for member in &change.remove_members {
                        sqlx::query(
                            "DELETE FROM directory_group_members WHERE tenant_id = $1 AND group_id = $2 AND user_id = $3",
                        )
                        .bind(&tenant)
                        .bind(&group_id)
                        .bind(member)
                        .execute(&mut **tx)
                        .await?;
                    }
                    touched.extend(change.remove_members.iter().cloned());
                    // A renamed group can change every member's mapping.
                    recompute_members_in(tx, batch, &tenant, &acting, &touched).await?;
                    stage_in(tx, batch, &tenant, &acting, ("directory_group", &group_id), "tenant.directory_group_updated",
                             json!({ "group_id": group_id, "added": change.add_members, "removed": change.remove_members,
                                     "replaced": change.replace_members, "display_name": change.display_name }))
                        .await?;
                    audit_in(tx, &tenant, &acting, operation, &format!("group:{group_id}"), "applied",
                             &format!("+{} -{} members", change.add_members.len(), change.remove_members.len()))
                        .await?;
                    Ok(())
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        self.group_reply(&principal.tenant_id, id, 200).await
    }

    /// `PUT /scim/v2/Groups/{id}`.
    ///
    /// # Errors
    /// [`ScimError`] 404 for an unknown group, 400 for an unknown member.
    pub async fn scim_replace_group(
        &self,
        principal: &PrincipalScope,
        id: &str,
        body: &Value,
    ) -> Result<ScimReply, ScimError> {
        let change = GroupChange {
            display_name: body
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::to_string),
            external_id: Some(
                body.get("externalId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ),
            replace_members: Some(body.get("members").map(member_ids).unwrap_or_default()),
            ..GroupChange::default()
        };
        self.scim_change_group(principal, id, change, "scim.replace_group")
            .await
    }

    /// `PATCH /scim/v2/Groups/{id}`.
    ///
    /// # Errors
    /// [`ScimError`] 404 for an unknown group, 400 for malformed operations or unknown members.
    pub async fn scim_patch_group(
        &self,
        principal: &PrincipalScope,
        id: &str,
        body: &Value,
    ) -> Result<ScimReply, ScimError> {
        let change = group_patch(body)?;
        self.scim_change_group(principal, id, change, "scim.patch_group")
            .await
    }

    /// `DELETE /scim/v2/Groups/{id}`: the group goes and its former members' roles are recomputed.
    ///
    /// # Errors
    /// [`ScimError`] 404 for an unknown group.
    pub async fn scim_delete_group(
        &self,
        principal: &PrincipalScope,
        id: &str,
    ) -> Result<ScimReply, ScimError> {
        let tenant = principal.tenant_id.clone();
        let acting = Acting::service(&principal.id);
        let group_id = id.to_string();
        self.events()
            .commit_mutation_tx(&principal.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let members: Vec<String> = sqlx::query_scalar(
                        "SELECT user_id FROM directory_group_members WHERE tenant_id = $1 AND group_id = $2",
                    )
                    .bind(&tenant)
                    .bind(&group_id)
                    .fetch_all(&mut **tx)
                    .await?;
                    let deleted = sqlx::query("DELETE FROM directory_groups WHERE tenant_id = $1 AND id = $2")
                        .bind(&tenant)
                        .bind(&group_id)
                        .execute(&mut **tx)
                        .await?;
                    if deleted.rows_affected() == 0 {
                        return Err(rejected(EnterpriseError::NotFound(format!("group {group_id} does not exist"))));
                    }
                    recompute_members_in(tx, batch, &tenant, &acting, &members).await?;
                    stage_in(tx, batch, &tenant, &acting, ("directory_group", &group_id), "tenant.directory_group_deleted",
                             json!({ "group_id": group_id, "former_members": members }))
                        .await?;
                    audit_in(tx, &tenant, &acting, "scim.delete_group", &format!("group:{group_id}"), "applied",
                             &format!("{} former member(s)", members.len()))
                        .await?;
                    Ok(())
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        Ok(ScimReply {
            status: 204,
            body: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_equality_only() {
        assert_eq!(
            equality_filter(r#"userName eq "Ada@Corp.test""#).expect("filter"),
            ("username".to_string(), "Ada@Corp.test".to_string())
        );
        for bad in [
            r#"userName co "a""#,
            "userName eq a",
            "userName",
            r#"a eq "b" and c eq "d""#,
        ] {
            assert!(equality_filter(bad).is_err(), "{bad}");
        }
        assert_eq!(
            equality_filter(r#"displayName eq "say \"hi\"""#)
                .expect("escaped")
                .1,
            r#"say "hi""#
        );
    }

    #[test]
    fn patch_reads_entra_and_okta_shapes() {
        let entra = json!({"Operations": [{"op": "Replace", "path": "active", "value": "False"}]});
        assert_eq!(patch_change(&entra).expect("entra").active, Some(false));
        let okta = json!({"Operations": [{"op": "replace", "value": {"active": true, "displayName": "Ada L"}}]});
        let change = patch_change(&okta).expect("okta");
        assert_eq!(change.active, Some(true));
        assert_eq!(change.display_name.as_deref(), Some("Ada L"));
        let ignored = json!({"Operations": [{"op": "add", "path": "title", "value": "Engineer"}]});
        assert_eq!(
            patch_change(&ignored).expect("ignored"),
            UserChange::default()
        );
        let bad = json!({"Operations": [{"op": "replace", "path": "active", "value": "maybe"}]});
        assert!(patch_change(&bad).is_err());
        let group = json!({"Operations": [
            {"op": "add", "path": "members", "value": [{"value": "usr_1"}]},
            {"op": "remove", "path": "members[value eq \"usr_2\"]"},
            {"op": "Remove", "path": "members", "value": [{"value": "usr_3"}]},
        ]});
        let change = group_patch(&group).expect("group");
        assert_eq!(change.add_members, vec!["usr_1".to_string()]);
        assert_eq!(
            change.remove_members,
            vec!["usr_2".to_string(), "usr_3".to_string()]
        );
    }

    #[test]
    fn errors_have_the_scim_shape() {
        let body = ScimError::from(EnterpriseError::Conflict("taken".into())).body();
        assert_eq!(body["status"], "409");
        assert_eq!(body["scimType"], "uniqueness");
        assert_eq!(body["schemas"][0], ERROR_SCHEMA);
    }
}
