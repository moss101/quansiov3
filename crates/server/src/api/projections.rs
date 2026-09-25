//! Read projections (APP-001, DOMAIN.md §10 / §14).
//!
//! Each GET asks the canonical owner. The API does not query foreign tables for
//! mutation; these handlers only read through the owner's store.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::effects::EffectLedger;
use crate::runtime::state_machine::{RuntimeIdentity, RuntimeStore};

/// GET /v1/runs/{id}
pub async fn run(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let run_id = CanonicalId::parse_typed(&id, Prefix::Run).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationBounds,
            error.to_string(),
            "projection",
        )
    })?;
    let identity = identity(&scope);
    let store = RuntimeStore::new(state.pool().clone(), identity)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection"))?;
    let run = store
        .load_run(&run_id)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::NotFound, error.to_string(), "projection"))?;
    let capability_projection = capability_projection(&state, &scope.tenant_id, &run_id).await?;
    Ok(Json(json!({
        "id": run.id.to_string(),
        "workspace_id": run.workspace_id,
        "status": run.status.as_db_str(),
        "generation": run.generation.get(),
        "capability_projection": capability_projection,
    })))
}

/// The run's task-scoped authority (APP-011): the latest capability projection computed
/// for it, with its inputs in the fixed layer order and the grants that resulted. The page
/// shows *why* a run may do what it may; nothing here can widen it.
async fn capability_projection(
    state: &ApiState,
    tenant_id: &str,
    run_id: &CanonicalId,
) -> Result<Value, ApiError> {
    let unreadable =
        |error: sqlx::Error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection");
    let mut tx = state.tenant_tx(tenant_id, "projection").await?;
    let row = sqlx::query(
        "SELECT id, inputs, grants, inputs_digest, computed_at, expires_at \
         FROM capability_projections \
         WHERE tenant_id = $1 AND subject_kind = 'run' AND subject_id = $2 \
         ORDER BY computed_at DESC, id DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(run_id.to_string())
    .fetch_optional(&mut *tx)
    .await
    .map_err(unreadable)?;
    tx.commit().await.map_err(unreadable)?;
    Ok(row.map_or(Value::Null, |row| {
        use sqlx::Row;
        json!({
            "id": row.get::<String, _>("id"),
            "subject": { "kind": "run", "id": run_id.to_string() },
            "inputs": row.get::<Value, _>("inputs"),
            "grants": row.get::<Value, _>("grants"),
            "inputs_digest": row.get::<String, _>("inputs_digest"),
            "computed_at": row.get::<chrono::DateTime<chrono::Utc>, _>("computed_at"),
            "expires_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("expires_at"),
        })
    }))
}

/// Query string for `GET /v1/effects`.
#[derive(Debug, Deserialize)]
pub struct EffectQuery {
    /// Run whose effects to list.
    pub run_id: Option<String>,
}

/// GET /v1/effects
pub async fn effects(
    State(state): State<ApiState>,
    Query(query): Query<EffectQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let run_id = query.run_id.ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "effects requires run_id",
            "projection",
        )
    })?;
    let identity = identity(&scope);
    let ledger = EffectLedger::new(state.pool().clone(), identity)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection"))?;
    let records = ledger
        .list_for_run(&run_id, None)
        .await
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection"))?;
    Ok(Json(json!({
        "effects": records.iter().map(|record| json!({
            "id": record.id,
            "status": record.status.as_str(),
            "effect_class": record.effect_class.as_str(),
        })).collect::<Vec<_>>()
    })))
}

fn identity(scope: &TenantScope) -> RuntimeIdentity {
    RuntimeIdentity::system(
        &scope.tenant_id,
        "api",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}
