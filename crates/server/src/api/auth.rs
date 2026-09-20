//! Session endpoints (APP-002, DOMAIN.md §2): magic link, OAuth, refresh, sign-out, `/v1/me`.
//!
//! These are the only public routes that resolve no tenant scope on entry: they establish
//! one. Every response body is a §15 error or the `Session`/identity shape the OpenAPI
//! document declares. Magic-link tokens leave the server only through the delivery port —
//! the console sink in development, an email channel in production (OD-009).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::identity::auth::{AuthError, IssuedSession, MagicLinkChallenge, SignedInUser};
use crate::control::identity::oauth::ReadyProvider;

/// How a magic link reaches the user. Production wires the email channel; development
/// prints to the server's stderr so a developer (or a harness) can read it.
pub trait MagicLinkDelivery: Send + Sync {
    /// Deliver `challenge` to its email; never store the token.
    fn deliver(&self, challenge: &MagicLinkChallenge);
    /// Human-readable description for `/v1/auth/magic-link/start` responses.
    fn describe(&self) -> &'static str;
}

/// Development delivery: the link is written to stderr, prefixed so it can be found.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsoleDelivery;

impl MagicLinkDelivery for ConsoleDelivery {
    fn deliver(&self, challenge: &MagicLinkChallenge) {
        eprintln!(
            "quansio-server: magic-link for {} (expires {}): token={}",
            challenge.email, challenge.expires_at, challenge.token
        );
    }

    fn describe(&self) -> &'static str {
        "console"
    }
}

/// Test delivery: keeps every challenge so a test can complete it.
#[derive(Debug, Default)]
pub struct RecordingDelivery {
    challenges: std::sync::Mutex<Vec<MagicLinkChallenge>>,
}

impl RecordingDelivery {
    /// The challenges delivered so far, oldest first.
    #[must_use]
    pub fn delivered(&self) -> Vec<MagicLinkChallenge> {
        self.challenges
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }
}

impl MagicLinkDelivery for RecordingDelivery {
    fn deliver(&self, challenge: &MagicLinkChallenge) {
        if let Ok(mut challenges) = self.challenges.lock() {
            challenges.push(challenge.clone());
        }
    }

    fn describe(&self) -> &'static str {
        "recording"
    }
}

fn auth_error(error: AuthError, correlation: &str) -> ApiError {
    match error {
        AuthError::InvalidToken(message) => {
            ApiError::new(ApiErrorCode::AuthInvalidToken, message, correlation)
        }
        AuthError::Expired(message) => {
            ApiError::new(ApiErrorCode::AuthExpired, message, correlation)
        }
        AuthError::SigningKey(message) => {
            ApiError::new(ApiErrorCode::Internal, message, correlation)
        }
        AuthError::Identity(error) => ApiError::new(
            ApiErrorCode::ValidationBounds,
            error.to_string(),
            correlation,
        ),
        AuthError::Database(error) => ApiError::new(
            ApiErrorCode::Internal,
            format!("database: {error}"),
            correlation,
        ),
    }
}

fn session_json(session: &IssuedSession, user: Option<&SignedInUser>) -> Value {
    let mut value = json!({
        "session_id": session.session_id,
        "user_id": session.user_id,
        "tenant_id": session.tenant_id,
        "access_token": session.access_token,
        "access_expires_at": session.access_expires_at,
        "refresh_token": session.refresh_token,
        "refresh_expires_at": session.refresh_expires_at,
    });
    if let Some(user) = user {
        value["workspace_id"] = json!(user.workspace_id);
        value["onboarded"] = json!(user.onboarded);
    }
    value
}

fn client_kind(headers: &HeaderMap) -> &'static str {
    match headers
        .get("x-quansio-client")
        .and_then(|value| value.to_str().ok())
    {
        Some("web") => "web",
        Some("cli") => "cli",
        _ => "desktop",
    }
}

/// Body of `POST /v1/auth/magic-link/start`.
#[derive(Debug, Deserialize)]
pub struct MagicLinkStartRequest {
    email: String,
    #[serde(default)]
    display_name: String,
}

/// `POST /v1/auth/magic-link/start`.
pub async fn magic_link_start(
    State(state): State<ApiState>,
    Json(request): Json<MagicLinkStartRequest>,
) -> Result<Json<Value>, ApiError> {
    let challenge = state
        .auth()
        .start_magic_link(&request.email, &request.display_name)
        .await
        .map_err(|error| auth_error(error, "auth"))?;
    state.magic_link_delivery().deliver(&challenge);
    Ok(Json(json!({
        "challenge_id": challenge.challenge_id,
        "email": challenge.email,
        "expires_at": challenge.expires_at,
        "delivery": state.magic_link_delivery().describe(),
    })))
}

/// Body of `POST /v1/auth/magic-link/complete`.
#[derive(Debug, Deserialize)]
pub struct MagicLinkCompleteRequest {
    token: String,
}

/// `POST /v1/auth/magic-link/complete`.
pub async fn magic_link_complete(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<MagicLinkCompleteRequest>,
) -> Result<Json<Value>, ApiError> {
    let (session, user) = state
        .auth()
        .complete_magic_link(&request.token, client_kind(&headers))
        .await
        .map_err(|error| auth_error(error, "auth"))?;
    Ok(Json(session_json(&session, Some(&user))))
}

fn ready_provider(state: &ApiState, name: &str) -> Result<ReadyProvider, ApiError> {
    state
        .oauth_providers()
        .into_iter()
        .find(|provider| provider.name == name)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ProviderUnavailable,
                format!("OAuth provider {name:?} is not configured on this deployment"),
                "auth",
            )
        })
}

/// Query of `GET /v1/auth/oauth/{provider}/start`.
#[derive(Debug, Deserialize)]
pub struct OAuthStartQuery {
    redirect_uri: String,
}

/// `GET /v1/auth/oauth/{provider}/start`.
pub async fn oauth_start(
    State(state): State<ApiState>,
    Path(provider): Path<String>,
    Query(query): Query<OAuthStartQuery>,
) -> Result<Json<Value>, ApiError> {
    let provider = ready_provider(&state, &provider)?;
    let start = state
        .auth()
        .start_oauth(&provider, &query.redirect_uri)
        .await
        .map_err(|error| auth_error(error, "auth"))?;
    Ok(Json(json!({
        "provider": provider.name,
        "authorization_url": start.authorization_url,
        "state": start.state,
        "expires_at": start.expires_at,
    })))
}

/// Body of `POST /v1/auth/oauth/{provider}/complete`.
#[derive(Debug, Deserialize)]
pub struct OAuthCompleteRequest {
    code: String,
    state: String,
    redirect_uri: String,
}

/// `POST /v1/auth/oauth/{provider}/complete`.
pub async fn oauth_complete(
    State(state): State<ApiState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OAuthCompleteRequest>,
) -> Result<Json<Value>, ApiError> {
    let provider = ready_provider(&state, &provider)?;
    let (session, user) = state
        .auth()
        .complete_oauth(
            &provider,
            state.oauth_exchange().as_ref(),
            &request.code,
            &request.state,
            &request.redirect_uri,
            client_kind(&headers),
        )
        .await
        .map_err(|error| auth_error(error, "auth"))?;
    Ok(Json(session_json(&session, Some(&user))))
}

/// Body of `POST /v1/auth/refresh`.
#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    refresh_token: String,
}

/// `POST /v1/auth/refresh`.
pub async fn refresh(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<RefreshRequest>,
) -> Result<Json<Value>, ApiError> {
    let session = state
        .auth()
        .refresh(&request.refresh_token, client_kind(&headers))
        .await
        .map_err(|error| auth_error(error, "auth"))?;
    Ok(Json(session_json(&session, None)))
}

/// `POST /v1/auth/sign-out` (bearer).
pub async fn sign_out(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let Some(session_id) = scope.session_id.as_deref() else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "sign-out needs a bearer session",
            "auth",
        ));
    };
    state
        .auth()
        .sign_out(session_id)
        .await
        .map_err(|error| auth_error(error, "auth"))?;
    Ok(Json(
        json!({ "signed_out": true, "session_id": session_id }),
    ))
}

/// `GET /v1/me` (bearer): the caller's identity, tenants, workspaces and effective settings.
pub async fn me(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    // A trusted-header scope (development harnesses only; the deployable resolves bearer
    // sessions alone) has a user but no session row: the view is the same, the session id
    // says what it is.
    let Some(user_id) = scope.user_id.clone() else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "/v1/me needs a bearer session",
            "auth",
        ));
    };
    let session_id = scope
        .session_id
        .clone()
        .unwrap_or_else(|| "trusted-headers".to_string());
    let session_scope = crate::control::identity::SessionScope {
        user_id,
        tenant_id: scope.tenant_id.clone(),
        session_id,
    };
    let mut view = state
        .auth()
        .me(&session_scope)
        .await
        .map_err(|error| auth_error(error, "auth"))?;
    view["auth_methods_offered"] = json!({
        "email_magic_link": true,
        "oauth": state
            .oauth_providers()
            .iter()
            .map(|provider| provider.name.clone())
            .collect::<Vec<_>>(),
    });
    Ok(Json(view))
}

/// Share the delivery port with the composition root.
pub type SharedDelivery = Arc<dyn MagicLinkDelivery>;
