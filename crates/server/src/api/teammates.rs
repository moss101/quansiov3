//! Teammate roster and definition commands (APP-014).
//!
//! `GET /v1/teammates?workspace_id=` is the roster: every definition with its AgentThread
//! (status, generation, suspension reason), its active run, its attention state (open
//! questions and pending approvals of its runs) and its effective capability — the
//! workspace's policy grants narrowed by the definition's needs, computed the way the next
//! run's projection will be. `CreateTeammate`, `UpdateTeammate` and `ArchiveTeammate` are
//! workspace administration (`manage_workspace`) and go through the definition owner,
//! which commits the event and the AuditEntry with the change.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::Actor;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::schema;
use crate::control::teammates::{
    narrow_by_needs, templates, NewTeammate, Teammate, TeammateAdmin, TeammateChange,
    TeammateError, TeammateStore,
};
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::RuntimeIdentity;

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

fn teammate_error(error: TeammateError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "RUNTIME_ILLEGAL_TRANSITION" => ApiErrorCode::RuntimeIllegalTransition,
        "FENCED_STALE_GENERATION" => ApiErrorCode::FencedStaleGeneration,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

/// `?workspace_id=`.
#[derive(Debug, Deserialize)]
pub struct RosterQuery {
    workspace_id: String,
}

/// The policy's grants for a workspace, as `seed_projection` collects them.
pub(super) async fn policy_grants(
    state: &ApiState,
    identity: &RuntimeIdentity,
    workspace_id: &str,
) -> Vec<quansio_capability::Grant> {
    let mut grants: Vec<quansio_capability::Grant> =
        vec![quansio_capability::Grant::from_json(&json!({
            "effect_class": "read.internal",
            "resource": { "kind": "fs", "selector": "**" },
            "constraints": {},
        }))
        .expect("grant")];
    if let Ok(policies) = crate::policy::PolicyStore::new(state.pool().clone(), identity.clone()) {
        if let Ok(set) = policies.load_policy_set(workspace_id).await {
            for rule in set.in_order().iter().flat_map(|policy| policy.rules.iter()) {
                if matches!(
                    rule.decision,
                    quansio_capability::PolicyDecision::Allow
                        | quansio_capability::PolicyDecision::Ask
                ) {
                    let grant = quansio_capability::Grant::new(
                        rule.effect_class.clone(),
                        rule.resource_selector.clone(),
                        quansio_capability::Constraints::default(),
                    );
                    if !grants.contains(&grant) {
                        grants.push(grant);
                    }
                }
            }
        }
    }
    grants
}

/// `GET /v1/teammates?workspace_id=`: the roster.
pub async fn roster(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<RosterQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let identity = RuntimeIdentity::system(
        &scope.tenant_id,
        "api",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let store = TeammateStore::new(state.pool().clone());
    let teammates = store
        .list(&scope.tenant_id, &query.workspace_id)
        .await
        .map_err(projection_error)?;
    let policy = policy_grants(&state, &identity, &query.workspace_id).await;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let threads = sqlx::query(
        "SELECT a.id, a.definition_id, a.status, a.generation, a.suspended_reason, a.updated_at, \
                (SELECT jsonb_build_object('id', r.id, 'status', r.status, 'generation', r.generation) FROM runs r \
                   WHERE r.tenant_id = a.tenant_id AND r.agent_thread_id = a.id \
                     AND r.status NOT IN ('SUCCEEDED', 'FAILED', 'CANCELLED') \
                   ORDER BY r.created_at DESC LIMIT 1) AS active_run, \
                (SELECT count(*) FROM runs r WHERE r.tenant_id = a.tenant_id AND r.agent_thread_id = a.id) AS runs, \
                (SELECT count(*) FROM questions q JOIN runs r ON r.id = q.run_id AND r.tenant_id = q.tenant_id \
                   WHERE r.tenant_id = a.tenant_id AND r.agent_thread_id = a.id AND q.status = 'open') AS open_questions, \
                (SELECT count(*) FROM approval_requests p JOIN runs r ON r.id = p.run_id AND r.tenant_id = p.tenant_id \
                   WHERE r.tenant_id = a.tenant_id AND r.agent_thread_id = a.id AND p.status = 'requested') AS pending_approvals \
         FROM agent_threads a WHERE a.tenant_id = $1 AND a.workspace_id = $2 AND a.agent_kind = 'teammate' \
         ORDER BY a.created_at, a.id",
    )
    .bind(&scope.tenant_id)
    .bind(&query.workspace_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let entries = teammates
        .iter()
        .map(|teammate| {
            let thread = threads.iter().find(|row| row.get::<Option<String>, _>("definition_id").as_deref() == Some(&teammate.id.to_string()));
            let effective = narrow_by_needs(policy.clone(), &teammate.default_capability_needs);
            let mut entry = teammate.to_json();
            entry["thread"] = thread.map_or(Value::Null, |row| json!({
                "id": row.get::<String, _>("id"),
                "status": row.get::<String, _>("status"),
                "generation": row.get::<i64, _>("generation"),
                "suspended_reason": row.get::<Option<String>, _>("suspended_reason"),
                "runs": row.get::<i64, _>("runs"),
                "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
            }));
            entry["active_run"] = thread.and_then(|row| row.get::<Option<Value>, _>("active_run")).unwrap_or(Value::Null);
            let open_questions = thread.map_or(0, |row| row.get::<i64, _>("open_questions"));
            let pending_approvals = thread.map_or(0, |row| row.get::<i64, _>("pending_approvals"));
            entry["attention"] = json!({
                "open_questions": open_questions,
                "pending_approvals": pending_approvals,
                "needs_human": open_questions + pending_approvals > 0,
            });
            entry["effective_capability"] = json!({
                "grants": effective.iter().map(|grant| json!({
                    "effect_class": grant.effect_class.as_str(),
                    "resource": grant.resource,
                })).collect::<Vec<_>>(),
                "policy_grants": policy.len(),
                "narrowed_by_needs": !teammate.default_capability_needs.is_empty(),
                "needs_not_granted": teammate.default_capability_needs.iter()
                    .filter(|need| !policy.iter().any(|grant| grant.effect_class.as_str() == need.as_str()))
                    .cloned()
                    .collect::<Vec<_>>(),
            });
            entry
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "workspace_id": query.workspace_id,
        "teammates": entries,
        "templates": templates().map_err(projection_error)?.iter().map(|template| json!({
            "id": template.id,
            "name": template.name,
            "persona": template.persona,
            "standing_instructions": template.standing_instructions,
            "default_capability_needs": template.default_capability_needs,
            "memory_scope": template.memory_scope,
        })).collect::<Vec<_>>(),
    })))
}

// ------------------------------------------------------------------ commands

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn strings(params: &Value, key: &str, correlation: &str) -> Result<Option<Vec<String>>, ApiError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    ApiError::new(
                        ApiErrorCode::ValidationSchema,
                        format!("{key} must be an array of effect classes"),
                        correlation,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("{key} must be an array of effect classes"),
            correlation,
        )),
    }
}

fn admin(identity: &RuntimeIdentity, user_id: Option<&str>) -> TeammateAdmin {
    TeammateAdmin {
        tenant_id: identity.tenant_id.clone(),
        actor: user_id.map_or_else(|| identity.actor.clone(), Actor::user),
        correlation_id: identity.correlation_id,
    }
}

fn with_thread(teammate: &Teammate, thread: Option<&crate::runtime::agents::AgentThread>) -> Value {
    let mut entry = teammate.to_json();
    entry["thread"] = thread.map_or(Value::Null, |thread| {
        json!({
            "id": thread.id.to_string(),
            "status": thread.status.as_db_str(),
            "generation": thread.generation.get(),
            "suspended_reason": thread.suspended_reason,
        })
    });
    entry
}

/// `CreateTeammate {workspace_id, template_id?, name?, persona?, standing_instructions?,
/// default_capability_needs?, memory_scope?, model_route_preference?, default_execution_target_id?, availability?}`.
pub async fn create_teammate(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let workspace_id = text(params, "workspace_id").ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "workspace_id is required",
            correlation,
        )
    })?;
    Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::ManageWorkspace,
        correlation,
    )
    .await?;
    let new = NewTeammate {
        template_id: text(params, "template_id").map(str::to_string),
        name: text(params, "name").map(str::to_string),
        persona: text(params, "persona").map(str::to_string),
        standing_instructions: text(params, "standing_instructions").map(str::to_string),
        default_capability_needs: strings(params, "default_capability_needs", correlation)?,
        memory_scope: text(params, "memory_scope").map(str::to_string),
        model_route_preference: text(params, "model_route_preference").map(str::to_string),
        default_execution_target_id: text(params, "default_execution_target_id")
            .map(str::to_string),
        availability: text(params, "availability").map(str::to_string),
    };
    let (teammate, thread) = TeammateStore::new(state.pool().clone())
        .create(&admin(identity, user_id), identity, workspace_id, new)
        .await
        .map_err(|error| teammate_error(error, correlation))?;
    Ok(with_thread(&teammate, Some(&thread)))
}

/// `UpdateTeammate {teammate_id, …fields}`; `model_route_preference: null` and
/// `default_execution_target_id: null` clear the preference.
pub async fn update_teammate(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let teammate_id =
        CanonicalId::parse_typed(text(params, "teammate_id").unwrap_or(""), Prefix::Teammate)
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!("teammate_id: {error}"),
                    correlation,
                )
            })?;
    let store = TeammateStore::new(state.pool().clone());
    let current = store
        .get(&identity.tenant_id, &teammate_id)
        .await
        .map_err(|error| teammate_error(error, correlation))?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&current.workspace_id),
        ActionFamily::ManageWorkspace,
        correlation,
    )
    .await?;
    let optional = |key: &str| -> Option<Option<String>> {
        match params.get(key) {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => Some(value.as_str().map(str::to_string)),
        }
    };
    let change = TeammateChange {
        name: text(params, "name").map(str::to_string),
        persona: text(params, "persona").map(str::to_string),
        standing_instructions: text(params, "standing_instructions").map(str::to_string),
        default_capability_needs: strings(params, "default_capability_needs", correlation)?,
        memory_scope: text(params, "memory_scope").map(str::to_string),
        model_route_preference: optional("model_route_preference"),
        default_execution_target_id: optional("default_execution_target_id"),
        availability: text(params, "availability").map(str::to_string),
    };
    let updated = store
        .update(&admin(identity, user_id), &teammate_id, change)
        .await
        .map_err(|error| teammate_error(error, correlation))?;
    let thread = store
        .thread_of(identity, &updated)
        .await
        .map_err(|error| teammate_error(error, correlation))?;
    Ok(with_thread(&updated, thread.as_ref()))
}

/// `ArchiveTeammate {teammate_id}`.
pub async fn archive_teammate(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let teammate_id =
        CanonicalId::parse_typed(text(params, "teammate_id").unwrap_or(""), Prefix::Teammate)
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!("teammate_id: {error}"),
                    correlation,
                )
            })?;
    let store = TeammateStore::new(state.pool().clone());
    let current = store
        .get(&identity.tenant_id, &teammate_id)
        .await
        .map_err(|error| teammate_error(error, correlation))?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&current.workspace_id),
        ActionFamily::ManageWorkspace,
        correlation,
    )
    .await?;
    let (archived, threads) = store
        .archive(&admin(identity, user_id), identity, &teammate_id)
        .await
        .map_err(|error| teammate_error(error, correlation))?;
    let mut entry = with_thread(&archived, threads.first());
    entry["threads"] = json!(threads
        .iter()
        .map(|thread| json!({
            "id": thread.id.to_string(),
            "status": thread.status.as_db_str(),
            "suspended_reason": thread.suspended_reason,
        }))
        .collect::<Vec<_>>());
    entry["lineage_retained"] = json!(true);
    Ok(entry)
}
