//! Knowledge, memory and skills (APP-011): visibility and control over reusable
//! intelligence.
//!
//! Projections over the Knowledge Fabric's rows (`knowledge_entries`, INT-006), semantic
//! memory (`memory_entries`, INT-007) and the Skill registry (`skills`/`skill_versions`,
//! INT-009), plus the §14 commands `DeleteKnowledge`, `SupersedeKnowledge`, `DeleteMemory`
//! (through the data-lifecycle control, which tombstones the row, purges its derived index
//! rows and stages the lifecycle event in one transaction) and `InstallSkill`,
//! `PromoteSkillVersion`, `DisableSkill` (through `SkillStore`, whose §11.5 ladder decides:
//! an unapproved version cannot become ACTIVE, and skills never grant anything).
//!
//! Memory is not recovery (DOSSIER.md D-007): [`MEMORY_SQL`] names only `memory_entries`,
//! and a test pins that, so no checkpoint, cursor or protocol field can reach this page.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_core::{CanonicalId, Prefix};
use quansio_events::Actor;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::data_lifecycle::{DataAdmin, DataLifecycle, DataLifecycleError, Tombstone};
use crate::control::schema;
use crate::control::skills::{
    NewSkill, NewSkillVersion, SkillAdmin, SkillControlError, SkillScope, SkillStatus, SkillStore,
};
use crate::runtime::state_machine::RuntimeIdentity;

/// Rows one projection returns at most.
pub const MAX_ROWS: i64 = 500;

/// The knowledge projection's statement.
pub const KNOWLEDGE_SQL: &str =
    "SELECT id, workspace_id, scope, kind, content_ref, content, provenance, confidence, version, \
            status, superseded_by, created_at, updated_at \
     FROM knowledge_entries \
     WHERE tenant_id = $1 AND ($2::text IS NULL OR workspace_id = $2 OR workspace_id IS NULL) \
       AND ($3::text IS NULL OR status = $3) AND ($4::boolean OR status <> 'deleted') \
     ORDER BY updated_at DESC, id LIMIT $5";

/// The memory projection's statement: only `memory_entries`, and `retrievable` is the
/// plane's own rule (ACTIVE and not past `expires_at`, judged by the database clock).
pub const MEMORY_SQL: &str = "SELECT id, workspace_id, scope, subject_ref, content, provenance_kind, provenance_ref, confidence, \
            status, last_used_at, expires_at, created_at, updated_at, \
            (status = 'active' AND (expires_at IS NULL OR expires_at > now())) AS retrievable \
     FROM memory_entries \
     WHERE tenant_id = $1 AND ($2::text IS NULL OR workspace_id = $2 OR workspace_id IS NULL) \
       AND ($3::text IS NULL OR scope = $3) AND ($4::text IS NULL OR subject_ref = $4) \
       AND ($5::boolean OR status <> 'deleted') \
     ORDER BY updated_at DESC, id LIMIT $6";

fn projection_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection")
}

/// `?workspace_id=&status=&include_deleted=` for knowledge; memory adds `scope`, `subject_ref`.
#[derive(Debug, Default, Deserialize)]
pub struct EntryQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    subject_ref: Option<String>,
    #[serde(default)]
    include_deleted: bool,
}

/// `GET /v1/knowledge`: every entry with its provenance, scope, confidence, version,
/// lifecycle state and successor. Deleted entries are tombstones and hidden unless asked for.
pub async fn knowledge(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<EntryQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(KNOWLEDGE_SQL)
        .bind(&scope.tenant_id)
        .bind(&query.workspace_id)
        .bind(&query.status)
        .bind(query.include_deleted)
        .bind(MAX_ROWS)
        .fetch_all(&mut *tx)
        .await
        .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({ "knowledge": rows.iter().map(|row| {
        let status = row.get::<String, _>("status");
        json!({
            "id": row.get::<String, _>("id"),
            "workspace_id": row.get::<Option<String>, _>("workspace_id"),
            "scope": row.get::<String, _>("scope"),
            "kind": row.get::<String, _>("kind"),
            "content_ref": row.get::<Option<String>, _>("content_ref"),
            "content": row.get::<Option<Value>, _>("content"),
            "provenance": row.get::<Value, _>("provenance"),
            "confidence": row.get::<f64, _>("confidence"),
            "version": row.get::<i32, _>("version"),
            // Only ACTIVE knowledge answers retrieval (DOMAIN.md §11.4).
            "retrievable": status == "active",
            "status": status,
            "superseded_by": row.get::<Option<String>, _>("superseded_by"),
            "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        })
    }).collect::<Vec<_>>() })))
}

/// `GET /v1/memory`: semantic memory with its provenance and scope — and nothing from the
/// recovery layers.
pub async fn memory(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<EntryQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let rows = sqlx::query(MEMORY_SQL)
        .bind(&scope.tenant_id)
        .bind(&query.workspace_id)
        .bind(&query.scope)
        .bind(&query.subject_ref)
        .bind(query.include_deleted)
        .bind(MAX_ROWS)
        .fetch_all(&mut *tx)
        .await
        .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    Ok(Json(json!({ "memory": rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"),
        "workspace_id": row.get::<Option<String>, _>("workspace_id"),
        "scope": row.get::<String, _>("scope"),
        "subject_ref": row.get::<String, _>("subject_ref"),
        "content": row.get::<String, _>("content"),
        "provenance": { "kind": row.get::<String, _>("provenance_kind"), "ref": row.get::<Option<String>, _>("provenance_ref") },
        "confidence": row.get::<f64, _>("confidence"),
        "status": row.get::<String, _>("status"),
        "retrievable": row.get::<bool, _>("retrievable"),
        "last_used_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_used_at"),
        "expires_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("expires_at"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
    })).collect::<Vec<_>>() })))
}

/// `GET /v1/skills`: every skill with every version, its ladder position, whether it
/// resolves into production context, and the edges the ladder allows from it.
pub async fn skills(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let mut tx = state.pool().begin().await.map_err(projection_error)?;
    schema::set_tenant_context(&mut tx, &scope.tenant_id)
        .await
        .map_err(projection_error)?;
    let skill_rows = sqlx::query(
        "SELECT id, scope, name, owner, current_active_version_id, created_at, updated_at \
         FROM skills WHERE tenant_id = $1 ORDER BY name, id",
    )
    .bind(&scope.tenant_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    let version_rows = sqlx::query(
        "SELECT id, skill_id, semver, manifest, provenance, status, created_at, updated_at \
         FROM skill_versions WHERE tenant_id = $1 ORDER BY skill_id, created_at, id",
    )
    .bind(&scope.tenant_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(projection_error)?;
    tx.commit().await.map_err(projection_error)?;
    let skills = skill_rows.iter().map(|skill| {
        let skill_id = skill.get::<String, _>("id");
        let active = skill.get::<Option<String>, _>("current_active_version_id");
        let versions = version_rows
            .iter()
            .filter(|version| version.get::<String, _>("skill_id") == skill_id)
            .map(|version| {
                let id = version.get::<String, _>("id");
                let status = version.get::<String, _>("status");
                let ladder = SkillStatus::parse(&status).ok();
                json!({
                    "id": id,
                    "semver": version.get::<String, _>("semver"),
                    "manifest": version.get::<Value, _>("manifest"),
                    "provenance": version.get::<Option<String>, _>("provenance"),
                    // A version resolves only when its own state is ACTIVE and the skill's
                    // pointer names it (INT-009's rule, restated for the page).
                    "resolves": ladder.is_some_and(SkillStatus::resolves) && active.as_deref() == Some(id.as_str()),
                    "promotable_to": ladder.map(|state| state.next_states().iter().map(|next| next.as_str()).collect::<Vec<_>>()).unwrap_or_default(),
                    "status": status,
                    "created_at": version.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                    "updated_at": version.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "id": skill_id,
            "scope": skill.get::<String, _>("scope"),
            "name": skill.get::<String, _>("name"),
            "owner": skill.get::<String, _>("owner"),
            "current_active_version_id": active,
            "versions": versions,
            "created_at": skill.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            "updated_at": skill.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        })
    }).collect::<Vec<_>>();
    Ok(Json(json!({ "skills": skills })))
}

// ------------------------------------------------------------------ commands

fn actor(identity: &RuntimeIdentity, user_id: Option<&str>) -> Actor {
    user_id.map_or_else(|| identity.actor.clone(), Actor::user)
}

fn required_str<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("{key} is required"),
                correlation,
            )
        })
}

fn lifecycle_error(error: DataLifecycleError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn skill_error(error: SkillControlError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "RUNTIME_ILLEGAL_TRANSITION" => ApiErrorCode::RuntimeIllegalTransition,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn tombstone_json(key: &str, tombstone: &Tombstone) -> Value {
    json!({
        key: tombstone.id,
        "workspace_id": tombstone.workspace_id,
        "previous_status": tombstone.previous_status,
        "status": tombstone.status,
        "superseded_by": tombstone.superseded_by,
        "derived_rows_removed": tombstone.derived_rows_removed,
        "already": tombstone.already,
    })
}

/// `DeleteKnowledge {knowledge_id}`.
pub async fn delete_knowledge(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let knowledge_id = required_str(params, "knowledge_id", correlation)?;
    let admin = DataAdmin::new(
        identity.tenant_id.clone(),
        actor(identity, user_id),
        identity.correlation_id,
    );
    let tombstone = DataLifecycle::new(state.pool().clone())
        .delete_knowledge(&admin, knowledge_id)
        .await
        .map_err(|error| lifecycle_error(error, correlation))?;
    Ok(tombstone_json("knowledge_id", &tombstone))
}

/// `SupersedeKnowledge {knowledge_id, superseded_by}`.
pub async fn supersede_knowledge(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let knowledge_id = required_str(params, "knowledge_id", correlation)?;
    let superseded_by = required_str(params, "superseded_by", correlation)?;
    let admin = DataAdmin::new(
        identity.tenant_id.clone(),
        actor(identity, user_id),
        identity.correlation_id,
    );
    let tombstone = DataLifecycle::new(state.pool().clone())
        .supersede_knowledge(&admin, knowledge_id, superseded_by)
        .await
        .map_err(|error| lifecycle_error(error, correlation))?;
    Ok(tombstone_json("knowledge_id", &tombstone))
}

/// `DeleteMemory {memory_id}`.
pub async fn delete_memory(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let memory_id = required_str(params, "memory_id", correlation)?;
    let admin = DataAdmin::new(
        identity.tenant_id.clone(),
        actor(identity, user_id),
        identity.correlation_id,
    );
    let tombstone = DataLifecycle::new(state.pool().clone())
        .delete_memory(&admin, memory_id)
        .await
        .map_err(|error| lifecycle_error(error, correlation))?;
    Ok(tombstone_json("memory_id", &tombstone))
}

fn skill_admin(identity: &RuntimeIdentity, user_id: Option<&str>) -> SkillAdmin {
    SkillAdmin::new(
        identity.tenant_id.clone(),
        actor(identity, user_id),
        identity.correlation_id,
    )
}

fn version_json(version: &crate::control::skills::SkillVersion) -> Value {
    json!({
        "skill_version_id": version.id.to_string(),
        "skill_id": version.skill_id.to_string(),
        "semver": version.semver,
        "status": version.status.as_str(),
        "resolves": version.resolves(),
        "promotable_to": version.status.next_states().iter().map(|next| next.as_str()).collect::<Vec<_>>(),
    })
}

/// `InstallSkill {scope, name, owner, version {semver, manifest, provenance?}}`: a skill with
/// one DRAFT version. Installing never activates: the ladder starts at its first rung.
pub async fn install_skill(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let scope =
        SkillScope::parse(required_str(params, "scope", correlation)?).map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                error.to_string(),
                correlation,
            )
        })?;
    let name = required_str(params, "name", correlation)?.to_string();
    let owner = params
        .get("owner")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| user_id.map(str::to_string))
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "owner is required",
                correlation,
            )
        })?;
    let version = params.get("version").cloned().unwrap_or(Value::Null);
    let semver = required_str(&version, "semver", correlation)?.to_string();
    let manifest = version
        .get("manifest")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !manifest.is_object() {
        return Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            "version.manifest must be an object",
            correlation,
        ));
    }
    let provenance = version
        .get("provenance")
        .and_then(Value::as_str)
        .map(str::to_string);
    let admin = skill_admin(identity, user_id);
    let store = SkillStore::new(state.pool().clone());
    let skill = store
        .create_skill(&admin, NewSkill { scope, name, owner })
        .await
        .map_err(|error| skill_error(error, correlation))?;
    let draft = store
        .create_version(
            &admin,
            &skill.id,
            NewSkillVersion {
                semver,
                manifest,
                provenance,
            },
        )
        .await
        .map_err(|error| skill_error(error, correlation))?;
    Ok(json!({
        "skill_id": skill.id.to_string(),
        "name": skill.name,
        "scope": skill.scope.as_str(),
        "owner": skill.owner,
        "version": version_json(&draft),
    }))
}

/// `PromoteSkillVersion {skill_version_id, to}`: one edge along the §11.5 ladder. A version
/// that is not APPROVED cannot become ACTIVE, and a second ACTIVE version is refused.
pub async fn promote_skill_version(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let version_id = CanonicalId::parse_typed(
        required_str(params, "skill_version_id", correlation)?,
        Prefix::SkillVersion,
    )
    .map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            error.to_string(),
            correlation,
        )
    })?;
    let to = SkillStatus::parse(required_str(params, "to", correlation)?).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            error.to_string(),
            correlation,
        )
    })?;
    let promotion = SkillStore::new(state.pool().clone())
        .promote(&skill_admin(identity, user_id), &version_id, to)
        .await
        .map_err(|error| skill_error(error, correlation))?;
    let mut result = version_json(&promotion.version);
    result["previous_active_version_id"] = json!(promotion
        .previous_active_version_id
        .map(|id| id.to_string()));
    Ok(result)
}

/// `DisableSkill {skill_id}`: the ACTIVE version becomes DEPRECATED, so the skill stops
/// resolving into production context. A skill with no active version is refused.
pub async fn disable_skill(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let skill_id = CanonicalId::parse_typed(
        required_str(params, "skill_id", correlation)?,
        Prefix::Skill,
    )
    .map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            error.to_string(),
            correlation,
        )
    })?;
    let store = SkillStore::new(state.pool().clone());
    let active = store
        .active_versions(&identity.tenant_id)
        .await
        .map_err(|error| skill_error(error, correlation))?
        .into_iter()
        .find(|(skill, _)| skill.id == skill_id)
        .map(|(_, version)| version);
    let Some(active) = active else {
        return Err(ApiError::new(
            ApiErrorCode::ConflictState,
            format!("skill {skill_id} has no active version to disable"),
            correlation,
        ));
    };
    let promotion = store
        .promote(
            &skill_admin(identity, user_id),
            &active.id,
            SkillStatus::Deprecated,
        )
        .await
        .map_err(|error| skill_error(error, correlation))?;
    let mut result = version_json(&promotion.version);
    result["previous_active_version_id"] = json!(promotion
        .previous_active_version_id
        .map(|id| id.to_string()));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_projection_reads_only_memory_entries() {
        // Memory is not recovery: the statement may name no checkpoint, protocol-state,
        // effect or run table, so nothing from a restart path can reach the page.
        let upper = MEMORY_SQL.to_uppercase();
        let tables: Vec<&str> = upper
            .split_whitespace()
            .zip(upper.split_whitespace().skip(1))
            .filter(|(word, _)| *word == "FROM" || *word == "JOIN")
            .map(|(_, table)| table)
            .collect();
        assert_eq!(tables, vec!["MEMORY_ENTRIES"]);
        for forbidden in [
            "checkpoint",
            "cursor",
            "generation",
            "lease",
            "protocol_state",
            "resume_token",
            "effect",
        ] {
            assert!(
                !MEMORY_SQL.contains(forbidden),
                "{forbidden} leaked into the memory projection"
            );
        }
    }
}
