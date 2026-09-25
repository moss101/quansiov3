//! OIDC single sign-on per tenant (OPS-001, DOMAIN.md §2).
//!
//! **Connection.** A tenant administrator configures one connection: issuer, client id, an
//! optional client-secret custody handle, the groups claim, a default workspace and whether SSO
//! is enforced. Setting a group mapping recomputes every linked member's roles at once, so a
//! mapping takes effect without waiting for the next sign-in.
//!
//! **Discovery and keys.** The issuer's discovery document is read at
//! `<issuer>/.well-known/openid-configuration`, and its `issuer` must equal the configured one.
//! Discovery and JWKS are cached; the JWKS is fetched again when a token names an unknown `kid`
//! (key rotation). Every IdP URL must be `https`. Plain `http` is admitted only for loopback hosts
//! and only when `allow_loopback_http_issuers` is set, so a tenant-configured issuer cannot make
//! the server call its own loopback services. Redirects are not followed.
//!
//! **Start.** Mints `state`, `nonce` and a PKCE S256 verifier. State and nonce are stored hashed,
//! the verifier stays on the server, and the row is consumed once.
//!
//! **Complete.**
//! 1. Consume the request row before anything else, so a replayed state fails even if the
//!    exchange would not.
//! 2. Exchange the code and verify the id_token ([`super::jwt`]).
//! 3. Link or provision the account, apply the server-side group mapping ([`super::mapping`]) and
//!    issue a tenant-bound `sso` session.
//!
//! **Linking rules.**
//! - A linked `(issuer, subject)` is its user.
//! - An unknown address is provisioned.
//! - An account this tenant provisioned through SCIM links on its first sign-in.
//! - Any other existing account links only when the request carries that same user's bearer
//!   session. The IdP alone never takes over an account it did not create.
//! - A suspended or removed membership cannot sign in; only SCIM or an administrator restores it.

use std::time::Instant;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use reqwest::Url;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};

use super::jwt::{self, Expectations, Jwks, JwtError};
use super::mapping;
use super::{
    audit_in, create_user_in, rejected, stage_in, stage_member_in, user_by_email_in, Acting,
    EnterpriseError, EnterpriseIdentity,
};
use crate::control::identity::auth::{normalize_email, IssuedSession, SessionScope, SignedInUser};
use crate::control::schema;
use crate::policy::rbac::{TenantRole, WorkspaceRole};

/// The most an IdP document may weigh.
const MAX_DOCUMENT_BYTES: usize = 512 * 1024;
/// The shortest interval between two forced JWKS fetches for one issuer.
const JWKS_REFETCH_FLOOR: std::time::Duration = std::time::Duration::from_secs(5);

/// A connection as an administrator sets it (`ConfigureSso`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsoSettings {
    /// The IdP issuer.
    pub issuer: String,
    /// The client id registered at the IdP.
    pub client_id: String,
    /// Custody handle of the client secret (public PKCE clients have none).
    pub client_secret_handle: Option<String>,
    /// The id_token claim that lists the user's groups.
    pub groups_claim: String,
    /// Where a user no mapping places lands (as `viewer`).
    pub default_workspace_id: Option<String>,
    /// Whether other sign-in methods are refused to non-owners.
    pub enforced: bool,
    /// `active` | `disabled`.
    pub status: String,
}

/// The start of an SSO sign-in.
#[derive(Debug, Clone)]
pub struct SsoStart {
    /// Where the client sends the user.
    pub authorization_url: String,
    /// The state the IdP echoes back.
    pub state: String,
    /// When the request stops being valid.
    pub expires_at: DateTime<Utc>,
}

/// The parts of the discovery document this flow uses.
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    /// Must equal the configured issuer.
    pub issuer: String,
    /// Where the user is sent.
    pub authorization_endpoint: String,
    /// Where the code is exchanged.
    pub token_endpoint: String,
    /// The signing keys.
    pub jwks_uri: String,
    /// Client authentication methods the token endpoint accepts.
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,
}

/// An issuer's cached metadata.
#[derive(Debug, Clone)]
pub struct CachedIssuer {
    discovery: Discovery,
    jwks: Jwks,
    fetched_at: Instant,
    jwks_fetched_at: Instant,
}

/// A loaded connection.
#[derive(Debug, Clone)]
struct Connection {
    issuer: String,
    client_id: String,
    client_secret_handle: Option<String>,
    groups_claim: String,
    default_workspace_id: Option<String>,
}

/// The connection row: settings plus timestamps.
type ConnectionRow = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    bool,
    String,
    String,
);

/// The active connection's row: issuer, client id, secret handle, groups claim, default workspace.
type ActiveConnectionRow = (String, String, Option<String>, String, Option<String>);

/// The environment variable that materializes a custody handle (the model gateway's rule).
#[must_use]
pub fn custody_env(handle: &str) -> String {
    let normalized: String = handle
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("QUANSIO_SECRET_{normalized}")
}

fn random_token(rng: &SystemRandom) -> Result<String, EnterpriseError> {
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .map_err(|_| EnterpriseError::Invalid("platform RNG failed".to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn is_loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

/// A redirect back to the client: `https`, a loopback `http` port on the user's machine
/// (RFC 8252 native apps), or the desktop's `quansio:` scheme.
fn check_redirect(redirect_uri: &str) -> Result<(), EnterpriseError> {
    let url = Url::parse(redirect_uri)
        .map_err(|_| EnterpriseError::Invalid("redirect_uri is not a URL".to_string()))?;
    let admitted = match url.scheme() {
        "https" | "quansio" => true,
        "http" => is_loopback(&url),
        _ => false,
    };
    if !admitted || url.fragment().is_some() {
        return Err(EnterpriseError::Invalid(
            "redirect_uri must be https, a loopback http port, or quansio:".to_string(),
        ));
    }
    Ok(())
}

fn groups_of(claims: &serde_json::Map<String, Value>, claim: &str) -> Vec<String> {
    match claims.get(claim) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    }
}

impl EnterpriseIdentity {
    fn admissible(&self, raw: &str, what: &str) -> Result<Url, EnterpriseError> {
        let url = Url::parse(raw)
            .map_err(|_| EnterpriseError::Invalid(format!("{what} is not a URL")))?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(EnterpriseError::Invalid(format!(
                "{what} must not carry credentials or a fragment"
            )));
        }
        match url.scheme() {
            "https" => Ok(url),
            "http" if self.config.allow_loopback_http_issuers && is_loopback(&url) => Ok(url),
            _ => Err(EnterpriseError::Invalid(format!(
                "{what} must be an https URL"
            ))),
        }
    }

    async fn fetch_json(&self, url: Url, what: &str) -> Result<Value, EnterpriseError> {
        let host = url.host_str().unwrap_or("the IdP").to_string();
        let response = self
            .http
            .get(url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|_| {
                EnterpriseError::Unavailable(format!("{what} at {host} could not be reached"))
            })?;
        if !response.status().is_success() {
            return Err(EnterpriseError::Unavailable(format!(
                "{what} at {host} answered {}",
                response.status().as_u16()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| EnterpriseError::Unavailable(format!("{what} at {host} was cut off")))?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(EnterpriseError::Unavailable(format!(
                "{what} at {host} is too large"
            )));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| EnterpriseError::Unavailable(format!("{what} at {host} is not JSON")))
    }

    /// The issuer's discovery and JWKS, from the cache when fresh. `refresh_jwks` fetches the keys
    /// again (a token named a `kid` the cached set lacks).
    async fn issuer_metadata(
        &self,
        issuer: &str,
        refresh_jwks: bool,
    ) -> Result<CachedIssuer, EnterpriseError> {
        let cached = self
            .issuers
            .lock()
            .ok()
            .and_then(|cache| cache.get(issuer).cloned());
        let fresh = cached
            .as_ref()
            .filter(|entry| entry.fetched_at.elapsed() < self.config.metadata_cache);
        if let Some(entry) = fresh {
            if !refresh_jwks || entry.jwks_fetched_at.elapsed() < JWKS_REFETCH_FLOOR {
                return Ok(entry.clone());
            }
        }
        let issuer_url = self.admissible(issuer, "issuer")?;
        let discovery = match fresh {
            Some(entry) => entry.discovery.clone(),
            None => {
                let mut url = issuer_url.clone();
                let path = format!(
                    "{}/.well-known/openid-configuration",
                    url.path().trim_end_matches('/')
                );
                url.set_path(&path);
                let document = self.fetch_json(url, "the discovery document").await?;
                let discovery: Discovery = serde_json::from_value(document).map_err(|_| {
                    EnterpriseError::Unavailable(
                        "the discovery document lacks the OIDC endpoints".to_string(),
                    )
                })?;
                if discovery.issuer != issuer {
                    return Err(EnterpriseError::Invalid(
                        "the discovery document names another issuer".to_string(),
                    ));
                }
                self.admissible(&discovery.authorization_endpoint, "authorization_endpoint")?;
                self.admissible(&discovery.token_endpoint, "token_endpoint")?;
                self.admissible(&discovery.jwks_uri, "jwks_uri")?;
                discovery
            }
        };
        let jwks_url = self.admissible(&discovery.jwks_uri, "jwks_uri")?;
        let jwks: Jwks = serde_json::from_value(self.fetch_json(jwks_url, "the JWKS").await?)
            .map_err(|_| EnterpriseError::Unavailable("the JWKS is not a key set".to_string()))?;
        let now = Instant::now();
        let entry = CachedIssuer {
            fetched_at: fresh.map_or(now, |entry| entry.fetched_at),
            discovery,
            jwks,
            jwks_fetched_at: now,
        };
        if let Ok(mut cache) = self.issuers.lock() {
            cache.insert(issuer.to_string(), entry.clone());
        }
        Ok(entry)
    }

    /// `ConfigureSso`: create or replace the tenant's connection. An active connection's issuer is
    /// checked live (discovery and keys) before it is saved; turning enforcement on revokes the
    /// non-SSO sessions of every non-owner.
    ///
    /// # Errors
    /// [`EnterpriseError::Invalid`] for a malformed setting, [`EnterpriseError::Unavailable`]
    /// when the issuer cannot be read.
    pub async fn configure_sso(
        &self,
        tenant_id: &str,
        acting: &Acting,
        settings: SsoSettings,
    ) -> Result<Value, EnterpriseError> {
        self.admissible(&settings.issuer, "issuer")?;
        if settings.client_id.trim().is_empty() || settings.client_id.len() > 512 {
            return Err(EnterpriseError::Invalid(
                "client_id must be 1 to 512 characters".to_string(),
            ));
        }
        if settings.groups_claim.is_empty()
            || settings.groups_claim.len() > 128
            || !settings
                .groups_claim
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:/".contains(&b))
        {
            return Err(EnterpriseError::Invalid(
                "groups_claim is not a claim name".to_string(),
            ));
        }
        if let Some(handle) = &settings.client_secret_handle {
            // A custody handle is `<namespace>/<name>` in lower case (`sso/acme`), the model
            // gateway's convention; anything else, or anything shaped like key material, is refused.
            let shaped = handle.len() <= 128
                && handle.split_once('/').is_some_and(|(namespace, name)| {
                    !namespace.is_empty()
                        && !name.is_empty()
                        && namespace.starts_with(|c: char| c.is_ascii_lowercase())
                })
                && handle
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"/_-".contains(&b))
                && !crate::control::identity::settings::looks_like_secret(handle);
            if !shaped {
                return Err(EnterpriseError::Invalid(
                    "client_secret_handle must be a custody handle such as sso/acme, never the secret"
                        .to_string(),
                ));
            }
        }
        if !matches!(settings.status.as_str(), "active" | "disabled") {
            return Err(EnterpriseError::Invalid(
                "status must be active or disabled".to_string(),
            ));
        }
        if settings.status == "active" {
            self.issuer_metadata(&settings.issuer, false).await?;
        }
        let tenant = tenant_id.to_string();
        let acting = acting.clone();
        self.events()
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    if let Some(workspace) = &settings.default_workspace_id {
                        let known: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM workspaces WHERE tenant_id = $1 AND id = $2)",
                        )
                        .bind(&tenant)
                        .bind(workspace)
                        .fetch_one(&mut **tx)
                        .await?;
                        if !known {
                            return Err(rejected(EnterpriseError::Invalid(format!(
                                "workspace {workspace} is not in this tenant"
                            ))));
                        }
                    }
                    let was_enforced: bool = sqlx::query_scalar(
                        "SELECT COALESCE((SELECT enforced AND status = 'active' FROM sso_connections \
                         WHERE tenant_id = $1), FALSE)",
                    )
                    .bind(&tenant)
                    .fetch_one(&mut **tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO sso_connections (tenant_id, protocol, issuer, client_id, \
                         client_secret_handle, groups_claim, default_workspace_id, enforced, status) \
                         VALUES ($1, 'oidc', $2, $3, $4, $5, $6, $7, $8) \
                         ON CONFLICT (tenant_id) DO UPDATE SET issuer = EXCLUDED.issuer, \
                         client_id = EXCLUDED.client_id, client_secret_handle = EXCLUDED.client_secret_handle, \
                         groups_claim = EXCLUDED.groups_claim, default_workspace_id = EXCLUDED.default_workspace_id, \
                         enforced = EXCLUDED.enforced, status = EXCLUDED.status",
                    )
                    .bind(&tenant)
                    .bind(&settings.issuer)
                    .bind(&settings.client_id)
                    .bind(&settings.client_secret_handle)
                    .bind(&settings.groups_claim)
                    .bind(&settings.default_workspace_id)
                    .bind(settings.enforced)
                    .bind(&settings.status)
                    .execute(&mut **tx)
                    .await?;
                    let now_enforced = settings.enforced && settings.status == "active";
                    let mut revoked = 0;
                    if now_enforced && !was_enforced {
                        revoked = sqlx::query(
                            "UPDATE sessions s SET revoked_at = now(), \
                             revoked_reason = 'single sign-on is now required' \
                             WHERE s.tenant_id = $1 AND s.revoked_at IS NULL AND s.auth_method <> 'sso' \
                             AND NOT EXISTS (SELECT 1 FROM tenant_memberships m WHERE m.tenant_id = s.tenant_id \
                             AND m.user_id = s.user_id AND m.role = 'owner' AND m.status = 'active')",
                        )
                        .bind(&tenant)
                        .execute(&mut **tx)
                        .await?
                        .rows_affected();
                    }
                    stage_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        ("tenant", &tenant),
                        "tenant.sso_configured",
                        json!({
                            "issuer": settings.issuer,
                            "enforced": settings.enforced,
                            "status": settings.status,
                            "revoked_sessions": revoked,
                        }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        "ConfigureSso",
                        &format!("sso:{tenant}"),
                        "applied",
                        &format!(
                            "issuer={} enforced={} status={} revoked_sessions={revoked}",
                            settings.issuer, settings.enforced, settings.status
                        ),
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        self.sso_connection(tenant_id)
            .await?
            .ok_or_else(|| EnterpriseError::NotFound("the connection was not saved".to_string()))
    }

    /// `GET /v1/sso`: the connection and its mappings (a custody handle, never material).
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn sso_connection(&self, tenant_id: &str) -> Result<Option<Value>, EnterpriseError> {
        let mut tx = self.tenant_transaction(tenant_id).await?;
        let row: Option<ConnectionRow> = sqlx::query_as(
            "SELECT issuer, client_id, client_secret_handle, groups_claim, default_workspace_id, \
             enforced, status, protocol FROM sso_connections WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let mappings = mapping::load_in(&mut tx, tenant_id).await?;
        tx.commit().await?;
        Ok(row.map(
            |(
                issuer,
                client_id,
                handle,
                groups_claim,
                default_workspace,
                enforced,
                status,
                protocol,
            )| {
                json!({
                    "tenant_id": tenant_id,
                    "protocol": protocol,
                    "issuer": issuer,
                    "client_id": client_id,
                    "client_secret_handle": handle,
                    "groups_claim": groups_claim,
                    "default_workspace_id": default_workspace,
                    "enforced": enforced,
                    "status": status,
                    "mappings": mappings.iter().map(|m| json!({
                        "external_group": m.external_group,
                        "workspace_id": (!m.scope.is_empty()).then_some(&m.scope),
                        "role": m.tenant_role.map(TenantRole::as_str)
                            .or(m.workspace_role.map(WorkspaceRole::as_str)),
                    })).collect::<Vec<_>>(),
                })
            },
        ))
    }

    async fn connection(&self, tenant_id: &str) -> Result<Connection, EnterpriseError> {
        let mut tx = self.tenant_transaction(tenant_id).await?;
        let row: Option<ActiveConnectionRow> = sqlx::query_as(
            "SELECT issuer, client_id, client_secret_handle, groups_claim, default_workspace_id \
             FROM sso_connections WHERE tenant_id = $1 AND status = 'active'",
        )
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let (issuer, client_id, client_secret_handle, groups_claim, default_workspace_id) = row
            .ok_or_else(|| {
                EnterpriseError::NotFound(
                    "this organization has no active single sign-on".to_string(),
                )
            })?;
        Ok(Connection {
            issuer,
            client_id,
            client_secret_handle,
            groups_claim,
            default_workspace_id,
        })
    }

    /// `SetGroupRoleMapping {external_group, workspace_id?, role}`: bind an IdP group to a tenant
    /// role (no workspace) or a workspace role, and recompute every linked member's roles.
    ///
    /// # Errors
    /// [`EnterpriseError::Invalid`] for an unknown role, `owner`, or a workspace not in the tenant.
    pub async fn set_group_role_mapping(
        &self,
        tenant_id: &str,
        acting: &Acting,
        external_group: &str,
        workspace_id: Option<&str>,
        role: &str,
    ) -> Result<Value, EnterpriseError> {
        let group = external_group.trim().to_string();
        if group.is_empty() || group.len() > 512 {
            return Err(EnterpriseError::Invalid(
                "external_group must be 1 to 512 characters".to_string(),
            ));
        }
        let (tenant_role, workspace_role) = match workspace_id {
            None => {
                let parsed = TenantRole::parse(role).map_err(EnterpriseError::Invalid)?;
                if parsed == TenantRole::Owner {
                    return Err(EnterpriseError::Invalid(
                        "owner is never granted by a group mapping".to_string(),
                    ));
                }
                (Some(parsed.as_str()), None)
            }
            Some(_) => (
                None,
                Some(
                    WorkspaceRole::parse(role)
                        .map_err(EnterpriseError::Invalid)?
                        .as_str(),
                ),
            ),
        };
        let scope = workspace_id.unwrap_or_default().to_string();
        let tenant = tenant_id.to_string();
        let acting = acting.clone();
        let (group_row, scope_row) = (group.clone(), scope.clone());
        let recomputed = self
            .events()
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    if !scope_row.is_empty() {
                        let known: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM workspaces WHERE tenant_id = $1 AND id = $2)",
                        )
                        .bind(&tenant)
                        .bind(&scope_row)
                        .fetch_one(&mut **tx)
                        .await?;
                        if !known {
                            return Err(rejected(EnterpriseError::Invalid(format!(
                                "workspace {scope_row} is not in this tenant"
                            ))));
                        }
                    }
                    sqlx::query(
                        "INSERT INTO sso_group_mappings (tenant_id, external_group, scope, tenant_role, workspace_role) \
                         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, external_group, scope) \
                         DO UPDATE SET tenant_role = EXCLUDED.tenant_role, workspace_role = EXCLUDED.workspace_role",
                    )
                    .bind(&tenant)
                    .bind(&group_row)
                    .bind(&scope_row)
                    .bind(tenant_role)
                    .bind(workspace_role)
                    .execute(&mut **tx)
                    .await?;
                    let recomputed = recompute_all_in(tx, &tenant).await?;
                    stage_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        ("tenant", &tenant),
                        "tenant.group_mapping_set",
                        json!({ "external_group": group_row, "workspace_id": (!scope_row.is_empty()).then_some(&scope_row),
                                "role": tenant_role.or(workspace_role), "members_changed": recomputed }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        "SetGroupRoleMapping",
                        &format!("group:{group_row}"),
                        "applied",
                        &format!("{} -> {}", if scope_row.is_empty() { "tenant" } else { scope_row.as_str() },
                                 tenant_role.or(workspace_role).unwrap_or_default()),
                    )
                    .await?;
                    Ok(recomputed)
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        Ok(json!({
            "external_group": group,
            "workspace_id": workspace_id,
            "role": role,
            "members_changed": recomputed,
        }))
    }

    /// `RemoveGroupRoleMapping {external_group, workspace_id?}`.
    ///
    /// # Errors
    /// [`EnterpriseError::NotFound`] when no such mapping exists.
    pub async fn remove_group_role_mapping(
        &self,
        tenant_id: &str,
        acting: &Acting,
        external_group: &str,
        workspace_id: Option<&str>,
    ) -> Result<Value, EnterpriseError> {
        let tenant = tenant_id.to_string();
        let acting = acting.clone();
        let group = external_group.trim().to_string();
        let scope = workspace_id.unwrap_or_default().to_string();
        let recomputed = self
            .events()
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let removed = sqlx::query(
                        "DELETE FROM sso_group_mappings WHERE tenant_id = $1 AND external_group = $2 AND scope = $3",
                    )
                    .bind(&tenant)
                    .bind(&group)
                    .bind(&scope)
                    .execute(&mut **tx)
                    .await?;
                    if removed.rows_affected() == 0 {
                        return Err(rejected(EnterpriseError::NotFound(format!(
                            "no mapping for group {group:?} at that scope"
                        ))));
                    }
                    let recomputed = recompute_all_in(tx, &tenant).await?;
                    stage_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        ("tenant", &tenant),
                        "tenant.group_mapping_removed",
                        json!({ "external_group": group, "workspace_id": (!scope.is_empty()).then_some(&scope),
                                "members_changed": recomputed }),
                    )
                    .await?;
                    audit_in(tx, &tenant, &acting, "RemoveGroupRoleMapping", &format!("group:{group}"), "applied", "removed")
                        .await?;
                    Ok(recomputed)
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        Ok(
            json!({ "external_group": external_group, "workspace_id": workspace_id, "members_changed": recomputed }),
        )
    }

    /// `GET /v1/auth/sso/{tenant_id}/start`.
    ///
    /// # Errors
    /// [`EnterpriseError::NotFound`] without an active connection, [`EnterpriseError::Invalid`]
    /// for a redirect that is not admitted, [`EnterpriseError::Unavailable`] when the IdP is down.
    pub async fn start_sso(
        &self,
        tenant_id: &str,
        redirect_uri: &str,
    ) -> Result<SsoStart, EnterpriseError> {
        check_redirect(redirect_uri)?;
        let connection = self.connection(tenant_id).await?;
        let issuer = self.issuer_metadata(&connection.issuer, false).await?;
        let rng = SystemRandom::new();
        let state = random_token(&rng)?;
        let nonce = random_token(&rng)?;
        let verifier = random_token(&rng)?;
        let challenge = URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()));
        let expires_at = Utc::now()
            + chrono::Duration::from_std(self.config.sso_state_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(10));
        let mut tx = self.auth_transaction(Some(tenant_id)).await?;
        sqlx::query("DELETE FROM sso_states WHERE expires_at < now() - interval '1 day'")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO sso_states (state_hash, tenant_id, nonce_hash, pkce_verifier, redirect_uri, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(jwt::sha256_hex(&state))
        .bind(tenant_id)
        .bind(jwt::sha256_hex(&nonce))
        .bind(&verifier)
        .bind(redirect_uri)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut url = self.admissible(
            &issuer.discovery.authorization_endpoint,
            "authorization_endpoint",
        )?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &connection.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", "openid email profile")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(SsoStart {
            authorization_url: url.to_string(),
            state,
            expires_at,
        })
    }

    /// `POST /v1/auth/sso/{tenant_id}/complete`: exchange, verify, link or provision, map, sign in.
    /// `linking` is the caller's own bearer session, when the request carries one.
    ///
    /// # Errors
    /// [`EnterpriseError::Unauthenticated`] for a state, code or id_token that does not verify,
    /// [`EnterpriseError::Conflict`] for an existing account the request may not link,
    /// [`EnterpriseError::Forbidden`] for a suspended membership or no workspace.
    pub async fn complete_sso(
        &self,
        tenant_id: &str,
        code: &str,
        state: &str,
        redirect_uri: &str,
        client: &str,
        linking: Option<&SessionScope>,
    ) -> Result<(IssuedSession, SignedInUser), EnterpriseError> {
        let connection = self.connection(tenant_id).await?;
        let mut tx = self.auth_transaction(Some(tenant_id)).await?;
        let row: Option<(String, String, String, DateTime<Utc>)> = sqlx::query_as(
            "UPDATE sso_states SET consumed_at = now() \
             WHERE state_hash = $1 AND tenant_id = $2 AND consumed_at IS NULL \
             RETURNING nonce_hash, pkce_verifier, redirect_uri, expires_at",
        )
        .bind(jwt::sha256_hex(state))
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some((nonce_hash, verifier, expected_redirect, expires_at)) = row else {
            return Err(EnterpriseError::Unauthenticated(
                "the sign-in request is unknown or was already used".to_string(),
            ));
        };
        if expires_at <= Utc::now() {
            return Err(EnterpriseError::Unauthenticated(
                "the sign-in request expired".to_string(),
            ));
        }
        if expected_redirect != redirect_uri {
            return Err(EnterpriseError::Unauthenticated(
                "redirect_uri is not the one the request started with".to_string(),
            ));
        }
        let issuer = self.issuer_metadata(&connection.issuer, false).await?;
        let id_token = self
            .exchange_code(
                &connection,
                &issuer.discovery,
                code,
                &verifier,
                redirect_uri,
            )
            .await?;
        let expect = Expectations {
            issuer: &connection.issuer,
            client_id: &connection.client_id,
            nonce_sha256: Some(&nonce_hash),
            now: Utc::now().timestamp(),
            skew: self.config.clock_skew_seconds,
        };
        let token = match jwt::verify(&id_token, &issuer.jwks, &expect) {
            Err(JwtError::UnknownKey(_)) => {
                let refreshed = self.issuer_metadata(&connection.issuer, true).await?;
                jwt::verify(&id_token, &refreshed.jwks, &expect)
            }
            other => other,
        }
        .map_err(|error| EnterpriseError::Unauthenticated(error.to_string()))?;
        if token.email_verified == Some(false) {
            return Err(EnterpriseError::Unauthenticated(
                "the IdP says this email address is not verified".to_string(),
            ));
        }
        let email = token
            .email
            .as_deref()
            .ok_or_else(|| {
                EnterpriseError::Unauthenticated("the IdP asserted no email address".to_string())
            })
            .and_then(|email| {
                normalize_email(email).map_err(|_| {
                    EnterpriseError::Unauthenticated(
                        "the asserted email is not an address".to_string(),
                    )
                })
            })?;
        let groups = groups_of(&token.claims, &connection.groups_claim);
        let display_name = token
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| email.split('@').next().unwrap_or("member").to_string());
        let signed_in = self
            .admit(
                tenant_id,
                &connection,
                Assertion {
                    subject: token.subject,
                    email,
                    display_name,
                    groups,
                },
                linking.map(|scope| scope.user_id.clone()),
            )
            .await?;
        let session = self
            .auth
            .issue_session(&signed_in.user_id, tenant_id, "sso", client)
            .await?;
        Ok((session, signed_in))
    }

    async fn exchange_code(
        &self,
        connection: &Connection,
        discovery: &Discovery,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<String, EnterpriseError> {
        let url = self.admissible(&discovery.token_endpoint, "token_endpoint")?;
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", redirect_uri.to_string()),
            ("client_id", connection.client_id.clone()),
            ("code_verifier", verifier.to_string()),
        ];
        let mut request = self.http.post(url).header("accept", "application/json");
        if let Some(handle) = &connection.client_secret_handle {
            let secret = std::env::var(custody_env(handle))
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    EnterpriseError::Unavailable(format!(
                        "the client secret for handle {handle} is not in custody"
                    ))
                })?;
            let methods = &discovery.token_endpoint_auth_methods_supported;
            if methods.is_empty() || methods.iter().any(|m| m == "client_secret_basic") {
                request = request.basic_auth(&connection.client_id, Some(secret));
            } else {
                form.push(("client_secret", secret));
            }
        }
        let response = request.form(&form).send().await.map_err(|_| {
            EnterpriseError::Unavailable("the token endpoint could not be reached".to_string())
        })?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let reason = body
                .get("error")
                .and_then(Value::as_str)
                .filter(|code| code.len() <= 64 && code.bytes().all(|b| b.is_ascii_graphic()))
                .unwrap_or("error");
            return Err(EnterpriseError::Unauthenticated(format!(
                "the IdP refused the authorization code ({reason})"
            )));
        }
        body.get("id_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                EnterpriseError::Unauthenticated("the IdP returned no id_token".to_string())
            })
    }

    /// Link or provision the asserted identity, apply the mapping and choose a workspace, in one
    /// transaction with its events and audit.
    async fn admit(
        &self,
        tenant_id: &str,
        connection: &Connection,
        assertion: Assertion,
        linking_user: Option<String>,
    ) -> Result<SignedInUser, EnterpriseError> {
        let tenant = tenant_id.to_string();
        let issuer = connection.issuer.clone();
        let default_workspace = connection.default_workspace_id.clone();
        let acting = Acting::sso();
        self.events()
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let (user_id, how) =
                        resolve_account(tx, &tenant, &issuer, &assertion, linking_user.as_deref()).await?;
                    schema::set_user_context(tx, &user_id)
                        .await
                        .map_err(|error| rejected(EnterpriseError::Invalid(error.to_string())))?;
                    let user_status: Option<String> =
                        sqlx::query_scalar("SELECT status FROM users WHERE id = $1")
                            .bind(&user_id)
                            .fetch_optional(&mut **tx)
                            .await?;
                    if user_status.as_deref() != Some("active") {
                        return Err(rejected(EnterpriseError::Forbidden(
                            "this account is not active".to_string(),
                        )));
                    }
                    let membership: Option<(String, String)> = sqlx::query_as(
                        "SELECT id, status FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2 FOR UPDATE",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let (membership_id, provisioned) = match membership {
                        Some((_, status)) if status == "suspended" || status == "removed" => {
                            return Err(rejected(EnterpriseError::Forbidden(format!(
                                "your access to this organization is {status}"
                            ))));
                        }
                        Some((id, status)) => {
                            if status == "invited" {
                                sqlx::query(
                                    "UPDATE tenant_memberships SET status = 'active', updated_at = now() WHERE id = $1",
                                )
                                .bind(&id)
                                .execute(&mut **tx)
                                .await?;
                                sqlx::query(
                                    "UPDATE workspace_memberships SET status = 'active', updated_at = now() \
                                     WHERE tenant_id = $1 AND user_id = $2 AND status = 'invited'",
                                )
                                .bind(&tenant)
                                .bind(&user_id)
                                .execute(&mut **tx)
                                .await?;
                            }
                            (id, false)
                        }
                        None => {
                            let id = mapping::membership_id("tm_");
                            sqlx::query(
                                "INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) \
                                 VALUES ($1, $2, $3, 'member', 'active')",
                            )
                            .bind(&id)
                            .bind(&tenant)
                            .bind(&user_id)
                            .execute(&mut **tx)
                            .await?;
                            (id, true)
                        }
                    };
                    sqlx::query(
                        "UPDATE external_identities SET sso_groups = $3 WHERE tenant_id = $1 AND user_id = $2",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .bind(json!(assertion.groups))
                    .execute(&mut **tx)
                    .await?;
                    let mappings = mapping::load_in(tx, &tenant).await?;
                    let groups = mapping::groups_in(tx, &tenant, &user_id).await?;
                    let resolved = mapping::resolve(&mappings, &groups);
                    let changes =
                        mapping::apply_in(tx, &tenant, &user_id, &resolved, default_workspace.as_deref()).await?;
                    let workspace: Option<String> = sqlx::query_scalar(
                        "SELECT wm.workspace_id FROM workspace_memberships wm \
                         JOIN workspaces w ON w.id = wm.workspace_id \
                         WHERE wm.tenant_id = $1 AND wm.user_id = $2 AND wm.status = 'active' \
                         ORDER BY (wm.workspace_id = $3) DESC, w.created_at ASC LIMIT 1",
                    )
                    .bind(&tenant)
                    .bind(&user_id)
                    .bind(default_workspace.as_deref().unwrap_or(""))
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some(workspace_id) = workspace else {
                        return Err(rejected(EnterpriseError::Forbidden(
                            "no workspace in this organization is open to your groups".to_string(),
                        )));
                    };
                    // Every sign-in is an event: a new member, a changed one, or a returning one.
                    let event = if provisioned {
                        "member.provisioned"
                    } else if changes.is_empty() {
                        "member.signed_in"
                    } else {
                        "member.updated"
                    };
                    stage_member_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        &membership_id,
                        event,
                        json!({
                            "membership_id": membership_id,
                            "user_id": user_id,
                            "source": "sso",
                            "changes": changes,
                        }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        "sso.sign_in",
                        &format!("user:{user_id}"),
                        "applied",
                        &format!("{how}; {} role change(s)", changes.len()),
                    )
                    .await?;
                    Ok(SignedInUser {
                        user_id,
                        tenant_id: tenant.clone(),
                        workspace_id,
                        onboarded: how == "provisioned",
                    })
                })
            })
            .await
            .map_err(super::unwrap_rejection)
    }
}

/// What the IdP asserted, once verified.
#[derive(Debug, Clone)]
struct Assertion {
    subject: String,
    email: String,
    display_name: String,
    groups: Vec<String>,
}

/// Apply the linking rules; returns the user and how it was reached.
async fn resolve_account(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
    issuer: &str,
    assertion: &Assertion,
    linking_user: Option<&str>,
) -> Result<(String, &'static str), quansio_events::EventError> {
    let linked: Option<String> = sqlx::query_scalar(
        "SELECT user_id FROM external_identities WHERE tenant_id = $1 AND issuer = $2 AND subject = $3",
    )
    .bind(tenant)
    .bind(issuer)
    .bind(&assertion.subject)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(user_id) = linked {
        return Ok((user_id, "signed in"));
    }
    let Some(user_id) = user_by_email_in(tx, &assertion.email).await? else {
        let user_id = create_user_in(tx, &assertion.email, &assertion.display_name, "sso").await?;
        sqlx::query(
            "INSERT INTO external_identities (tenant_id, user_id, issuer, subject, origin) \
             VALUES ($1, $2, $3, $4, 'provisioned')",
        )
        .bind(tenant)
        .bind(&user_id)
        .bind(issuer)
        .bind(&assertion.subject)
        .execute(&mut **tx)
        .await?;
        return Ok((user_id, "provisioned"));
    };
    let existing: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT subject, origin FROM external_identities WHERE tenant_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(&user_id)
    .fetch_optional(&mut **tx)
    .await?;
    match existing {
        Some((Some(_), _)) => Err(rejected(EnterpriseError::Conflict(
            "this account is linked to another identity of the organization's IdP".to_string(),
        ))),
        Some((None, origin)) if origin == "provisioned" => {
            sqlx::query(
                "UPDATE external_identities SET issuer = $3, subject = $4 WHERE tenant_id = $1 AND user_id = $2",
            )
            .bind(tenant)
            .bind(&user_id)
            .bind(issuer)
            .bind(&assertion.subject)
            .execute(&mut **tx)
            .await?;
            Ok((user_id, "linked a provisioned account"))
        }
        _ if linking_user == Some(user_id.as_str()) => {
            sqlx::query(
                "INSERT INTO external_identities (tenant_id, user_id, issuer, subject, origin) \
                 VALUES ($1, $2, $3, $4, 'linked') ON CONFLICT (tenant_id, user_id) \
                 DO UPDATE SET issuer = EXCLUDED.issuer, subject = EXCLUDED.subject",
            )
            .bind(tenant)
            .bind(&user_id)
            .bind(issuer)
            .bind(&assertion.subject)
            .execute(&mut **tx)
            .await?;
            Ok((user_id, "linked by its signed-in user"))
        }
        _ => Err(rejected(EnterpriseError::Conflict(
            "an account with this email already exists; sign in to it and complete single sign-on \
             from that session to link it"
                .to_string(),
        ))),
    }
}

/// Recompute every linked member's roles after a mapping change; returns how many changed.
async fn recompute_all_in(
    tx: &mut Transaction<'static, Postgres>,
    tenant: &str,
) -> Result<usize, sqlx::Error> {
    let users: Vec<String> = sqlx::query_scalar(
        "SELECT e.user_id FROM external_identities e JOIN tenant_memberships m \
         ON m.tenant_id = e.tenant_id AND m.user_id = e.user_id AND m.status = 'active' \
         WHERE e.tenant_id = $1 ORDER BY e.user_id",
    )
    .bind(tenant)
    .fetch_all(&mut **tx)
    .await?;
    let mappings = mapping::load_in(tx, tenant).await?;
    let mut changed = 0;
    for user_id in users {
        let groups = mapping::groups_in(tx, tenant, &user_id).await?;
        let resolved = mapping::resolve(&mappings, &groups);
        if !mapping::apply_in(tx, tenant, &user_id, &resolved, None)
            .await?
            .is_empty()
        {
            changed += 1;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custody_handles_materialize_like_the_model_gateway() {
        assert_eq!(custody_env("sso/acme-corp"), "QUANSIO_SECRET_SSO_ACME_CORP");
    }

    #[test]
    fn redirects_must_be_https_loopback_or_the_desktop_scheme() {
        for good in [
            "https://app.quansio.test/auth/sso/callback",
            "http://127.0.0.1:53123/callback",
            "http://localhost:8080/cb",
            "quansio://auth/sso",
        ] {
            assert!(check_redirect(good).is_ok(), "{good}");
        }
        for bad in [
            "http://evil.test/cb",
            "javascript:alert(1)",
            "https://app.test/cb#frag",
            "not a url",
        ] {
            assert!(check_redirect(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn groups_come_from_the_configured_claim_only() {
        let claims: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "groups": ["eng", 7, "ops"], "roles": ["admin"], "team": "solo",
        }))
        .expect("claims");
        assert_eq!(
            groups_of(&claims, "groups"),
            vec!["eng".to_string(), "ops".to_string()]
        );
        assert_eq!(groups_of(&claims, "team"), vec!["solo".to_string()]);
        assert!(groups_of(&claims, "missing").is_empty());
    }
}
