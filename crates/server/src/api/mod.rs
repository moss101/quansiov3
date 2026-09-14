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

pub mod error;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use quansio_core::{
    CanonicalId, CommandId, CorrelationId, Digest, Generation, Prefix, UlidGenerator,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

pub use error::{ApiError, ApiErrorCode};

use crate::runtime::state_machine::{RuntimeEngine, RuntimeIdentity};

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

/// The API's state: the pool, the catalogs, and the tenant the call is for.
#[derive(Clone)]
pub struct ApiState {
    pool: PgPool,
    catalog: Arc<Catalog>,
    projections: Arc<[String]>,
}

impl ApiState {
    /// Build the state from a pool, reading the embedded catalogs once.
    ///
    /// # Errors
    /// Returns [`ApiError`] when a catalog is not the shape the contract defines.
    pub fn new(pool: PgPool) -> Result<Self, ApiError> {
        Ok(Self {
            pool,
            catalog: Arc::new(Catalog::from_embedded()?),
            projections: Arc::from(Catalog::read_projections()?),
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

    /// The pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// The deployed surface.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/commands", get(commands))
        .route("/v1/read-projections", get(read_projections))
        .route("/v1/commands/:command", post(invoke))
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
async fn commands(State(state): State<ApiState>) -> Json<Catalog> {
    Json(state.catalog().clone())
}

/// The §10 read projections.
async fn read_projections(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "read_projections": state.projections() }))
}

/// The one command this surface applies, until the rest are wired.
///
/// `CancelRun` is chosen deliberately: its canonical owner is unambiguous — the runtime owns a Run's
/// lifecycle — so this handler can call the owner and report the owner's answer without reinterpreting
/// anything. The commands whose owner is a *store* this deployment does not yet compose (`PostMessage`
/// creating a message and a run, `TriggerRoutineNow`) are refused rather than half-applied, because a
/// handler that applies half a command and reports success is worse than one that says it cannot.
const CANCEL_RUN: &str = "CancelRun";

/// Apply a command.
///
/// The command is recorded in `commands` — the log §1.2's idempotency is defined over — before the owner
/// is called, and the owner is the module that owns the change. A repeat of the same `command_id` with the
/// same parameters returns the recorded result; a repeat with different parameters is refused, because the
/// two cannot both be what that command meant.
async fn invoke(
    State(state): State<ApiState>,
    Path(command): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let scope = TenantScope::from_headers(&headers)?;
    let command_id = TenantScope::command_id(&request)?;
    // The correlation id is a bare ULID shared by everything caused by one external input (DOMAIN.md §1.2), so
    // it is generated once here and carried into the runtime identity.
    let correlation_id = CorrelationId::generate(&mut UlidGenerator::new());
    let correlation = correlation_id.to_string();

    let params_digest = Digest::of_canonical_json(&request.params.to_string())
        .as_str()
        .to_string();
    let known = state
        .catalog()
        .commands()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&command));
    if !known {
        return Err(ApiError::new(
            ApiErrorCode::NotFound,
            format!("{command:?} is not a command in the catalog"),
            correlation,
        ));
    }

    // The idempotency decision and the record are one statement: the unique key is (tenant, command_id), so
    // a concurrent replay cannot insert a second row and both callers see the row that won.
    let inserted = sqlx::query(
        "INSERT INTO commands (id, tenant_id, command_name, command_id, params_digest, status, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, 'accepted', $6) \
         ON CONFLICT (tenant_id, command_id) DO NOTHING",
    )
    .bind(command_id.as_canonical().to_string())
    .bind(&scope.tenant_id)
    .bind(&command)
    .bind(command_id.as_canonical().to_string())
    .bind(&params_digest)
    .bind(&correlation)
    .execute(state.pool())
    .await
    .map_err(|error| database_error(&error))?;

    if inserted.rows_affected() == 0 {
        let existing = sqlx::query_as::<_, (String, Option<serde_json::Value>, Option<serde_json::Value>)>(
            "SELECT params_digest, result, error FROM commands WHERE tenant_id = $1 AND command_id = $2",
        )
        .bind(&scope.tenant_id)
        .bind(command_id.as_canonical().to_string())
        .fetch_optional(state.pool())
        .await
        .map_err(|error| database_error(&error))?
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::Internal,
                "a command conflicted but cannot be read back",
                correlation.clone(),
            )
        })?;
        if existing.0 != params_digest {
            return Err(ApiError::new(
                ApiErrorCode::ConflictIdempotencyMismatch,
                "this command_id was already used with different parameters",
                correlation,
            ));
        }
        return Ok(Json(CommandResponse {
            command_id: command_id.as_canonical().to_string().to_string(),
            replayed: true,
            result: existing.1.or(existing.2).unwrap_or(serde_json::Value::Null),
        }));
    }

    let result = match command.as_str() {
        CANCEL_RUN => {
            let run_id = CanonicalId::parse_typed(
                request
                    .params
                    .get("run_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ApiError::new(
                            ApiErrorCode::ValidationSchema,
                            "CancelRun needs a run_id",
                            correlation.clone(),
                        )
                    })?,
                Prefix::Run,
            )
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    error.to_string(),
                    correlation.clone(),
                )
            })?;
            let generation = Generation::new(
                request
                    .params
                    .get("generation")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(Generation::INITIAL.get()),
            )
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    error.to_string(),
                    correlation.clone(),
                )
            })?;
            let identity = RuntimeIdentity::system(&scope.tenant_id, "api", correlation_id)
                .with_command_id(command_id);
            let engine = RuntimeEngine::new(state.pool().clone(), identity).map_err(|error| {
                ApiError::new(
                    ApiErrorCode::Internal,
                    error.to_string(),
                    correlation.clone(),
                )
            })?;
            // The runtime owns the Run's lifecycle: this handler asks it to cancel and reports what it
            // said, rather than writing the status itself.
            let run = engine
                .cancel(&run_id, generation)
                .await
                .map_err(|error| runtime_error(error, &correlation))?;
            serde_json::json!({ "run_id": run.id.to_string(), "status": run.status.as_db_str() })
        }
        // The catalog names every command; this surface applies one until the rest are wired. Refusing
        // rather than pretending is what keeps the catalog an honest statement of what exists.
        other => {
            return Err(ApiError::new(
                ApiErrorCode::Internal,
                format!("{other} is in the catalog and has no handler in this build"),
                correlation,
            ))
        }
    };

    sqlx::query(
        "UPDATE commands SET status = 'applied', result = $3 WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(command_id.as_canonical().to_string())
    .bind(&result)
    .execute(state.pool())
    .await
    .map_err(|error| database_error(&error))?;

    Ok(Json(CommandResponse {
        command_id: command_id.as_canonical().to_string().to_string(),
        replayed: false,
        result,
    }))
}

/// A database failure is never the caller's fault and never carries a row back.
fn database_error(error: &sqlx::Error) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "database")
}

/// Map a runtime refusal onto §15. The mapping is exhaustive over the codes the runtime can produce, so a
/// new one is a compile error rather than a silent 500.
fn runtime_error(
    error: crate::runtime::state_machine::RuntimeError,
    correlation: &str,
) -> ApiError {
    let code = match error.code() {
        "RUNTIME_ILLEGAL_TRANSITION" => ApiErrorCode::RuntimeIllegalTransition,
        "FENCED_STALE_GENERATION" => ApiErrorCode::FencedStaleGeneration,
        "RUNTIME_NOT_FOUND" => ApiErrorCode::NotFound,
        "RUNTIME_SCHEMA" => ApiErrorCode::ValidationSchema,
        "RUNTIME_STATE_CONFLICT" => ApiErrorCode::ConflictState,
        "RUNTIME_LEASE_LOST" => ApiErrorCode::LeaseLost,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
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
