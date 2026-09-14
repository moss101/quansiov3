//! Public API v1: authentication, tenant scope, commands and read projections (APP-001, DOMAIN.md §14, §15).
//!
//! This module is a *transport*, and the boundary it must not cross is the point of it: a handler
//! authenticates, resolves the tenant, records the command it was given, and then calls the module that
//! owns the thing being changed. It writes exactly one table of its own — `commands`, which is the command
//! log §1.2's idempotency is defined over — and no handler reaches into another owner's tables. A test
//! scans this module for statements against tables it does not own, because that is the failure this
//! design exists to prevent and it is not visible in a handler's output.
//!
//! Three §-level contracts are projected rather than restated:
//!
//! * the error shape and vocabulary are §15's, held as a closed enum in [`error`] and checked against the
//!   generated `schemas/catalog/errors.yaml`;
//! * `/v1/commands` reports the §14 command catalog the contract gate checks, embedded at build time so a
//!   deployment cannot serve a catalog its own binary was not built from;
//! * `/v1/read-projections` reports the §10 read projections the same way.

pub mod commands;
pub mod error;
pub mod flags;
pub mod limits;
pub mod projections;
pub mod seams;
pub mod stream;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use quansio_core::{CanonicalId, CommandId, Prefix};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

pub use error::{ApiError, ApiErrorCode};
pub use flags::FeatureFlags;
pub use limits::RateLimiter;
pub use seams::{ConformanceStubHost, ConformanceStubModel, TenantMemberRoles};

use crate::runtime::state_machine::ModelProposalSource;
use crate::runtime::turn_loop::{RoleProvider, ToolHostPort};

/// The §14 command catalog, embedded so the served catalog is the one this binary was built from.
const COMMAND_CATALOG: &str = include_str!("../../../../schemas/catalog/commands.yaml");

/// The §10 read projections, embedded for the same reason.
const READ_PROJECTIONS: &str = include_str!("../../../../schemas/catalog/read-projections.yaml");

/// The §15 error catalog, used to prove the enum and the table agree.
pub const ERROR_CATALOG: &str = include_str!("../../../../schemas/catalog/errors.yaml");

/// The catalog as it is served.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Catalog {
    /// Group name to the commands it holds, sorted by group so the answer is deterministic.
    pub groups: BTreeMap<String, Vec<String>>,
}

impl Catalog {
    /// Every command, whatever group it is in.
    #[must_use]
    pub fn commands(&self) -> Vec<&str> {
        self.groups.values().flatten().map(String::as_str).collect()
    }

    /// Parse the embedded §14 catalog.
    ///
    /// # Errors
    /// Returns [`ApiError`] when the catalog is not the shape the contract defines, which would mean the
    /// binary was built from a catalog the gate never saw.
    pub fn from_embedded() -> Result<Self, ApiError> {
        let parsed: serde_yaml::Value = serde_yaml::from_str(COMMAND_CATALOG)
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "startup"))?;
        let groups = parsed
            .get("commands")
            .and_then(serde_yaml::Value::as_mapping)
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::Internal,
                    "the command catalog has no `commands` mapping",
                    "startup",
                )
            })?;
        let mut out = BTreeMap::new();
        for (group, commands) in groups {
            let group = group.as_str().unwrap_or_default().to_string();
            let commands = commands
                .as_sequence()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_yaml::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            out.insert(group, commands);
        }
        Ok(Self { groups: out })
    }

    /// The §10 read projections.
    ///
    /// # Errors
    /// Returns [`ApiError`] when the catalog is not the shape the contract defines.
    pub fn read_projections() -> Result<Vec<String>, ApiError> {
        let parsed: serde_yaml::Value = serde_yaml::from_str(READ_PROJECTIONS)
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "startup"))?;
        parsed
            .get("read_projections")
            .and_then(serde_yaml::Value::as_sequence)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_yaml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::Internal,
                    "the read-projection catalog has no `read_projections` list",
                    "startup",
                )
            })
    }
}

/// What a request says about itself (DOMAIN.md §1.2).
///
/// `command_id` is the idempotency anchor and is required for a mutating call: §1.2 has clients supply one
/// so a resend returns the original result rather than repeating the effect.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandRequest {
    /// The client-generated `cmd_` identifier.
    pub command_id: String,
    /// The command's parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// What a mutating call answers with.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CommandResponse {
    /// The `cmd_` identifier the call carried.
    pub command_id: String,
    /// Whether this call applied the command or found it already applied.
    pub replayed: bool,
    /// The canonical owner's result.
    pub result: serde_json::Value,
}

/// The tenant a call acts for, resolved from the credential's tenant claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantScope {
    /// The `tn_` identifier.
    pub tenant_id: String,
}

impl TenantScope {
    /// Resolve the scope from the request's tenant header.
    ///
    /// # Errors
    /// Returns [`ApiErrorCode::AuthRequired`] when no tenant is presented, and
    /// [`ApiErrorCode::ValidationBounds`] when the value is not a canonical `tn_` identifier — a malformed
    /// header is a bad request rather than an authentication failure, and the two are kept apart so a
    /// client can tell a missing credential from a typo.
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, ApiError> {
        let raw = headers
            .get("x-quansio-tenant")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::AuthRequired,
                    "the request carries no tenant",
                    "unresolved",
                )
            })?;
        CanonicalId::parse_typed(raw, Prefix::Tenant)
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    format!("the tenant header is not a canonical tenant id: {error}"),
                    "unresolved",
                )
            })
            .map(|_| Self {
                tenant_id: raw.to_string(),
            })
    }

    /// The idempotency anchor a mutating call must carry.
    ///
    /// # Errors
    /// Returns [`ApiErrorCode::ValidationBounds`] when the body's `command_id` is not a canonical `cmd_`
    /// identifier.
    pub fn command_id(request: &CommandRequest) -> Result<CommandId, ApiError> {
        CanonicalId::parse_typed(&request.command_id, Prefix::Command)
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    format!("command_id is not a canonical command id: {error}"),
                    "unresolved",
                )
            })
            .and_then(|id| {
                CommandId::new(id).map_err(|error| {
                    ApiError::new(
                        ApiErrorCode::ValidationBounds,
                        format!("command_id is not a canonical command id: {error}"),
                        "unresolved",
                    )
                })
            })
    }
}

/// The API's state: the pool, the catalogs, flags, limits and walking-skeleton seams.
#[derive(Clone)]
pub struct ApiState {
    pool: PgPool,
    catalog: Arc<Catalog>,
    projections: Arc<[String]>,
    flags: Arc<FeatureFlags>,
    limits: RateLimiter,
    model: Arc<dyn ModelProposalSource>,
    host: Arc<dyn ToolHostPort>,
    roles: Arc<dyn RoleProvider>,
}

impl ApiState {
    /// Build the state from a pool, reading the embedded catalogs once.
    ///
    /// # Errors
    /// Returns [`ApiError`] when a catalog is not the shape the contract defines.
    pub fn new(pool: PgPool) -> Result<Self, ApiError> {
        let (model, host, roles) = seams::skeleton_seams(None);
        Ok(Self {
            pool,
            catalog: Arc::new(Catalog::from_embedded()?),
            projections: Arc::from(Catalog::read_projections()?),
            flags: Arc::new(
                FeatureFlags::load()
                    .map_err(|error| ApiError::new(ApiErrorCode::Internal, error, "startup"))?,
            ),
            limits: RateLimiter::from_env(),
            model,
            host,
            roles,
        })
    }

    /// The catalog being served.
    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The read projections being served.
    #[must_use]
    pub fn projections(&self) -> &[String] {
        &self.projections
    }

    /// Evaluated feature flags.
    #[must_use]
    pub fn flags(&self) -> &FeatureFlags {
        &self.flags
    }

    /// Per-tenant rate limiter.
    #[must_use]
    pub fn limits(&self) -> &RateLimiter {
        &self.limits
    }

    /// Replace the rate limiter (tests).
    #[must_use]
    pub fn with_limits(mut self, limits: RateLimiter) -> Self {
        self.limits = limits;
        self
    }

    /// The pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Conformance-stub model source.
    #[must_use]
    pub fn model(&self) -> Arc<dyn ModelProposalSource> {
        Arc::clone(&self.model)
    }

    /// Conformance-stub tool host.
    #[must_use]
    pub fn host(&self) -> Arc<dyn ToolHostPort> {
        Arc::clone(&self.host)
    }

    /// Acting-role seam.
    #[must_use]
    pub fn roles(&self) -> Arc<dyn RoleProvider> {
        Arc::clone(&self.roles)
    }
}

/// The deployed surface.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/commands", get(command_catalog))
        .route("/v1/read-projections", get(read_projections))
        .route("/v1/commands/:command", post(commands::invoke))
        .route("/v1/stream", get(stream::stream))
        .route("/v1/threads/:id", get(projections::thread))
        .route("/v1/messages", get(projections::messages))
        .route("/v1/runs/:id", get(projections::run))
        .route("/v1/effects", get(projections::effects))
        .with_state(state)
}

/// Readiness: the process is up and the database answers.
async fn health(State(state): State<ApiState>) -> Result<Json<serde_json::Value>, ApiError> {
    let reachable = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(state.pool())
        .await
        .is_ok();
    if !reachable {
        return Err(ApiError::new(
            ApiErrorCode::Internal,
            "the database is not reachable",
            "health",
        ));
    }
    Ok(Json(serde_json::json!({
        "status": "ready",
        "commands": state.catalog().commands().len(),
        "read_projections": state.projections().len(),
    })))
}

/// The §14 command catalog.
async fn command_catalog(State(state): State<ApiState>) -> Json<Catalog> {
    Json(state.catalog().clone())
}

/// The §10 read projections.
async fn read_projections(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "read_projections": state.projections() }))
}

/// The status a successful mutating call is served with, so a caller can tell created from replayed.
#[must_use]
pub const fn created_status(replayed: bool) -> StatusCode {
    if replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    }
}
