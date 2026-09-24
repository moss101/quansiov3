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

pub mod admin;
pub mod artifacts;
pub mod auth;
pub mod collaboration;
pub mod commands;
pub mod diagnostics;
pub mod error;
pub mod flags;
pub mod knowledge;
pub mod limits;
pub mod live;
pub mod live_sessions;
pub mod projections;
pub mod rbac;
pub mod routines;
pub mod seams;
pub mod search;
pub mod stream;
pub mod teammates;
pub mod threads;
pub mod trust;
pub mod web;
pub mod webhooks;
pub mod workgraph;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use quansio_core::{CanonicalId, CommandId, Prefix};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::artifacts::ObjectStore;
use crate::control::identity::auth::{AuthError, AuthStore, SigningKey};
use crate::control::identity::oauth::{AuthFile, HttpExchange, OAuthExchange, ReadyProvider};

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
    /// The acting `usr_`, when the request carries a session (or a trusted user header).
    pub user_id: Option<String>,
    /// The `ses_` the bearer token belongs to, when authenticated by session.
    pub session_id: Option<String>,
}

/// Where a request's scope may come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeTrust {
    /// Only a bearer access token establishes scope (the deployable's default).
    SessionOnly,
    /// `x-quansio-tenant` / `x-quansio-user` headers are believed as-is: in-process test
    /// routers and explicitly configured development harnesses.
    TrustedHeaders,
}

impl TenantScope {
    /// Resolve the scope: a bearer session first, then the trusted headers if the state allows them.
    ///
    /// # Errors
    /// [`ApiErrorCode::AuthRequired`] when nothing establishes a scope, [`ApiErrorCode::AuthInvalidToken`]
    /// / [`ApiErrorCode::AuthExpired`] for a bearer token that does not verify, and
    /// [`ApiErrorCode::ValidationBounds`] for a malformed trusted header.
    pub async fn resolve(state: &ApiState, headers: &HeaderMap) -> Result<Self, ApiError> {
        if let Some(bearer) = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            let session = state
                .auth()
                .authenticate(bearer)
                .await
                .map_err(|error| match error {
                    AuthError::Expired(message) => {
                        ApiError::new(ApiErrorCode::AuthExpired, message, "unresolved")
                    }
                    AuthError::InvalidToken(message) => {
                        ApiError::new(ApiErrorCode::AuthInvalidToken, message, "unresolved")
                    }
                    other => ApiError::new(ApiErrorCode::Internal, other.to_string(), "unresolved"),
                })?;
            return Ok(Self {
                tenant_id: session.tenant_id,
                user_id: Some(session.user_id),
                session_id: Some(session.session_id),
            });
        }
        match state.scope_trust() {
            ScopeTrust::TrustedHeaders => Self::from_headers(headers),
            ScopeTrust::SessionOnly => Err(ApiError::new(
                ApiErrorCode::AuthRequired,
                "the request carries no bearer session",
                "unresolved",
            )),
        }
    }

    /// Resolve the scope from the request's trusted tenant/user headers.
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
                user_id: headers
                    .get("x-quansio-user")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
                session_id: None,
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
    auth: Arc<AuthStore>,
    auth_file: Arc<AuthFile>,
    oauth_exchange: Arc<dyn OAuthExchange>,
    magic_link_delivery: auth::SharedDelivery,
    scope_trust: ScopeTrust,
    live: Arc<live::LiveHub>,
    objects: Option<Arc<dyn ObjectStore>>,
    search: Option<Arc<search::SearchService>>,
    web_root: Option<web::WebRoot>,
    keys: Option<Arc<dyn quansio_machine::secrets::KeyProvider>>,
    intelligence: Option<crate::intelligence::IntelligenceClient>,
    metrics_token: Option<Arc<str>>,
}

impl ApiState {
    /// Build the state from a pool, reading the embedded catalogs once.
    ///
    /// The scope trust defaults to [`ScopeTrust::TrustedHeaders`], which is what an in-process
    /// test router wants; the deployable composition sets [`ScopeTrust::SessionOnly`] unless a
    /// development harness asks otherwise. The signing key comes from secret custody or is
    /// ephemeral (see [`SigningKey::from_env`]).
    ///
    /// # Errors
    /// Returns [`ApiError`] when a catalog or `config/auth.yaml` is not the shape the contract defines.
    pub fn new(pool: PgPool) -> Result<Self, ApiError> {
        let (model, host, roles) = seams::skeleton_seams(pool.clone(), None);
        let auth_file = AuthFile::load()
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error, "startup"))?;
        let key = SigningKey::from_env()
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "startup"))?;
        let auth = AuthStore::new(pool.clone(), Arc::new(key), auth_file.auth_config());
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
            auth: Arc::new(auth),
            auth_file: Arc::new(auth_file),
            oauth_exchange: Arc::new(HttpExchange),
            magic_link_delivery: Arc::new(auth::ConsoleDelivery),
            scope_trust: ScopeTrust::TrustedHeaders,
            live: live::LiveHub::new(),
            objects: None,
            search: None,
            web_root: None,
            keys: None,
            intelligence: None,
            metrics_token: None,
        })
    }

    /// Serve `GET /metrics` to the operator presenting `token` as a bearer (OPS-003).
    #[must_use]
    pub fn with_metrics_token(mut self, token: &str) -> Self {
        self.metrics_token = Some(Arc::from(token));
        self
    }

    /// The metrics token, when metrics are served.
    #[must_use]
    pub fn metrics_token(&self) -> Option<&str> {
        self.metrics_token.as_deref()
    }

    /// The intelligence plane's client, for control-plane calls that are not a turn's model
    /// call (skill evolution, CAP-008). Without one those commands fail closed.
    #[must_use]
    pub fn with_intelligence(mut self, client: crate::intelligence::IntelligenceClient) -> Self {
        self.intelligence = Some(client);
        self
    }

    /// The intelligence plane's client, when the deployment has one.
    #[must_use]
    pub fn intelligence(&self) -> Option<&crate::intelligence::IntelligenceClient> {
        self.intelligence.as_ref()
    }

    /// Replace the walking-skeleton model with a real proposal source — the intelligence
    /// gateway (`intelligence::model_source::GatewayModelSource`) in the deployable (APP-001).
    #[must_use]
    pub fn with_model_source(mut self, model: Arc<dyn ModelProposalSource>) -> Self {
        self.model = model;
        self
    }

    /// Install the key provider that seals and opens secret handles (EXEC-007): webhook
    /// signing secrets (APP-015). Without one, creating a webhook fails closed.
    #[must_use]
    pub fn with_key_provider(
        mut self,
        keys: Arc<dyn quansio_machine::secrets::KeyProvider>,
    ) -> Self {
        self.keys = Some(keys);
        self
    }

    /// The key provider, when the deployment has one.
    #[must_use]
    pub fn key_provider(&self) -> Option<Arc<dyn quansio_machine::secrets::KeyProvider>> {
        self.keys.clone()
    }

    /// Serve the built web app from `root` under `/app/` (APP-012).
    #[must_use]
    pub fn with_web_root(mut self, root: web::WebRoot) -> Self {
        self.web_root = Some(root);
        self
    }

    /// The web app's asset root, when the deployable serves it.
    #[must_use]
    pub fn web_root(&self) -> Option<&web::WebRoot> {
        self.web_root.as_ref()
    }

    /// Install the object store attachments are written to (CORE-007's store; none means
    /// uploads fail closed with `TARGET_UNAVAILABLE`).
    #[must_use]
    pub fn with_object_store(mut self, objects: Arc<dyn ObjectStore>) -> Self {
        // With a store, the server hosts the artifact tools itself (CAP-003); every other
        // tool still goes to the host it had.
        self.host = Arc::new(crate::artifacts::tool_host::ServerToolHost::new(
            crate::artifacts::tool_host::ArtifactToolHost::new(self.pool.clone(), objects.clone()),
            Arc::clone(&self.host),
        ));
        self.objects = Some(objects);
        self
    }

    /// The live-frame hub `/v1/stream` forwards from and the runtime publishes to.
    #[must_use]
    pub fn live(&self) -> &Arc<live::LiveHub> {
        &self.live
    }

    /// The object store, when one is configured.
    #[must_use]
    pub fn objects(&self) -> Option<&Arc<dyn ObjectStore>> {
        self.objects.as_ref()
    }

    /// Install INT-004's derived search index (`/v1/search` artifacts channel over content).
    #[must_use]
    pub fn with_search_index(mut self, index: Arc<quansio_indexer::SearchIndex>) -> Self {
        self.search = Some(Arc::new(search::SearchService::new(index)));
        self
    }

    /// The search index, when one is configured.
    #[must_use]
    pub fn search_index(&self) -> Option<&Arc<quansio_indexer::SearchIndex>> {
        self.search.as_ref().map(|service| service.index())
    }

    /// The search service, when an index is configured.
    #[must_use]
    pub fn search(&self) -> Option<&Arc<search::SearchService>> {
        self.search.as_ref()
    }

    /// Choose how requests establish their scope.
    #[must_use]
    pub const fn with_scope_trust(mut self, trust: ScopeTrust) -> Self {
        self.scope_trust = trust;
        self
    }

    /// Replace the magic-link delivery port (tests, or the email channel).
    #[must_use]
    pub fn with_magic_link_delivery(mut self, delivery: auth::SharedDelivery) -> Self {
        self.magic_link_delivery = delivery;
        self
    }

    /// Replace the OAuth code exchange (tests against a stub provider).
    #[must_use]
    pub fn with_oauth_exchange(mut self, exchange: Arc<dyn OAuthExchange>) -> Self {
        self.oauth_exchange = exchange;
        self
    }

    /// The session authority.
    #[must_use]
    pub fn auth(&self) -> &Arc<AuthStore> {
        &self.auth
    }

    /// The OAuth providers offered by this deployment (client id present).
    #[must_use]
    pub fn oauth_providers(&self) -> Vec<ReadyProvider> {
        AuthStore::ready_providers(&self.auth_file)
    }

    /// The OAuth code exchange in use.
    #[must_use]
    pub fn oauth_exchange(&self) -> &Arc<dyn OAuthExchange> {
        &self.oauth_exchange
    }

    /// The magic-link delivery port in use.
    #[must_use]
    pub fn magic_link_delivery(&self) -> &auth::SharedDelivery {
        &self.magic_link_delivery
    }

    /// How requests may establish their scope.
    #[must_use]
    pub const fn scope_trust(&self) -> ScopeTrust {
        self.scope_trust
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
        .route("/v1/auth/magic-link/start", post(auth::magic_link_start))
        .route(
            "/v1/auth/magic-link/complete",
            post(auth::magic_link_complete),
        )
        .route("/v1/auth/oauth/:provider/start", get(auth::oauth_start))
        .route(
            "/v1/auth/oauth/:provider/complete",
            post(auth::oauth_complete),
        )
        .route("/v1/auth/refresh", post(auth::refresh))
        .route("/v1/auth/sign-out", post(auth::sign_out))
        .route("/v1/me", get(auth::me))
        .route("/v1/stream", get(stream::stream))
        .route("/v1/stream/events", get(stream::stream_events))
        .route("/v1/threads", get(collaboration::list_threads))
        .route("/v1/threads/:id", get(collaboration::thread))
        .route("/v1/messages", get(collaboration::messages))
        .route("/v1/runs/:id", get(projections::run))
        .route("/v1/runs/:id/timeline", get(threads::timeline))
        .route("/v1/questions", get(threads::questions))
        .route("/v1/artifacts", get(artifacts::list_artifacts))
        .route(
            "/v1/artifacts/:id/versions",
            get(artifacts::artifact_versions),
        )
        .route(
            "/v1/artifacts/:id/versions/:vid/content",
            get(artifacts::artifact_content),
        )
        .route("/v1/evidence/:id", get(artifacts::evidence))
        .route("/v1/evidence/:id/content", get(trust::evidence_content))
        .route("/v1/approvals", get(trust::approvals))
        .route("/v1/search", get(search::search))
        .route("/v1/workgraph", get(workgraph::workgraph))
        .route("/v1/effects", get(trust::effects))
        .route("/v1/knowledge", get(knowledge::knowledge))
        .route("/v1/memory", get(knowledge::memory))
        .route("/v1/skills", get(knowledge::skills))
        .route("/v1/members", get(admin::members))
        .route("/v1/policies", get(admin::policies))
        .route("/v1/connectors", get(admin::connectors))
        .route("/v1/usage", get(admin::usage))
        .route("/v1/audit", get(admin::audit_log))
        .route("/v1/teammates", get(teammates::roster))
        .route("/v1/routines", get(routines::routines))
        .route("/v1/notifications", get(routines::notifications))
        .route("/v1/webhooks", get(webhooks::webhooks))
        .route("/v1/webhooks/:id/deliveries", get(webhooks::deliveries))
        .route("/v1/targets", get(live_sessions::targets))
        .route(
            "/v1/browser-sessions/:id",
            get(live_sessions::browser_session),
        )
        .route("/app", get(web::index))
        .route("/app/", get(web::index))
        .route("/app/*path", get(web::asset))
        .route("/v1/diagnostics/bundle", get(diagnostics::bundle))
        .route("/metrics", get(diagnostics::metrics_endpoint))
        .route_layer(axum::middleware::from_fn(diagnostics::trace_requests))
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
