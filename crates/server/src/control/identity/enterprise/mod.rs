//! Enterprise identity (OPS-001, DOMAIN.md §2): per-tenant OIDC single sign-on, SCIM 2.0
//! provisioning, service principals and session revocation.
//!
//! The canonical owner of organisation-grade authentication. It writes the identity owner's tables
//! (`users`, memberships, `sessions`, `service_principals`) plus its own (`sso_*`,
//! `external_identities`, `directory_*`). Every membership change commits in one transaction with
//! its `member.*`/`tenant.*` RuntimeEvent and its AuditEntry.
//! - [`oidc`] runs the authorization-code flow against the tenant's IdP.
//! - [`jwt`] verifies the id_token.
//! - [`mapping`] turns external groups into roles, server-side only.
//! - [`scim`] is the provisioning surface.
//! - [`principals`] mints and authenticates workload identities.
//! - [`sessions`] revokes.

pub mod jwt;
pub mod mapping;
pub mod oidc;
pub mod principals;
pub mod scim;
pub mod sessions;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};

pub use oidc::{SsoSettings, SsoStart};
pub use principals::{PrincipalScope, KEY_PREFIX, PROVISIONING_SCOPE};
pub use scim::{ScimError, ScimReply};

use super::auth::{AuthError, AuthStore};
use super::members::MemberAdmin;
use super::IdentityError;
use crate::audit::{append_in, AuditDraft};
use crate::control::schema;

/// Repository path of this owner, named in refusals.
pub const ENTERPRISE_OWNER: &str = "crates/server/src/control/identity/enterprise";

/// Environment of the live IdP proof (tests only): a sandbox realm's issuer, a confidential
/// client, and a test user the realm's password grant accepts.
pub const LIVE_OIDC_ENV: [&str; 5] = [
    "QUANSIO_TEST_OIDC_ISSUER",
    "QUANSIO_TEST_OIDC_CLIENT_ID",
    "QUANSIO_TEST_OIDC_CLIENT_SECRET",
    "QUANSIO_TEST_OIDC_USERNAME",
    "QUANSIO_TEST_OIDC_PASSWORD",
];

/// `config/identity.yaml`, as loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnterpriseConfig {
    /// SSO request lifetime.
    pub sso_state_ttl: Duration,
    /// How long discovery and JWKS are reused.
    pub metadata_cache: Duration,
    /// Allowed clock difference, seconds.
    pub clock_skew_seconds: i64,
    /// Timeout of each IdP request.
    pub http_timeout: Duration,
    /// Whether `http://` loopback issuers are admitted (development and tests only).
    pub allow_loopback_http_issuers: bool,
    /// argon2id memory, KiB.
    pub argon2_memory_kib: u32,
    /// argon2id passes.
    pub argon2_iterations: u32,
    /// argon2id lanes.
    pub argon2_parallelism: u32,
    /// Command families a principal can never hold.
    pub ungrantable_scopes: Vec<String>,
    /// Largest SCIM page.
    pub scim_max_results: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityFile {
    version: u32,
    sso: SsoSection,
    service_principals: PrincipalSection,
    scim: ScimSection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SsoSection {
    state_ttl_seconds: u64,
    metadata_cache_seconds: u64,
    clock_skew_seconds: i64,
    http_timeout_seconds: u64,
    allow_loopback_http_issuers: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalSection {
    argon2_memory_kib: u32,
    argon2_iterations: u32,
    argon2_parallelism: u32,
    ungrantable_scopes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScimSection {
    max_results: usize,
}

impl EnterpriseConfig {
    /// Load `config/identity.yaml` from the repository/deployment root.
    ///
    /// # Errors
    /// Returns a message when the file is missing or malformed.
    pub fn load() -> Result<Self, String> {
        let path =
            super::oauth::locate("config/identity.yaml").ok_or("config/identity.yaml not found")?;
        Self::load_from(&path)
    }

    /// Load a specific file.
    ///
    /// # Errors
    /// Returns a message when the file is missing, malformed or out of bounds.
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let file: IdentityFile =
            serde_yaml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        if file.version != 1 {
            return Err(format!(
                "{}: unsupported version {}",
                path.display(),
                file.version
            ));
        }
        if !(1..=3600).contains(&file.sso.state_ttl_seconds)
            || !(0..=600).contains(&file.sso.clock_skew_seconds)
            || !(1..=120).contains(&file.sso.http_timeout_seconds)
            || file.service_principals.argon2_memory_kib < 8 * 1024
            || file.service_principals.argon2_iterations == 0
            || file.service_principals.argon2_parallelism == 0
            || !(1..=1000).contains(&file.scim.max_results)
        {
            return Err(format!("{}: a value is out of bounds", path.display()));
        }
        Ok(Self {
            sso_state_ttl: Duration::from_secs(file.sso.state_ttl_seconds),
            metadata_cache: Duration::from_secs(file.sso.metadata_cache_seconds),
            clock_skew_seconds: file.sso.clock_skew_seconds,
            http_timeout: Duration::from_secs(file.sso.http_timeout_seconds),
            allow_loopback_http_issuers: file.sso.allow_loopback_http_issuers,
            argon2_memory_kib: file.service_principals.argon2_memory_kib,
            argon2_iterations: file.service_principals.argon2_iterations,
            argon2_parallelism: file.service_principals.argon2_parallelism,
            ungrantable_scopes: file.service_principals.ungrantable_scopes,
            scim_max_results: file.scim.max_results,
        })
    }
}

/// Refusals of enterprise identity. The `Display` prefixes are stable: a refusal raised inside a
/// mutation travels as `MutationRejected` text and is typed again by [`unwrap_rejection`].
#[derive(Debug, thiserror::Error)]
pub enum EnterpriseError {
    /// A request that does not meet the contract.
    #[error("enterprise invalid: {0}")]
    Invalid(String),
    /// The entity is not in this tenant.
    #[error("enterprise not found: {0}")]
    NotFound(String),
    /// The change contradicts the current state.
    #[error("enterprise conflict: {0}")]
    Conflict(String),
    /// A credential or assertion that does not verify.
    #[error("enterprise unauthenticated: {0}")]
    Unauthenticated(String),
    /// A verified caller that may not do this.
    #[error("enterprise forbidden: {0}")]
    Forbidden(String),
    /// The IdP could not be reached or answered outside the protocol.
    #[error("enterprise unavailable: {0}")]
    Unavailable(String),
    /// A mutation found nothing to change (never surfaced: the caller answers as a success).
    #[error("enterprise unchanged")]
    Unchanged,
    /// Session issuance failed.
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// The identity owner refused.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// The event store refused the commit.
    #[error("enterprise event store: {0}")]
    Event(EventError),
    /// Database failure.
    #[error("enterprise database: {0}")]
    Database(#[from] sqlx::Error),
}

impl EnterpriseError {
    /// The DOMAIN.md §15 code this refusal maps to.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT_STATE",
            Self::Unauthenticated(_) => "AUTH_INVALID_TOKEN",
            Self::Forbidden(_) => "SCOPE_FORBIDDEN",
            Self::Unavailable(_) => "PROVIDER_UNAVAILABLE",
            Self::Auth(AuthError::InvalidToken(_)) => "AUTH_INVALID_TOKEN",
            Self::Auth(AuthError::Expired(_)) => "AUTH_EXPIRED",
            Self::Identity(error) => error.code(),
            Self::Auth(_) | Self::Event(_) | Self::Database(_) | Self::Unchanged => "INTERNAL",
        }
    }
}

/// A refusal raised inside a mutation closure.
pub(crate) fn rejected(error: EnterpriseError) -> EventError {
    EventError::MutationRejected {
        owner: ENTERPRISE_OWNER,
        message: error.to_string(),
    }
}

/// Give a refusal that crossed the event store its type back.
pub(crate) fn unwrap_rejection(error: EventError) -> EnterpriseError {
    match error {
        EventError::MutationRejected { message, .. } => {
            let strip = |prefix: &str| message.strip_prefix(prefix).map(str::to_string);
            if let Some(rest) = strip("enterprise invalid: ") {
                EnterpriseError::Invalid(rest)
            } else if let Some(rest) = strip("enterprise not found: ") {
                EnterpriseError::NotFound(rest)
            } else if let Some(rest) = strip("enterprise conflict: ") {
                EnterpriseError::Conflict(rest)
            } else if let Some(rest) = strip("enterprise forbidden: ") {
                EnterpriseError::Forbidden(rest)
            } else if let Some(rest) = strip("enterprise unauthenticated: ") {
                EnterpriseError::Unauthenticated(rest)
            } else if message == "enterprise unchanged" {
                EnterpriseError::Unchanged
            } else if let Some(rest) = strip("identity conflict: ") {
                EnterpriseError::Conflict(rest)
            } else {
                EnterpriseError::Invalid(message)
            }
        }
        other => EnterpriseError::Event(other),
    }
}

/// Who is acting: an administrator's session, a service principal, or the SSO flow itself.
#[derive(Debug, Clone)]
pub struct Acting {
    /// Event/audit actor.
    pub actor: Actor,
    /// Request correlation.
    pub correlation_id: CorrelationId,
}

impl Acting {
    /// A service principal acting through SCIM or the command surface.
    #[must_use]
    pub fn service(principal_id: &str) -> Self {
        Self {
            actor: Actor::service(principal_id),
            correlation_id: CorrelationId::generate(&mut UlidGenerator::new()),
        }
    }

    /// The runtime acting on an IdP's verified assertion.
    #[must_use]
    pub fn sso() -> Self {
        Self {
            actor: Actor::system("sso"),
            correlation_id: CorrelationId::generate(&mut UlidGenerator::new()),
        }
    }
}

impl From<&MemberAdmin> for Acting {
    fn from(admin: &MemberAdmin) -> Self {
        Self {
            actor: admin.actor.clone(),
            correlation_id: admin.correlation_id,
        }
    }
}

/// Stage one RuntimeEvent on `aggregate_type/aggregate_id` inside `tx`, versioned after the
/// aggregate's existing events. A mutation stages at most one event per aggregate.
pub(crate) async fn stage_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    tenant_id: &str,
    acting: &Acting,
    aggregate: (&str, &str),
    event_type: &str,
    payload: Value,
) -> Result<(), EventError> {
    let (aggregate_type, aggregate_id) = aggregate;
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 \
         AND aggregate_type = $2 AND aggregate_id = $3",
    )
    .bind(tenant_id)
    .bind(aggregate_type)
    .bind(aggregate_id)
    .fetch_one(&mut **tx)
    .await?;
    let version = u64::try_from(existing).unwrap_or(0).saturating_add(1);
    batch.emit(
        EventDraft::new(
            aggregate_type,
            aggregate_id,
            version,
            EventType::parse(event_type)?,
            acting.correlation_id,
            acting.actor.clone(),
        )
        .with_payload(payload),
    );
    Ok(())
}

/// Stage a `member.*` event on the membership aggregate.
pub(crate) async fn stage_member_in(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    tenant_id: &str,
    acting: &Acting,
    membership_id: &str,
    event_type: &str,
    payload: Value,
) -> Result<(), EventError> {
    stage_in(
        tx,
        batch,
        tenant_id,
        acting,
        ("member", membership_id),
        event_type,
        payload,
    )
    .await
}

/// Append the AuditEntry for a decision inside `tx`.
pub(crate) async fn audit_in(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    acting: &Acting,
    action: &str,
    target_ref: &str,
    decision: &str,
    reason: &str,
) -> Result<(), sqlx::Error> {
    append_in(
        tx,
        tenant_id,
        AuditDraft {
            workspace_id: None,
            actor: serde_json::to_value(&acting.actor)
                .unwrap_or_else(|_| json!({ "kind": "system", "id": "identity" })),
            action: action.to_string(),
            target_ref: Some(target_ref.to_string()),
            decision: Some(decision.to_string()),
            reason: Some(reason.to_string()),
            correlation_id: Some(acting.correlation_id.to_string()),
        },
    )
    .await
    .map(|_| ())
}

/// The account for `email`, whatever tenant it belongs to (id only; `identity_user_by_email`).
pub(crate) async fn user_by_email_in(
    tx: &mut Transaction<'_, Postgres>,
    email: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT identity_user_by_email($1)")
        .bind(email)
        .fetch_one(&mut **tx)
        .await
}

/// Create a user inside `tx`; the users policy admits an insert only for the acting user, so the
/// new user is made the actor of their own provisioning.
pub(crate) async fn create_user_in(
    tx: &mut Transaction<'_, Postgres>,
    email: &str,
    display_name: &str,
    method: &str,
) -> Result<String, EventError> {
    let user_id = CanonicalId::generate(Prefix::User, &mut UlidGenerator::new()).to_string();
    schema::set_user_context(tx, &user_id)
        .await
        .map_err(|error| rejected(EnterpriseError::Invalid(error.to_string())))?;
    sqlx::query(
        "INSERT INTO users (id, primary_email, display_name, auth_methods, settings) \
         VALUES ($1, $2, $3, $4, '{}'::jsonb)",
    )
    .bind(&user_id)
    .bind(email)
    .bind(display_name)
    .bind(json!([{ "method": method }]))
    .execute(&mut **tx)
    .await?;
    Ok(user_id)
}

/// A cached issuer: discovery and JWKS, with when they were fetched.
type IssuerCache = Arc<Mutex<HashMap<String, oidc::CachedIssuer>>>;

/// The enterprise identity owner.
#[derive(Clone)]
pub struct EnterpriseIdentity {
    pool: PgPool,
    auth: AuthStore,
    events: EventStore,
    config: Arc<EnterpriseConfig>,
    http: reqwest::Client,
    issuers: IssuerCache,
    verified_keys: Arc<Mutex<HashMap<String, (String, String)>>>,
}

impl std::fmt::Debug for EnterpriseIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnterpriseIdentity")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl EnterpriseIdentity {
    /// Bind to the session authority with `config`.
    ///
    /// # Errors
    /// Returns a message when the HTTP client cannot be built.
    pub fn new(auth: AuthStore, config: EnterpriseConfig) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("enterprise identity HTTP client: {error}"))?;
        Ok(Self {
            pool: auth.pool().clone(),
            events: EventStore::new(auth.pool().clone()),
            auth,
            config: Arc::new(config),
            http,
            issuers: Arc::new(Mutex::new(HashMap::new())),
            verified_keys: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The configuration in force.
    #[must_use]
    pub fn config(&self) -> &EnterpriseConfig {
        &self.config
    }

    pub(crate) fn events(&self) -> &EventStore {
        &self.events
    }

    /// A transaction with the pre-authentication gate open and, when known, the tenant context set.
    pub(crate) async fn auth_transaction(
        &self,
        tenant_id: Option<&str>,
    ) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('quansio.auth_flow', 'on', true)")
            .execute(&mut *tx)
            .await?;
        if let Some(tenant_id) = tenant_id {
            sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
                .bind(tenant_id)
                .execute(&mut *tx)
                .await?;
        }
        Ok(tx)
    }

    /// A transaction in the tenant's context (row-level security in force for that tenant).
    pub(crate) async fn tenant_transaction(
        &self,
        tenant_id: &str,
    ) -> Result<Transaction<'static, Postgres>, EnterpriseError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, tenant_id)
            .await
            .map_err(|error| EnterpriseError::Invalid(error.to_string()))?;
        Ok(tx)
    }
}
