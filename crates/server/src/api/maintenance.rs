//! The maintenance status read projection (ADP-004, DOMAIN.md §13.6 A).
//!
//! `GET /v1/maintenance[?workspace_id=]` answers one question for a client that is about to restart
//! itself: is anything running that the restart would interrupt? It is a fence in the reporting sense
//! only. Nothing here blocks, drains or pauses anything: each count is read through the module that owns
//! the rows (the runtime store for Turns, the Effect Ledger for effects, the machine's terminal store
//! for terminal sessions), and the client decides what to do with the answer. The projection holds no
//! state of its own (D-019: no new store).

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_machine::control::terminal::TerminalStore;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::effects::EffectLedger;
use crate::runtime::state_machine::{RuntimeIdentity, RuntimeStore};

/// Query string for `GET /v1/maintenance`.
#[derive(Debug, Deserialize)]
pub struct MaintenanceQuery {
    /// Narrow the counts to one workspace; absent, the whole tenant is counted.
    pub workspace_id: Option<String>,
}

fn unreadable(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "maintenance")
}

/// `GET /v1/maintenance`: `{idle, active_turns, inflight_actions, open_terminals, workspace_id, as_of}`.
///
/// `idle` is true when no Turn is executing and no effect is in flight. Open terminals are reported but
/// do not make the status busy: a terminal session may stay open for days, and the status would then
/// never be idle (DOMAIN.md §13.6 A).
///
/// # Errors
/// [`ApiErrorCode::ValidationBounds`] for a `workspace_id` that is not a `ws_` id; the usual scope
/// refusals; [`ApiErrorCode::Internal`] when a count cannot be read.
pub async fn status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<MaintenanceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    if let Some(workspace) = query.workspace_id.as_deref() {
        if !workspace.starts_with("ws_") || workspace.len() > 64 {
            return Err(ApiError::new(
                ApiErrorCode::ValidationBounds,
                "workspace_id must be a ws_ id",
                "maintenance",
            ));
        }
    }
    let workspace = query.workspace_id.as_deref();
    let identity = RuntimeIdentity::system(
        &scope.tenant_id,
        "api",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let active_turns = RuntimeStore::new(state.pool().clone(), identity.clone())
        .map_err(unreadable)?
        .count_active_turns(workspace)
        .await
        .map_err(unreadable)?;
    let inflight_actions = EffectLedger::new(state.pool().clone(), identity)
        .map_err(unreadable)?
        .count_in_flight(workspace)
        .await
        .map_err(unreadable)?;
    let mut tx = state.tenant_tx(&scope.tenant_id, "maintenance").await?;
    let open_terminals = TerminalStore::count_open(&mut tx, &scope.tenant_id, workspace)
        .await
        .map_err(unreadable)?;
    tx.commit().await.map_err(unreadable)?;
    Ok(Json(json!({
        "idle": active_turns == 0 && inflight_actions == 0,
        "active_turns": active_turns,
        "inflight_actions": inflight_actions,
        "open_terminals": open_terminals,
        "workspace_id": query.workspace_id,
        "as_of": Utc::now().to_rfc3339(),
    })))
}
