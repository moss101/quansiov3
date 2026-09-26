//! Administration and review (APP-012): members, policies, connectors, usage and the
//! audit log, every one of them decided by the server's RBAC for whichever client asks.
//!
//! Projections: `GET /v1/members` (manage_workspace), `GET /v1/policies`
//! (manage_workspace), `GET /v1/connectors` (manage_workspace), `GET /v1/usage`
//! (view_usage — tenant owner/admin/billing), `GET /v1/audit` (manage_tenant).
//! Commands: `InviteMember`, `UpdateMembership`, `RemoveMember` (manage_tenant for a
//! tenant membership, manage_workspace for a workspace one) and `SetPolicy`
//! (manage_tenant for the tenant policy, manage_workspace for a workspace's). Each
//! command's owner commits its `member.*` / `policy.*` event and its AuditEntry with the
//! change. The API module still writes only `commands`.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::audit;
use crate::control::identity::{IdentityError, Invitation, MemberAdmin};
use crate::control::schema;
use crate::policy::rbac::{ActionFamily, TenantRole, WorkspaceRole};
use crate::policy::{PolicyChange, PolicyError, PolicyScope, PolicyStore};
use crate::runtime::state_machine::RuntimeIdentity;
use crate::usage::UsageStore;

/// Audit rows one page returns at most.
pub const MAX_AUDIT_ROWS: i64 = 500;
/// Usage records `GET /v1/usage` lists (the newest); totals cover them all.
pub const MAX_USAGE_RECORDS: i64 = 500;

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

/// `?workspace_id=`.
#[derive(Debug, Default, Deserialize)]
pub struct WorkspaceQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /v1/members[?workspace_id=]`: every member with their roles.
pub async fn members(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    Authorization::check(
        state.pool(),
        &scope,
        query.workspace_id.as_deref(),
        ActionFamily::ManageWorkspace,
        "projection",
    )
    .await?;
    let members = state
        .auth()
        .identity()
        .list_members(&scope.tenant_id, query.workspace_id.as_deref())
        .await
        .map_err(projection_error)?;
    Ok(Json(
        json!({ "members": members.iter().map(crate::control::identity::MemberView::to_json).collect::<Vec<_>>() }),
    ))
}

fn policy_json(policy: &crate::policy::Policy) -> Value {
    json!({
        "id": policy.id,
        "scope": policy.scope.as_str(),
        "workspace_id": policy.workspace_id,
        "rules": policy.rules,
        "question_default_ttl_seconds": policy.question_default_ttl_seconds,
        "approval_default_ttl_seconds": policy.approval_default_ttl_seconds,
        "max_plan_nodes": policy.max_plan_nodes,
        "version": policy.version,
    })
}

/// `GET /v1/policies[?workspace_id=]`: the tenant policy and the workspace policies.
pub async fn policies(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    Authorization::check(
        state.pool(),
        &scope,
        query.workspace_id.as_deref(),
        ActionFamily::ManageWorkspace,
        "projection",
    )
    .await?;
    let store =
        PolicyStore::new(state.pool().clone(), identity_of(&scope)).map_err(projection_error)?;
    let policies = store
        .list_policies(query.workspace_id.as_deref())
        .await
        .map_err(projection_error)?;
    Ok(Json(
        json!({ "policies": policies.iter().map(policy_json).collect::<Vec<_>>() }),
    ))
}

/// `GET /v1/connectors[?workspace_id=]`: connector instances with their status; credential
/// handles are named, never materialized.
pub async fn connectors(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    Authorization::check(
        state.pool(),
        &scope,
        query.workspace_id.as_deref(),
        ActionFamily::ManageWorkspace,
        "projection",
    )
    .await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(
        "SELECT id, workspace_id, connector_id, display_name, credential_handle, metadata, status, \
                last_health_at, created_at, updated_at \
         FROM connector_instances WHERE tenant_id = $1 \
           AND ($2::text IS NULL OR workspace_id = $2 OR workspace_id IS NULL) \
         ORDER BY created_at DESC, id LIMIT 500",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({ "connectors": rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"),
        "workspace_id": row.get::<Option<String>, _>("workspace_id"),
        "connector_id": row.get::<String, _>("connector_id"),
        "display_name": row.get::<String, _>("display_name"),
        "credential_handle": row.get::<Option<String>, _>("credential_handle"),
        "metadata": row.get::<Value, _>("metadata"),
        "status": row.get::<String, _>("status"),
        "last_health_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_health_at"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
    })).collect::<Vec<_>>() })))
}

/// `GET /v1/usage[?workspace_id=]`: OPS-004's projection — totals per meter over the tenant's
/// whole history (the materialized `usage_records` and the events after them), and the newest
/// records behind them.
pub async fn usage(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    Authorization::check(
        state.pool(),
        &scope,
        query.workspace_id.as_deref(),
        ActionFamily::ViewUsage,
        "projection",
    )
    .await?;
    let summary = UsageStore::new(state.pool().clone(), scope.tenant_id.clone())
        .summary(query.workspace_id.as_deref(), MAX_USAGE_RECORDS)
        .await
        .map_err(projection_error)?;
    Ok(Json(json!({
        "events_read": summary.through_sequence,
        "complete": summary.complete,
        "meters": summary.meters.iter().map(|meter| json!({
            "meter": meter.meter.as_str(),
            "total": meter.total,
            "unit": meter.unit,
        })).collect::<Vec<_>>(),
        "record_count": summary.record_count,
        "records": summary.recent.iter().map(|record| json!({
            "id": record.id,
            "workspace_id": record.workspace_id,
            "meter": record.meter.as_str(),
            "quantity": record.quantity,
            "unit": record.unit,
            "cost_minor_units": record.cost_minor_units,
            "source_event_id": record.source_event_id,
            "occurred_at": record.occurred_at,
        })).collect::<Vec<_>>(),
    })))
}

/// `GET /v1/audit[?workspace_id=][&limit=]`: the tenant's hash-chained audit log, newest
/// first, with the chain verified from its genesis on every read.
pub async fn audit_log(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    Authorization::check(
        state.pool(),
        &scope,
        None,
        ActionFamily::ManageTenant,
        "projection",
    )
    .await?;
    let limit = query.limit.unwrap_or(100).clamp(1, MAX_AUDIT_ROWS);
    let entries = audit::list(
        state.pool(),
        &scope.tenant_id,
        query.workspace_id.as_deref(),
        limit,
    )
    .await
    .map_err(projection_error)?;
    let chain = audit::verify(state.pool(), &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    Ok(Json(json!({
        "chain": { "entries": chain.entries, "holds": chain.holds(), "broken_at": chain.broken_at },
        "entries": entries.iter().map(|record| json!({
            "id": record.entry.id,
            "workspace_id": record.entry.workspace_id,
            "actor": record.actor_json,
            "action": record.entry.action,
            "target_ref": record.entry.target_ref,
            "decision": record.entry.decision,
            "reason": record.entry.reason,
            "correlation_id": record.entry.correlation_id,
            "occurred_at": record.occurred_at,
            "prev_hash": record.entry.prev_hash,
            "hash": record.entry.hash,
        })).collect::<Vec<_>>(),
    })))
}

// ------------------------------------------------------------------ commands

fn identity_of(scope: &TenantScope) -> RuntimeIdentity {
    RuntimeIdentity::system(
        &scope.tenant_id,
        "api",
        quansio_core::CorrelationId::generate(&mut quansio_core::UlidGenerator::new()),
    )
}

fn identity_error(error: IdentityError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn policy_error(error: PolicyError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "POLICY_DENIED" => ApiErrorCode::PolicyDenied,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn acting_user<'a>(
    scope: &'a TenantScope,
    command: &str,
    correlation: &str,
) -> Result<&'a str, ApiError> {
    scope.user_id.as_deref().ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::AuthRequired,
            format!("{command} needs an acting user"),
            correlation,
        )
    })
}

fn member_admin(scope: &TenantScope, identity: &RuntimeIdentity, user_id: &str) -> MemberAdmin {
    let _ = scope;
    MemberAdmin {
        user_id: user_id.to_string(),
        actor: quansio_events::Actor::user(user_id),
        correlation_id: identity.correlation_id,
    }
}

/// `InviteMember {email, role, workspace_id?, workspace_role?}`.
pub async fn invite_member(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user_id = acting_user(scope, "InviteMember", correlation)?;
    let email = text(params, "email").ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "email is required",
            correlation,
        )
    })?;
    let role = TenantRole::parse(text(params, "role").unwrap_or("member")).map_err(|value| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("{value:?} is not a tenant role"),
            correlation,
        )
    })?;
    let workspace = match text(params, "workspace_id") {
        Some(workspace_id) => Some((
            workspace_id.to_string(),
            WorkspaceRole::parse(text(params, "workspace_role").unwrap_or("viewer")).map_err(
                |value| {
                    ApiError::new(
                        ApiErrorCode::ValidationSchema,
                        format!("{value:?} is not a workspace role"),
                        correlation,
                    )
                },
            )?,
        )),
        None => None,
    };
    // Inviting into the tenant is tenant administration; inviting into a workspace only
    // (as a plain member) is workspace administration.
    let action = if role == TenantRole::Member && workspace.is_some() {
        ActionFamily::ManageWorkspace
    } else {
        ActionFamily::ManageTenant
    };
    Authorization::check(
        state.pool(),
        scope,
        workspace.as_ref().map(|(id, _)| id.as_str()),
        action,
        correlation,
    )
    .await?;
    let member = state
        .auth()
        .identity()
        .invite_member(
            &scope.tenant_id,
            &member_admin(scope, identity, user_id),
            Invitation {
                email: email.to_string(),
                role,
                workspace,
            },
        )
        .await
        .map_err(|error| identity_error(error, correlation))?;
    Ok(member.to_json())
}

/// `UpdateMembership {membership_id, role?, status?}`.
pub async fn update_membership(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user_id = acting_user(scope, "UpdateMembership", correlation)?;
    let membership_id = text(params, "membership_id").ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "membership_id is required",
            correlation,
        )
    })?;
    let (workspace_id, action) = membership_scope(state, scope, membership_id, correlation).await?;
    Authorization::check(
        state.pool(),
        scope,
        workspace_id.as_deref(),
        action,
        correlation,
    )
    .await?;
    let member = state
        .auth()
        .identity()
        .update_membership(
            &scope.tenant_id,
            &member_admin(scope, identity, user_id),
            membership_id,
            text(params, "role"),
            text(params, "status"),
        )
        .await
        .map_err(|error| identity_error(error, correlation))?;
    Ok(member.to_json())
}

/// `RemoveMember {membership_id}`.
pub async fn remove_member(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user_id = acting_user(scope, "RemoveMember", correlation)?;
    let membership_id = text(params, "membership_id").ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "membership_id is required",
            correlation,
        )
    })?;
    let (workspace_id, action) = membership_scope(state, scope, membership_id, correlation).await?;
    Authorization::check(
        state.pool(),
        scope,
        workspace_id.as_deref(),
        action,
        correlation,
    )
    .await?;
    let member = state
        .auth()
        .identity()
        .remove_member(
            &scope.tenant_id,
            &member_admin(scope, identity, user_id),
            membership_id,
        )
        .await
        .map_err(|error| identity_error(error, correlation))?;
    Ok(member.to_json())
}

/// A `tm_` membership is tenant administration; a `wm_` one is administration of its workspace.
async fn membership_scope(
    state: &ApiState,
    scope: &TenantScope,
    membership_id: &str,
    correlation: &str,
) -> Result<(Option<String>, ActionFamily), ApiError> {
    if membership_id.starts_with("tm_") {
        return Ok((None, ActionFamily::ManageTenant));
    }
    if !membership_id.starts_with("wm_") {
        return Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            "membership_id must be a tm_ or wm_ id",
            correlation,
        ));
    }
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let workspace_id: Option<String> = sqlx::query_scalar(
        "SELECT workspace_id FROM workspace_memberships WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(membership_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let workspace_id = workspace_id.ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::NotFound,
            format!("membership {membership_id} not found"),
            correlation,
        )
    })?;
    Ok((Some(workspace_id), ActionFamily::ManageWorkspace))
}

/// `SetPolicy {scope, workspace_id?, rules, question_default_ttl_seconds?, approval_default_ttl_seconds?, max_plan_nodes?}`.
pub async fn set_policy(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let policy_scope =
        PolicyScope::parse(text(params, "scope").unwrap_or("workspace")).map_err(|value| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("{value:?} is not a policy scope"),
                correlation,
            )
        })?;
    let workspace_id = text(params, "workspace_id").map(str::to_string);
    let rules = params.get("rules").cloned().unwrap_or_else(|| json!([]));
    if !rules.is_array() {
        return Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            "rules must be an array of policy rules",
            correlation,
        ));
    }
    let action = match policy_scope {
        PolicyScope::Tenant => ActionFamily::ManageTenant,
        PolicyScope::Workspace => ActionFamily::ManageWorkspace,
    };
    Authorization::check(
        state.pool(),
        scope,
        workspace_id.as_deref(),
        action,
        correlation,
    )
    .await?;
    let mut acting = identity.clone();
    if let Some(user_id) = user_id {
        acting.actor = quansio_events::Actor::user(user_id);
    }
    let store = PolicyStore::new(state.pool().clone(), acting)
        .map_err(|error| policy_error(error, correlation))?;
    let integer =
        |key: &str, default: i64| params.get(key).and_then(Value::as_i64).unwrap_or(default);
    let policy = store
        .set_policy(PolicyChange {
            scope: policy_scope,
            workspace_id,
            rules,
            question_default_ttl_seconds: integer("question_default_ttl_seconds", 86_400),
            approval_default_ttl_seconds: integer("approval_default_ttl_seconds", 3_600),
            max_plan_nodes: i32::try_from(integer("max_plan_nodes", 25)).unwrap_or(i32::MAX),
        })
        .await
        .map_err(|error| policy_error(error, correlation))?;
    Ok(policy_json(&policy))
}
