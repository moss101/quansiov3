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
use crate::control::conversation::ConversationStore;
use crate::effects::EffectLedger;
use crate::runtime::state_machine::{RuntimeIdentity, RuntimeStore};

/// GET /v1/threads/{id}
pub async fn thread(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::from_headers(&headers)?;
    let identity = identity(&scope);
    let store = ConversationStore::new(state.pool().clone(), identity)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection"))?;
    let thread = store.get_thread(&id).await.map_err(conversation_error)?;
    Ok(Json(json!({
        "id": thread.id,
        "workspace_id": thread.workspace_id,
        "kind": thread.kind,
        "title": thread.title,
    })))
}

/// Query string for `GET /v1/messages`.
#[derive(Debug, Deserialize)]
pub struct MessageQuery {
    /// Thread to list.
    pub thread_id: Option<String>,
}

/// GET /v1/messages
pub async fn messages(
    State(state): State<ApiState>,
    Query(query): Query<MessageQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::from_headers(&headers)?;
    let thread_id = query.thread_id.ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "messages requires thread_id",
            "projection",
        )
    })?;
    let identity = identity(&scope);
    let store = ConversationStore::new(state.pool().clone(), identity)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "projection"))?;
    let messages = store
        .list_messages(&thread_id)
        .await
        .map_err(conversation_error)?;
    Ok(Json(
        json!({ "messages": messages.iter().map(|message| json!({
        "id": message.id,
        "thread_id": message.thread_id,
        "seq": message.seq,
        "author_id": message.author_id,
        "content_blocks": message.content_blocks,
    })).collect::<Vec<_>>() }),
    ))
}

/// GET /v1/runs/{id}
pub async fn run(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::from_headers(&headers)?;
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
    Ok(Json(json!({
        "id": run.id.to_string(),
        "workspace_id": run.workspace_id,
        "status": run.status.as_db_str(),
        "generation": run.generation.get(),
    })))
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
    let scope = TenantScope::from_headers(&headers)?;
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

fn conversation_error(error: crate::control::conversation::ConversationError) -> ApiError {
    let code = match error.code() {
        "VALIDATION_BOUNDS" => ApiErrorCode::ValidationBounds,
        _ => ApiErrorCode::Internal,
    };
    let mapped = if matches!(
        error,
        crate::control::conversation::ConversationError::NotFound(_)
    ) {
        ApiErrorCode::NotFound
    } else {
        code
    };
    ApiError::new(mapped, error.to_string(), "projection")
}
