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
pub mod browser_imports;
pub mod collaboration;
pub mod commands;
pub mod diagnostics;
pub mod enterprise;
pub mod error;
pub mod flags;
pub mod hooks;
pub mod knowledge;
pub mod limits;
pub mod live;
pub mod live_sessions;
pub mod maintenance;
pub mod ops;
pub mod packs;
pub mod payment_inputs;
pub mod projections;
pub mod rbac;
pub mod recordings;
pub mod remote;
pub mod routines;
pub mod scim;
pub mod seams;
pub mod search;
pub mod stream;
pub mod teammates;
pub mod threads;
pub mod trust;
pub mod web;
pub mod webhooks;
pub mod workflow;
pub mod workgraph;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use quansio_core::{CanonicalId, CommandId, Prefix};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};

use crate::artifacts::ObjectStore;
use crate::control::identity::auth::{AuthError, AuthStore, SigningKey};
use crate::control::identity::enterprise::KEY_PREFIX as PRINCIPAL_KEY_PREFIX;
use crate::control::identity::oauth::{AuthFile, HttpExchange, OAuthExchange, ReadyProvider};
use crate::control::identity::{EnterpriseConfig, EnterpriseIdentity, PrincipalScope};
use crate::control::schema;

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
    /// When the command was accepted (its record committed); a replay reports the original.
    pub accepted_at: chrono::DateTime<chrono::Utc>,
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
    /// The service principal, when the request presented a `qsk_` key (OPS-001). Only the
    /// command surface and SCIM resolve one; see [`TenantScope::resolve_actor`].
    pub principal: Option<PrincipalScope>,
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

/// The bearer credential a request presents, if any.
#[must_use]
pub fn bearer_of(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

impl TenantScope {
    /// Resolve the scope: a bearer session first, then the trusted headers if the state allows them.
    ///
    /// # Errors
    /// [`ApiErrorCode::AuthRequired`] when nothing establishes a scope, [`ApiErrorCode::AuthInvalidToken`]
    /// / [`ApiErrorCode::AuthExpired`] for a bearer token that does not verify, and
    /// [`ApiErrorCode::ValidationBounds`] for a malformed trusted header.
    pub async fn resolve(state: &ApiState, headers: &HeaderMap) -> Result<Self, ApiError> {
        if bearer_of(headers).is_some_and(|bearer| bearer.starts_with(PRINCIPAL_KEY_PREFIX)) {
            return Err(ApiError::new(
                ApiErrorCode::ScopeForbidden,
                "a service principal key is accepted only by the command surface and SCIM",
                "unresolved",
            ));
        }
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
                principal: None,
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
                principal: None,
            })
    }

    /// Resolve the acting identity for the command surface: a service principal key (OPS-001) as
    /// well as everything [`Self::resolve`] accepts.
    ///
    /// # Errors
    /// [`ApiErrorCode::AuthInvalidToken`] for a key that does not authenticate, otherwise as
    /// [`Self::resolve`].
    pub async fn resolve_actor(state: &ApiState, headers: &HeaderMap) -> Result<Self, ApiError> {
        match bearer_of(headers) {
            Some(key) if key.starts_with(PRINCIPAL_KEY_PREFIX) => {
                let principal = state
                    .enterprise()
                    .authenticate_principal(key)
                    .await
                    .map_err(|error| enterprise::api_error(error, "unresolved"))?;
                Ok(Self {
                    tenant_id: principal.tenant_id.clone(),
                    user_id: None,
                    session_id: None,
                    principal: Some(principal),
                })
            }
            _ => Self::resolve(state, headers).await,
        }
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
    /// The host the artifact and output tools wrap, kept so a worker port composed later can rebuild
    /// the server-hosted layer over it.
    inner_host: Arc<dyn ToolHostPort>,
    outputs: Option<crate::artifacts::outputs::OutputToolHost>,
    /// The host of `computer.*` calls (ADP-002): composed over the database with the bridge this build has, which is none
    /// that can reach a machine until the native helper exists (`UnavailableComputerBridge`).
    computer: Arc<crate::computer::ComputerToolHost>,
    /// The host of the worker tools (`fs.*`, `terminal.exec`, `process.spawn`) when the deployment composes a worker hub
    /// (EXEC-002). Without one those calls reach the wrapped host, which in a served build is the conformance stub.
    worker: Option<Arc<crate::worker::WorkerToolHost>>,
    roles: Arc<dyn RoleProvider>,
    auth: Arc<AuthStore>,
    auth_file: Arc<AuthFile>,
    oauth_exchange: Arc<dyn OAuthExchange>,
    magic_link_delivery: auth::SharedDelivery,
    scope_trust: ScopeTrust,
    live: Arc<live::LiveHub>,
    remote: Arc<remote::RemoteHub>,
    objects: Option<Arc<dyn ObjectStore>>,
    search: Option<Arc<search::SearchService>>,
    web_root: Option<web::WebRoot>,
    keys: Option<Arc<dyn quansio_machine::secrets::KeyProvider>>,
    intelligence: Option<crate::intelligence::IntelligenceClient>,
    metrics_token: Option<Arc<str>>,
    background_turns: bool,
    enterprise: Arc<EnterpriseIdentity>,
    automation: Arc<crate::control::routines::config::AutomationConfig>,
    hook_limits: RateLimiter,
    dry_run_limits: RateLimiter,
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
        let automation = crate::control::routines::config::AutomationConfig::shipped();
        let (model, inner_host, roles) = seams::skeleton_seams(pool.clone(), None);
        let computer = Arc::new(crate::computer::ComputerToolHost::new(pool.clone()));
        let host: Arc<dyn ToolHostPort> = Arc::new(crate::computer::ComputerRoutedHost::new(
            Arc::clone(&computer),
            Arc::clone(&inner_host),
        ));
        let auth_file = AuthFile::load()
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error, "startup"))?;
        let key = SigningKey::from_env()
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "startup"))?;
        let auth = AuthStore::new(pool.clone(), Arc::new(key), auth_file.auth_config());
        let enterprise = EnterpriseConfig::load()
            .and_then(|config| EnterpriseIdentity::new(auth.clone(), config))
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error, "startup"))?;
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
            inner_host,
            outputs: None,
            computer,
            worker: None,
            host,
            roles,
            auth: Arc::new(auth),
            auth_file: Arc::new(auth_file),
            oauth_exchange: Arc::new(HttpExchange),
            magic_link_delivery: Arc::new(auth::ConsoleDelivery),
            scope_trust: ScopeTrust::TrustedHeaders,
            live: live::LiveHub::new(),
            remote: Arc::new(remote::RemoteHub::default()),
            objects: None,
            search: None,
            web_root: None,
            keys: None,
            intelligence: None,
            metrics_token: None,
            background_turns: false,
            enterprise: Arc::new(enterprise),
            hook_limits: RateLimiter::new(
                automation.hooks.rate_per_minute,
                std::time::Duration::from_secs(60),
            ),
            dry_run_limits: RateLimiter::new(
                automation.dry_run.rate_per_minute,
                std::time::Duration::from_secs(60),
            ),
            automation: Arc::new(automation),
        })
    }

    /// `config/automation.yaml` as the deployment runs it (ADP-007).
    #[must_use]
    pub fn automation(&self) -> &Arc<crate::control::routines::config::AutomationConfig> {
        &self.automation
    }

    /// Run under `config` instead of the shipped automation policy, with its rate limiters (tests).
    #[must_use]
    pub fn with_automation_config(
        mut self,
        config: crate::control::routines::config::AutomationConfig,
    ) -> Self {
        self.hook_limits = RateLimiter::new(
            config.hooks.rate_per_minute,
            std::time::Duration::from_secs(60),
        );
        self.dry_run_limits = RateLimiter::new(
            config.dry_run.rate_per_minute,
            std::time::Duration::from_secs(60),
        );
        self.automation = Arc::new(config);
        self
    }

    /// Requests per inbound hook endpoint per minute.
    #[must_use]
    pub fn hook_limits(&self) -> &RateLimiter {
        &self.hook_limits
    }

    /// Dry runs per tenant per minute.
    #[must_use]
    pub fn dry_run_limits(&self) -> &RateLimiter {
        &self.dry_run_limits
    }

    /// Enterprise identity with `config` instead of `config/identity.yaml` (tests admit the
    /// in-test IdP's loopback issuer this way).
    ///
    /// # Errors
    /// Returns [`ApiError`] when the HTTP client cannot be built.
    pub fn with_enterprise_config(mut self, config: EnterpriseConfig) -> Result<Self, ApiError> {
        self.enterprise = Arc::new(
            EnterpriseIdentity::new((*self.auth).clone(), config)
                .map_err(|error| ApiError::new(ApiErrorCode::Internal, error, "startup"))?,
        );
        Ok(self)
    }

    /// Enterprise identity: SSO, SCIM, service principals (OPS-001).
    #[must_use]
    pub fn enterprise(&self) -> &EnterpriseIdentity {
        &self.enterprise
    }

    /// Answer a command that continues a run once it is accepted, and run the turn after it: a
    /// model's turn takes as long as the model does, and a command is accepted when its own state is
    /// committed (DOMAIN.md §9; DOSSIER.md §21.2 "command accepted"). The turn's progress, reply and
    /// any question or approval it raises reach clients as events and live frames.
    #[must_use]
    pub fn with_background_turns(mut self) -> Self {
        self.background_turns = true;
        self
    }

    /// Whether a command that continues a run answers once it is accepted and runs the turn after
    /// (the served composition), rather than waiting for the turn inside the command.
    #[must_use]
    pub const fn background_turns(&self) -> bool {
        self.background_turns
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

    /// The independent semantic verifier completion claims are judged by (RUN-008): the plane's
    /// `VerifyCompletion` when the deployment has the intelligence plane, and otherwise one that
    /// refuses, so a contract requiring semantic verification never passes without it.
    #[must_use]
    pub fn semantic_verifier(&self) -> Arc<dyn crate::runtime::verification::SemanticVerifierPort> {
        let settings = crate::runtime::verification::SemanticVerificationSettings::load();
        match (&self.intelligence, settings) {
            (Some(client), Ok(settings)) => {
                Arc::new(crate::runtime::verification::GatewaySemanticVerifier::new(
                    client.clone(),
                    settings,
                ))
            }
            _ => Arc::new(crate::runtime::verification::UnavailableSemanticVerifier),
        }
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
        // The output tools (ADP-006) are hosted beside them. No worker port is composed here, so a tool
        // that needs a browser or `ffmpeg` fails ADAPTER_UNAVAILABLE until `with_output_host` supplies one.
        let outputs =
            crate::artifacts::outputs::OutputToolHost::new(self.pool.clone(), objects.clone());
        self.outputs = Some(outputs);
        self.objects = Some(objects);
        self.rebuild_host();
        self
    }

    /// The host of the output tools, when the deployment has an object store.
    #[must_use]
    pub fn output_host(&self) -> Option<&crate::artifacts::outputs::OutputToolHost> {
        self.outputs.as_ref()
    }

    /// Replace the host of the output tools, for a deployment that has composed worker ports
    /// (a render host, an `ffmpeg` target, a transcription route).
    #[must_use]
    pub fn with_output_host(mut self, outputs: crate::artifacts::outputs::OutputToolHost) -> Self {
        self.outputs = Some(outputs);
        self.rebuild_host();
        self
    }

    fn rebuild_host(&mut self) {
        // The server-hosted layer (artifact and output tools) when there is an object store; otherwise the host the
        // state started with. `computer.*` goes to the computer host in front of either.
        let base: Arc<dyn ToolHostPort> = match self.objects.clone() {
            Some(objects) => {
                let mut host = crate::artifacts::tool_host::ServerToolHost::new(
                    crate::artifacts::tool_host::ArtifactToolHost::new(self.pool.clone(), objects),
                    Arc::clone(&self.inner_host),
                );
                if let Some(outputs) = self.outputs.clone() {
                    host = host.with_outputs(outputs);
                }
                Arc::new(host)
            }
            None => Arc::clone(&self.inner_host),
        };
        // The worker tools go to the attached worker when a hub is composed; every other call is the base host's.
        let base: Arc<dyn ToolHostPort> = match self.worker.clone() {
            Some(worker) => Arc::new(crate::worker::WorkerRoutedHost::new(worker, base)),
            None => base,
        };
        self.host = Arc::new(crate::computer::ComputerRoutedHost::new(
            Arc::clone(&self.computer),
            base,
        ));
    }

    /// Run the worker tools on the workers attached to `hub` (EXEC-002). A workspace with no attached worker has its
    /// worker tools refused `WORKER_UNAVAILABLE`: they are never answered by the conformance stub.
    #[must_use]
    pub fn with_worker_hub(mut self, hub: crate::worker::WorkerHub) -> Self {
        self.worker = Some(Arc::new(crate::worker::WorkerToolHost::new(hub)));
        self.rebuild_host();
        self
    }

    /// Reach a machine's input through `bridge`, recognising `known` applications up to `grant` (ADP-002). The
    /// deployable composes none today: the helper that holds the operating-system permissions does not exist yet, so
    /// every `computer.*` call is refused `TARGET_UNAVAILABLE`-style before any input. A test supplies a scripted one.
    #[must_use]
    pub fn with_computer_bridge(
        mut self,
        bridge: Arc<dyn quansio_machine::computer_use::bridge::NativeComputerBridge>,
        known: quansio_machine::computer_use::KnownApps,
        grant: quansio_machine::computer_use::ComputerGrant,
    ) -> Self {
        self.computer = Arc::new((*self.computer).clone().with_bridge(bridge, known, grant));
        self.rebuild_host();
        self
    }

    /// The stop flags of the native computer sequences in flight: a person's `CancelRun` or `PauseRun` raises the one of
    /// the run it names, so the sequence ends before it asks for another action (DOMAIN.md 8.6.1 B).
    #[must_use]
    pub fn computer_stops(&self) -> &crate::computer::StopRegistry {
        self.computer.stops()
    }

    /// Run remote control against another configuration. A test shortens the readiness window; the deployable reads
    /// `config/remote-control.yaml`.
    #[must_use]
    pub fn with_remote_config(mut self, config: quansio_machine::remote::RemoteConfig) -> Self {
        self.remote = Arc::new(remote::RemoteHub::new(config));
        self
    }

    /// The remote-control sessions of this process (ADP-009).
    #[must_use]
    pub fn remote(&self) -> &Arc<remote::RemoteHub> {
        &self.remote
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

    /// A transaction in `tenant_id`'s scope, for a statement the API runs itself (the command log, a read
    /// projection) or a store that works on the caller's connection.
    ///
    /// Row-level security is forced on every tenant table and the tenant setting lasts one transaction, so
    /// a statement on the bare pool, or on a connection taken from it outside a transaction, runs with no
    /// tenant: as the application role it reads nothing and may write nothing.
    ///
    /// # Errors
    /// Returns [`ApiErrorCode::Internal`] when no transaction can begin or the tenant id is not canonical.
    pub(crate) async fn tenant_tx(
        &self,
        tenant_id: &str,
        correlation: &str,
    ) -> Result<Transaction<'static, Postgres>, ApiError> {
        let unscoped = |error: &dyn std::fmt::Display| {
            ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation)
        };
        let mut tx = self.pool.begin().await.map_err(|error| unscoped(&error))?;
        schema::set_tenant_context(&mut tx, tenant_id)
            .await
            .map_err(|error| unscoped(&error))?;
        Ok(tx)
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
        .route("/v1/hooks/:endpoint", post(hooks::receive))
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
        .route("/v1/auth/sso/:tenant_id/start", get(enterprise::sso_start))
        .route(
            "/v1/auth/sso/:tenant_id/complete",
            post(enterprise::sso_complete),
        )
        .route("/v1/auth/sessions", get(enterprise::sessions))
        .route(
            "/v1/auth/sessions/:session_id/revoke",
            post(enterprise::revoke_session),
        )
        .route(
            "/v1/service-principals",
            get(enterprise::service_principals),
        )
        .route("/v1/sso", get(enterprise::sso))
        .route(
            "/scim/v2/ServiceProviderConfig",
            get(scim::service_provider_config),
        )
        .route(
            "/scim/v2/Users",
            get(scim::list_users).post(scim::create_user),
        )
        .route(
            "/scim/v2/Users/:id",
            get(scim::get_user)
                .put(scim::replace_user)
                .patch(scim::patch_user)
                .delete(scim::delete_user),
        )
        .route(
            "/scim/v2/Groups",
            get(scim::list_groups).post(scim::create_group),
        )
        .route(
            "/scim/v2/Groups/:id",
            get(scim::get_group)
                .put(scim::replace_group)
                .patch(scim::patch_group)
                .delete(scim::delete_group),
        )
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
        .route("/v1/packs", get(packs::packs))
        .route("/v1/recordings", get(recordings::recordings))
        .route("/v1/recordings/:id", get(recordings::recording))
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
        .route("/v1/maintenance", get(maintenance::status))
        .route("/v1/ops/switches", get(ops::switches))
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
