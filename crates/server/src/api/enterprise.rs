//! Enterprise identity routes (OPS-001, DOMAIN.md §2, §14): SSO sign-in, a user's own sessions,
//! the `/v1/service-principals` and `/v1/sso` read projections, and the Identity-family commands
//! `CreateServicePrincipal`, `RevokeServicePrincipal`, `ConfigureSso`, `SetGroupRoleMapping`,
//! `RemoveGroupRoleMapping` and `RevokeUserSessions`.
//!
//! Every decision is the owner's (`control::identity::enterprise`). This transport authenticates
//! the caller, checks the RBAC family and translates refusals into §15 errors. Principal management,
//! SSO configuration and session revocation need a person: a tenant administrator's own session.
//! A service principal is refused them even when it holds the Identity family.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_core::{CanonicalId, Prefix};
use serde::Deserialize;
use serde_json::{json, Value};

use super::auth::{client_kind, session_json};
use super::rbac::Authorization;
use super::{bearer_of, ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::identity::auth::SessionScope;
use crate::control::identity::enterprise::{Acting, EnterpriseError, SsoSettings};
use crate::control::identity::MemberAdmin;
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::RuntimeIdentity;

/// Translate an owner refusal into the §15 error.
#[must_use]
pub fn api_error(error: EnterpriseError, correlation: &str) -> ApiError {
    let code = match &error {
        EnterpriseError::Invalid(_) => ApiErrorCode::ValidationSchema,
        EnterpriseError::NotFound(_) => ApiErrorCode::NotFound,
        EnterpriseError::Conflict(_) => ApiErrorCode::ConflictState,
        EnterpriseError::Unauthenticated(_) => ApiErrorCode::AuthInvalidToken,
        EnterpriseError::Forbidden(_) => ApiErrorCode::ScopeForbidden,
        EnterpriseError::Unavailable(_) => ApiErrorCode::ProviderUnavailable,
        other => match other.code() {
            "AUTH_INVALID_TOKEN" => ApiErrorCode::AuthInvalidToken,
            "AUTH_EXPIRED" => ApiErrorCode::AuthExpired,
            "NOT_FOUND" => ApiErrorCode::NotFound,
            "CONFLICT_STATE" => ApiErrorCode::ConflictState,
            "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
            _ => ApiErrorCode::Internal,
        },
    };
    let message = match error {
        EnterpriseError::Invalid(message)
        | EnterpriseError::NotFound(message)
        | EnterpriseError::Conflict(message)
        | EnterpriseError::Unauthenticated(message)
        | EnterpriseError::Forbidden(message)
        | EnterpriseError::Unavailable(message) => message,
        other => other.to_string(),
    };
    ApiError::new(code, message, correlation)
}

fn tenant_path(tenant_id: &str) -> Result<(), ApiError> {
    CanonicalId::parse_typed(tenant_id, Prefix::Tenant)
        .map(|_| ())
        .map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("tenant_id: {error}"),
                "auth",
            )
        })
}

/// Query of `GET /v1/auth/sso/{tenant_id}/start`.
#[derive(Debug, Deserialize)]
pub struct SsoStartQuery {
    redirect_uri: String,
}

/// `GET /v1/auth/sso/{tenant_id}/start`: where to send the user.
pub async fn sso_start(
    State(state): State<ApiState>,
    Path(tenant_id): Path<String>,
    Query(query): Query<SsoStartQuery>,
) -> Result<Json<Value>, ApiError> {
    tenant_path(&tenant_id)?;
    let start = state
        .enterprise()
        .start_sso(&tenant_id, &query.redirect_uri)
        .await
        .map_err(|error| api_error(error, "auth"))?;
    Ok(Json(json!({
        "tenant_id": tenant_id,
        "authorization_url": start.authorization_url,
        "state": start.state,
        "expires_at": start.expires_at,
    })))
}

/// Body of `POST /v1/auth/sso/{tenant_id}/complete`. Anything else a client sends (roles,
/// groups, claims) is ignored: roles come from the server-side mapping only.
#[derive(Debug, Deserialize)]
pub struct SsoCompleteRequest {
    code: String,
    state: String,
    redirect_uri: String,
}

/// `POST /v1/auth/sso/{tenant_id}/complete`. A bearer session on the request is the signed-in
/// user's consent to link an existing account.
pub async fn sso_complete(
    State(state): State<ApiState>,
    Path(tenant_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SsoCompleteRequest>,
) -> Result<Json<Value>, ApiError> {
    tenant_path(&tenant_id)?;
    let linking = match bearer_of(&headers) {
        Some(bearer) => Some(state.auth().authenticate(bearer).await.map_err(|error| {
            ApiError::new(ApiErrorCode::AuthInvalidToken, error.to_string(), "auth")
        })?),
        None => None,
    };
    let (session, user) = state
        .enterprise()
        .complete_sso(
            &tenant_id,
            &request.code,
            &request.state,
            &request.redirect_uri,
            client_kind(&headers),
            linking.as_ref(),
        )
        .await
        .map_err(|error| api_error(error, "auth"))?;
    Ok(Json(session_json(&session, Some(&user))))
}

async fn own_session(state: &ApiState, headers: &HeaderMap) -> Result<SessionScope, ApiError> {
    let scope = TenantScope::resolve(state, headers).await?;
    match (scope.user_id, scope.session_id) {
        (Some(user_id), Some(session_id)) => Ok(SessionScope {
            user_id,
            tenant_id: scope.tenant_id,
            session_id,
        }),
        _ => Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "this needs a bearer session",
            "auth",
        )),
    }
}

/// `GET /v1/auth/sessions` (bearer): the caller's live sessions in the tenant.
pub async fn sessions(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = own_session(&state, &headers).await?;
    let sessions = state
        .enterprise()
        .own_sessions(&scope)
        .await
        .map_err(|error| api_error(error, "auth"))?;
    Ok(Json(json!({ "sessions": sessions })))
}

/// `POST /v1/auth/sessions/{session_id}/revoke` (bearer): end one of the caller's sessions.
pub async fn revoke_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = own_session(&state, &headers).await?;
    let revoked = state
        .enterprise()
        .revoke_own_session(&scope, &session_id)
        .await
        .map_err(|error| api_error(error, "auth"))?;
    Ok(Json(revoked))
}

async fn tenant_admin(
    state: &ApiState,
    headers: &HeaderMap,
    what: &str,
) -> Result<TenantScope, ApiError> {
    let scope = TenantScope::resolve(state, headers).await?;
    Authorization::check(state.pool(), &scope, None, ActionFamily::ManageTenant, what).await?;
    Ok(scope)
}

/// `GET /v1/service-principals` (tenant administrators): never a key or its hash.
pub async fn service_principals(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = tenant_admin(&state, &headers, "service-principals").await?;
    let principals = state
        .enterprise()
        .service_principals(&scope.tenant_id)
        .await
        .map_err(|error| api_error(error, "service-principals"))?;
    Ok(Json(json!({ "service_principals": principals })))
}

/// `GET /v1/sso` (tenant administrators): the connection and its group mappings.
pub async fn sso(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = tenant_admin(&state, &headers, "sso").await?;
    let connection = state
        .enterprise()
        .sso_connection(&scope.tenant_id)
        .await
        .map_err(|error| api_error(error, "sso"))?;
    Ok(Json(json!({ "sso": connection })))
}

/// The acting tenant administrator for an Identity-family command that only a person may run.
async fn human_admin(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    command: &str,
    correlation: &str,
) -> Result<Acting, ApiError> {
    let user_id = match (&scope.principal, scope.user_id.as_deref()) {
        (None, Some(user_id)) => user_id,
        (Some(_), _) => {
            return Err(ApiError::new(
                ApiErrorCode::ScopeForbidden,
                format!("{command} needs a person: a service principal cannot run it"),
                correlation,
            ))
        }
        (None, None) => {
            return Err(ApiError::new(
                ApiErrorCode::AuthRequired,
                format!("{command} needs an acting user"),
                correlation,
            ))
        }
    };
    Authorization::check(
        state.pool(),
        scope,
        None,
        ActionFamily::ManageTenant,
        correlation,
    )
    .await?;
    Ok(Acting::from(&MemberAdmin {
        user_id: user_id.to_string(),
        actor: quansio_events::Actor::user(user_id),
        correlation_id: identity.correlation_id,
    }))
}

/// Run one of the enterprise Identity-family commands.
///
/// # Errors
/// [`ApiError`] for a caller that may not run it, malformed params, or the owner's refusal.
pub async fn command(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    command: &str,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let acting = human_admin(state, scope, identity, command, correlation).await?;
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    let required = |key: &str| {
        text(key).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                format!("{command} needs `{key}`"),
                correlation,
            )
        })
    };
    let owner = state.enterprise();
    let tenant = scope.tenant_id.as_str();
    let refused = |error| api_error(error, correlation);
    match command {
        "CreateServicePrincipal" => {
            let scopes: Vec<String> = params
                .get("scopes")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let families: Vec<&str> = state.catalog().groups.keys().map(String::as_str).collect();
            let (principal, key) = owner
                .create_service_principal(tenant, &acting, required("name")?, &scopes, &families)
                .await
                .map_err(refused)?;
            // The key is the one-time value: it is returned here and never stored in the command log.
            Ok(json!({ "service_principal": principal, super::webhooks::ONE_TIME_FIELD: key }))
        }
        "RevokeServicePrincipal" => owner
            .revoke_service_principal(tenant, &acting, required("service_principal_id")?)
            .await
            .map(|principal| json!({ "service_principal": principal }))
            .map_err(refused),
        "ConfigureSso" => {
            let settings = SsoSettings {
                issuer: required("issuer")?.to_string(),
                client_id: required("client_id")?.to_string(),
                client_secret_handle: text("client_secret_handle").map(str::to_string),
                groups_claim: text("groups_claim").unwrap_or("groups").to_string(),
                default_workspace_id: text("default_workspace_id").map(str::to_string),
                enforced: params
                    .get("enforced")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                status: text("status").unwrap_or("active").to_string(),
            };
            owner
                .configure_sso(tenant, &acting, settings)
                .await
                .map(|connection| json!({ "sso": connection }))
                .map_err(refused)
        }
        "SetGroupRoleMapping" => owner
            .set_group_role_mapping(
                tenant,
                &acting,
                required("external_group")?,
                text("workspace_id"),
                required("role")?,
            )
            .await
            .map_err(refused),
        "RemoveGroupRoleMapping" => owner
            .remove_group_role_mapping(
                tenant,
                &acting,
                required("external_group")?,
                text("workspace_id"),
            )
            .await
            .map_err(refused),
        "RevokeUserSessions" => owner
            .revoke_user_sessions(tenant, &acting, required("user_id")?)
            .await
            .map_err(refused),
        other => Err(ApiError::new(
            ApiErrorCode::NotFound,
            format!("{other} is not an enterprise identity command"),
            correlation,
        )),
    }
}

/// The §14 family a command belongs to.
#[must_use]
pub fn family_of<'a>(state: &'a ApiState, command: &str) -> Option<&'a str> {
    state
        .catalog()
        .groups
        .iter()
        .find(|(_, commands)| {
            commands
                .iter()
                .any(|name| name.eq_ignore_ascii_case(command))
        })
        .map(|(family, _)| family.as_str())
}
