//! Operator kill-switch routes (OPS-008, DOMAIN.md §7.6, §14 Admin/ops).
//!
//! Commands:
//! - named: `FreezeEffects`, `UnfreezeEffects`, `DisableProvider`, `EnableProvider`,
//!   `QuarantineWorker`, `DrainTarget` and `RevokeConnector`;
//! - generic, covering every switch: `EngageKillSwitch` and `ReleaseKillSwitch`;
//! - the `GET /v1/ops/switches` read projection.
//!
//! Every decision is the owner's (`control::ops`). This transport checks that the caller manages
//! the tenant, maps the command onto a switch and target, and tells the live hub when streams are
//! shed or resumed.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use quansio_events::Actor;
use serde_json::{json, Value};

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::identity::Acting;
use crate::control::ops::{KillSwitch, OpsControl, OpsError};
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::RuntimeIdentity;

fn ops_error(error: OpsError, correlation: &str) -> ApiError {
    let code = match &error {
        OpsError::Invalid(_) => ApiErrorCode::ValidationSchema,
        OpsError::NotFound(_) => ApiErrorCode::NotFound,
        OpsError::Conflict(_) => ApiErrorCode::ConflictState,
        OpsError::Event(_) | OpsError::Database(_) => ApiErrorCode::Internal,
    };
    let message = match error {
        OpsError::Invalid(message) | OpsError::NotFound(message) | OpsError::Conflict(message) => {
            message
        }
        other => other.to_string(),
    };
    ApiError::new(code, message, correlation)
}

/// `GET /v1/ops/switches`: active switches first, then recent history (tenant administrators).
pub async fn switches(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    Authorization::check(
        state.pool(),
        &scope,
        None,
        ActionFamily::ManageTenant,
        "ops",
    )
    .await?;
    let switches = OpsControl::new(state.pool().clone())
        .list(&scope.tenant_id, 100)
        .await
        .map_err(|error| ops_error(error, "ops"))?;
    Ok(Json(json!({ "switches": switches })))
}

/// Run one of the kill-switch commands.
///
/// # Errors
/// [`ApiError`] for a caller without manage_tenant, malformed params or the owner's refusal.
pub async fn command(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    command: &str,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    Authorization::check(
        state.pool(),
        scope,
        None,
        ActionFamily::ManageTenant,
        correlation,
    )
    .await?;
    let actor = match (&scope.principal, scope.user_id.as_deref()) {
        (Some(principal), _) => Actor::service(&principal.id),
        (None, Some(user_id)) => Actor::user(user_id),
        (None, None) => Actor::system("api"),
    };
    let acting = Acting {
        actor,
        correlation_id: identity.correlation_id,
    };
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim);
    let required = |key: &str| {
        text(key).filter(|value| !value.is_empty()).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("{command} needs `{key}`"),
                correlation,
            )
        })
    };
    let (engage, switch, target) = match command {
        "FreezeEffects" | "UnfreezeEffects" => (
            command == "FreezeEffects",
            KillSwitch::EffectFreeze,
            text("workspace_id").unwrap_or("*").to_string(),
        ),
        "DisableProvider" | "EnableProvider" => (
            command == "DisableProvider",
            KillSwitch::ProviderDisable,
            required("provider")?.to_string(),
        ),
        "QuarantineWorker" => (
            true,
            KillSwitch::WorkerQuarantine,
            required("target_id")?.to_string(),
        ),
        "DrainTarget" => (
            true,
            KillSwitch::TargetDrain,
            required("target_id")?.to_string(),
        ),
        "RevokeConnector" => (
            true,
            KillSwitch::ConnectorRevoke,
            required("instance_id")?.to_string(),
        ),
        "EngageKillSwitch" | "ReleaseKillSwitch" => {
            let name = required("switch")?;
            let switch = KillSwitch::parse(name).ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!(
                        "{name:?} is not a kill switch (one of {})",
                        KillSwitch::ALL.map(KillSwitch::as_str).join(", ")
                    ),
                    correlation,
                )
            })?;
            (
                command == "EngageKillSwitch",
                switch,
                required("target")?.to_string(),
            )
        }
        other => {
            return Err(ApiError::new(
                ApiErrorCode::NotFound,
                format!("{other} is not a kill-switch command"),
                correlation,
            ))
        }
    };
    let owner = OpsControl::new(state.pool().clone());
    let row = if engage {
        let reason = match command {
            "RevokeConnector" => text("reason")
                .filter(|reason| !reason.is_empty())
                .unwrap_or("connector revoked by an administrator")
                .to_string(),
            _ => required("reason")?.to_string(),
        };
        owner
            .engage(&scope.tenant_id, &acting, switch, &target, &reason)
            .await
    } else {
        owner
            .release(&scope.tenant_id, &acting, switch, &target, text("reason"))
            .await
    }
    .map_err(|error| ops_error(error, correlation))?;
    if switch == KillSwitch::StreamShed {
        if engage {
            state.live().shed(&scope.tenant_id);
        } else {
            state.live().resume(&scope.tenant_id);
        }
    }
    Ok(json!({ "switch": row }))
}
