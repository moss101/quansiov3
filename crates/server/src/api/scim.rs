//! SCIM 2.0 transport (OPS-001, DOMAIN.md §2): `/scim/v2/ServiceProviderConfig`, `/Users`,
//! `/Users/{id}`, `/Groups`, `/Groups/{id}`.
//!
//! The caller is the tenant's IdP presenting a service principal key that holds `Provisioning`.
//! Anything else — no key, a session, a revoked key, a key without the scope — is refused
//! with a SCIM error body. Responses are `application/scim+json`; the owner
//! (`control::identity::enterprise::scim`) makes every decision.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use super::{bearer_of, ApiState};
use crate::control::identity::enterprise::{
    PrincipalScope, ScimError, ScimReply, KEY_PREFIX, PROVISIONING_SCOPE,
};

const SCIM_JSON: &str = "application/scim+json";

fn respond(status: u16, body: Option<&Value>) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, SCIM_JSON);
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn reply(result: Result<ScimReply, ScimError>) -> Response {
    match result {
        Ok(reply) => respond(reply.status, reply.body.as_ref()),
        Err(error) => respond(error.status, Some(&error.body())),
    }
}

fn refusal(status: u16, detail: &str) -> ScimError {
    ScimError {
        status,
        scim_type: None,
        detail: detail.to_string(),
    }
}

fn error_response(error: &ScimError) -> Response {
    respond(error.status, Some(&error.body()))
}

/// The provisioning principal, or the SCIM refusal to answer with.
async fn provisioner(state: &ApiState, headers: &HeaderMap) -> Result<PrincipalScope, ScimError> {
    let Some(key) = bearer_of(headers).filter(|key| key.starts_with(KEY_PREFIX)) else {
        return Err(refusal(
            401,
            "SCIM needs a service principal key as the bearer token",
        ));
    };
    let principal = state
        .enterprise()
        .authenticate_principal(key)
        .await
        .map_err(|error| refusal(401, &error.to_string()))?;
    if !principal.holds(PROVISIONING_SCOPE) {
        return Err(refusal(
            403,
            "this service principal does not hold the Provisioning scope",
        ));
    }
    Ok(principal)
}

/// List query: `filter`, `startIndex`, `count`.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    filter: Option<String>,
    #[serde(default, rename = "startIndex")]
    start_index: Option<usize>,
    #[serde(default)]
    count: Option<usize>,
}

fn body_of(bytes: &[u8]) -> Result<Value, ScimError> {
    serde_json::from_slice(bytes).map_err(|_| ScimError {
        status: 400,
        scim_type: Some("invalidSyntax"),
        detail: "the body is not JSON".to_string(),
    })
}

/// `GET /scim/v2/ServiceProviderConfig`.
pub async fn service_provider_config(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Response {
    match provisioner(&state, &headers).await {
        Ok(_) => respond(
            200,
            Some(&state.enterprise().scim_service_provider_config()),
        ),
        Err(error) => error_response(&error),
    }
}

/// `GET /scim/v2/Users`.
pub async fn list_users(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    match provisioner(&state, &headers).await {
        Ok(principal) => reply(
            state
                .enterprise()
                .scim_list_users(
                    &principal,
                    query.filter.as_deref(),
                    query.start_index,
                    query.count,
                )
                .await,
        ),
        Err(error) => error_response(&error),
    }
}

/// `POST /scim/v2/Users`.
pub async fn create_user(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let principal = match provisioner(&state, &headers).await {
        Ok(principal) => principal,
        Err(error) => return error_response(&error),
    };
    match body_of(&body) {
        Ok(body) => reply(state.enterprise().scim_create_user(&principal, &body).await),
        Err(error) => error_response(&error),
    }
}

/// `GET /scim/v2/Users/{id}`.
pub async fn get_user(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match provisioner(&state, &headers).await {
        Ok(principal) => reply(state.enterprise().scim_get_user(&principal, &id).await),
        Err(error) => error_response(&error),
    }
}

/// `PUT /scim/v2/Users/{id}`.
pub async fn replace_user(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let principal = match provisioner(&state, &headers).await {
        Ok(principal) => principal,
        Err(error) => return error_response(&error),
    };
    match body_of(&body) {
        Ok(body) => reply(
            state
                .enterprise()
                .scim_replace_user(&principal, &id, &body)
                .await,
        ),
        Err(error) => error_response(&error),
    }
}

/// `PATCH /scim/v2/Users/{id}`.
pub async fn patch_user(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let principal = match provisioner(&state, &headers).await {
        Ok(principal) => principal,
        Err(error) => return error_response(&error),
    };
    match body_of(&body) {
        Ok(body) => reply(
            state
                .enterprise()
                .scim_patch_user(&principal, &id, &body)
                .await,
        ),
        Err(error) => error_response(&error),
    }
}

/// `DELETE /scim/v2/Users/{id}`.
pub async fn delete_user(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match provisioner(&state, &headers).await {
        Ok(principal) => reply(state.enterprise().scim_delete_user(&principal, &id).await),
        Err(error) => error_response(&error),
    }
}

/// `GET /scim/v2/Groups`.
pub async fn list_groups(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    match provisioner(&state, &headers).await {
        Ok(principal) => reply(
            state
                .enterprise()
                .scim_list_groups(
                    &principal,
                    query.filter.as_deref(),
                    query.start_index,
                    query.count,
                )
                .await,
        ),
        Err(error) => error_response(&error),
    }
}

/// `POST /scim/v2/Groups`.
pub async fn create_group(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let principal = match provisioner(&state, &headers).await {
        Ok(principal) => principal,
        Err(error) => return error_response(&error),
    };
    match body_of(&body) {
        Ok(body) => reply(
            state
                .enterprise()
                .scim_create_group(&principal, &body)
                .await,
        ),
        Err(error) => error_response(&error),
    }
}

/// `GET /scim/v2/Groups/{id}`.
pub async fn get_group(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match provisioner(&state, &headers).await {
        Ok(principal) => reply(state.enterprise().scim_get_group(&principal, &id).await),
        Err(error) => error_response(&error),
    }
}

/// `PUT /scim/v2/Groups/{id}`.
pub async fn replace_group(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let principal = match provisioner(&state, &headers).await {
        Ok(principal) => principal,
        Err(error) => return error_response(&error),
    };
    match body_of(&body) {
        Ok(body) => reply(
            state
                .enterprise()
                .scim_replace_group(&principal, &id, &body)
                .await,
        ),
        Err(error) => error_response(&error),
    }
}

/// `PATCH /scim/v2/Groups/{id}`.
pub async fn patch_group(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let principal = match provisioner(&state, &headers).await {
        Ok(principal) => principal,
        Err(error) => return error_response(&error),
    };
    match body_of(&body) {
        Ok(body) => reply(
            state
                .enterprise()
                .scim_patch_group(&principal, &id, &body)
                .await,
        ),
        Err(error) => error_response(&error),
    }
}

/// `DELETE /scim/v2/Groups/{id}`.
pub async fn delete_group(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match provisioner(&state, &headers).await {
        Ok(principal) => reply(state.enterprise().scim_delete_group(&principal, &id).await),
        Err(error) => error_response(&error),
    }
}
