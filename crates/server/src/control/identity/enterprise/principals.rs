//! Service principals: the tenant's workload identities (OPS-001, DOMAIN.md §2).
//!
//! **Scopes.** A principal holds explicit scopes: §14 command families, plus `Provisioning` for
//! SCIM. They are checked when the principal is created: non-empty, known, no duplicates, and
//! none of the configured ungrantable families (`Trust`: approval decisions stay with people).
//!
//! **Key.** The key `qsk_<ULID>_<secret>` names its principal (`sp_<ULID>`) and is returned
//! exactly once. Only its argon2id hash is stored.
//!
//! **Authentication.**
//! - It reads the one row the key names and refuses anything not `active`, so a revocation takes
//!   effect on the next request.
//! - It verifies the hash on a blocking thread. A digest cache skips re-hashing only while the
//!   row still holds the hash that verified.
//! - Every authentication stamps `last_used_at`, and every command a principal runs is audited
//!   with actor `{kind: service, id}`.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};

use super::{audit_in, rejected, stage_in, Acting, EnterpriseError, EnterpriseIdentity};

/// Every service principal key starts with this.
pub const KEY_PREFIX: &str = "qsk_";

/// The scope that admits a principal to SCIM.
pub const PROVISIONING_SCOPE: &str = "Provisioning";

/// The most principals a digest cache remembers before it starts over.
const VERIFIED_CACHE_LIMIT: usize = 1024;

/// An authenticated service principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalScope {
    /// `sp_` id.
    pub id: String,
    /// The tenant it belongs to.
    pub tenant_id: String,
    /// Its explicit scopes.
    pub scopes: Vec<String>,
}

impl PrincipalScope {
    /// Whether the principal holds `scope`.
    #[must_use]
    pub fn holds(&self, scope: &str) -> bool {
        self.scopes.iter().any(|held| held == scope)
    }
}

/// A principal row as listed.
type PrincipalRow = (
    String,
    String,
    Value,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

fn view(row: PrincipalRow) -> Value {
    let (id, name, scopes, status, created_at, last_used_at) = row;
    json!({
        "id": id,
        "name": name,
        "scopes": scopes,
        "status": status,
        "created_at": created_at,
        "last_used_at": last_used_at,
    })
}

/// Split a presented key into the principal it names; `None` when it is not a key's shape.
fn principal_of(key: &str) -> Option<String> {
    let rest = key.strip_prefix(KEY_PREFIX)?;
    let (ulid, secret) = rest.split_at_checked(26)?;
    let secret = secret.strip_prefix('_')?;
    if secret.len() != 43
        || !secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let id = format!("sp_{ulid}");
    CanonicalId::parse_typed(&id, Prefix::ServicePrincipal).ok()?;
    Some(id)
}

impl EnterpriseIdentity {
    fn hasher(&self) -> Result<Argon2<'static>, EnterpriseError> {
        let params = Params::new(
            self.config.argon2_memory_kib,
            self.config.argon2_iterations,
            self.config.argon2_parallelism,
            None,
        )
        .map_err(|error| EnterpriseError::Invalid(format!("argon2 parameters: {error}")))?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    /// Check requested scopes against the §14 command `families` and the configured policy.
    ///
    /// # Errors
    /// [`EnterpriseError::Invalid`] naming the first scope refused.
    pub fn validate_scopes(
        &self,
        requested: &[String],
        families: &[&str],
    ) -> Result<Vec<String>, EnterpriseError> {
        if requested.is_empty() {
            return Err(EnterpriseError::Invalid(
                "a service principal needs at least one explicit scope".to_string(),
            ));
        }
        let mut scopes = Vec::with_capacity(requested.len());
        for scope in requested {
            if scopes.contains(scope) {
                return Err(EnterpriseError::Invalid(format!(
                    "scope {scope:?} is repeated"
                )));
            }
            if scope != PROVISIONING_SCOPE && !families.contains(&scope.as_str()) {
                return Err(EnterpriseError::Invalid(format!(
                    "scope {scope:?} is not a command family (one of {families:?}, or {PROVISIONING_SCOPE:?})"
                )));
            }
            if self
                .config
                .ungrantable_scopes
                .iter()
                .any(|refused| refused == scope)
            {
                return Err(EnterpriseError::Invalid(format!(
                    "scope {scope:?} cannot be granted to a service principal"
                )));
            }
            scopes.push(scope.clone());
        }
        Ok(scopes)
    }

    /// `CreateServicePrincipal {name, scopes}`: the principal and its key, returned once.
    ///
    /// # Errors
    /// [`EnterpriseError::Invalid`] for a bad name or scope.
    pub async fn create_service_principal(
        &self,
        tenant_id: &str,
        acting: &Acting,
        name: &str,
        scopes: &[String],
        families: &[&str],
    ) -> Result<(Value, String), EnterpriseError> {
        let name = name.trim().to_string();
        if name.is_empty() || name.len() > 200 {
            return Err(EnterpriseError::Invalid(
                "name must be 1 to 200 characters".to_string(),
            ));
        }
        let scopes = self.validate_scopes(scopes, families)?;
        let id =
            CanonicalId::generate(Prefix::ServicePrincipal, &mut UlidGenerator::new()).to_string();
        let mut secret = [0u8; 32];
        let mut salt = [0u8; 16];
        let rng = SystemRandom::new();
        rng.fill(&mut secret)
            .and_then(|()| rng.fill(&mut salt))
            .map_err(|_| EnterpriseError::Invalid("platform RNG failed".to_string()))?;
        let key = format!(
            "{KEY_PREFIX}{}_{}",
            &id[3..],
            URL_SAFE_NO_PAD.encode(secret)
        );
        let hasher = self.hasher()?;
        let salt = SaltString::encode_b64(&salt)
            .map_err(|error| EnterpriseError::Invalid(format!("salt: {error}")))?;
        let secret_key = key.clone();
        let key_hash = tokio::task::spawn_blocking(move || {
            hasher
                .hash_password(secret_key.as_bytes(), &salt)
                .map(|hash| hash.to_string())
        })
        .await
        .map_err(|error| EnterpriseError::Invalid(format!("hashing: {error}")))?
        .map_err(|error| EnterpriseError::Invalid(format!("hashing: {error}")))?;

        let tenant = tenant_id.to_string();
        let acting = acting.clone();
        let (row_id, row_name, row_scopes) = (id.clone(), name.clone(), scopes.clone());
        let row = self
            .events()
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let row: PrincipalRow = sqlx::query_as(
                        "INSERT INTO service_principals (id, tenant_id, name, scopes, key_hash, status) \
                         VALUES ($1, $2, $3, $4, $5, 'active') \
                         RETURNING id, name, scopes, status, created_at, last_used_at",
                    )
                    .bind(&row_id)
                    .bind(&tenant)
                    .bind(&row_name)
                    .bind(json!(row_scopes))
                    .bind(&key_hash)
                    .fetch_one(&mut **tx)
                    .await?;
                    stage_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        ("tenant", &tenant),
                        "tenant.service_principal_created",
                        json!({ "service_principal_id": row_id, "name": row_name, "scopes": row_scopes }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        "CreateServicePrincipal",
                        &format!("service_principal:{row_id}"),
                        "applied",
                        &format!("scopes={}", row_scopes.join(",")),
                    )
                    .await?;
                    Ok(row)
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        Ok((view(row), key))
    }

    /// `RevokeServicePrincipal {service_principal_id}`: refused on its next request.
    ///
    /// # Errors
    /// [`EnterpriseError::NotFound`] for a principal not in the tenant,
    /// [`EnterpriseError::Conflict`] for one already revoked.
    pub async fn revoke_service_principal(
        &self,
        tenant_id: &str,
        acting: &Acting,
        principal_id: &str,
    ) -> Result<Value, EnterpriseError> {
        let tenant = tenant_id.to_string();
        let acting = acting.clone();
        let principal = principal_id.to_string();
        let row = self
            .events()
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let status: Option<String> = sqlx::query_scalar(
                        "SELECT status FROM service_principals WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
                    )
                    .bind(&tenant)
                    .bind(&principal)
                    .fetch_optional(&mut **tx)
                    .await?;
                    match status.as_deref() {
                        None => {
                            return Err(rejected(EnterpriseError::NotFound(format!(
                                "service principal {principal} is not in this tenant"
                            ))))
                        }
                        Some("removed") => {
                            return Err(rejected(EnterpriseError::Conflict(format!(
                                "service principal {principal} is already revoked"
                            ))))
                        }
                        Some(_) => {}
                    }
                    let row: PrincipalRow = sqlx::query_as(
                        "UPDATE service_principals SET status = 'removed' WHERE tenant_id = $1 AND id = $2 \
                         RETURNING id, name, scopes, status, created_at, last_used_at",
                    )
                    .bind(&tenant)
                    .bind(&principal)
                    .fetch_one(&mut **tx)
                    .await?;
                    stage_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        ("tenant", &tenant),
                        "tenant.service_principal_revoked",
                        json!({ "service_principal_id": principal }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        "RevokeServicePrincipal",
                        &format!("service_principal:{principal}"),
                        "applied",
                        "revoked",
                    )
                    .await?;
                    Ok(row)
                })
            })
            .await
            .map_err(super::unwrap_rejection)?;
        Ok(view(row))
    }

    /// `GET /v1/service-principals`: the tenant's principals (never a key or its hash).
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn service_principals(&self, tenant_id: &str) -> Result<Vec<Value>, EnterpriseError> {
        let mut tx = self.tenant_transaction(tenant_id).await?;
        let rows: Vec<PrincipalRow> = sqlx::query_as(
            "SELECT id, name, scopes, status, created_at, last_used_at FROM service_principals \
             WHERE tenant_id = $1 ORDER BY created_at DESC",
        )
        .bind(tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(view).collect())
    }

    /// Authenticate a presented `qsk_` key.
    ///
    /// # Errors
    /// [`EnterpriseError::Unauthenticated`] for a key that is malformed, unknown, revoked or wrong.
    pub async fn authenticate_principal(
        &self,
        key: &str,
    ) -> Result<PrincipalScope, EnterpriseError> {
        let refused = || {
            EnterpriseError::Unauthenticated("the service principal key is not valid".to_string())
        };
        let id = principal_of(key).ok_or_else(refused)?;
        let mut tx = self.auth_transaction(None).await?;
        let row: Option<(String, Value, String, String)> = sqlx::query_as(
            "SELECT tenant_id, scopes, key_hash, status FROM service_principals WHERE id = $1",
        )
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((tenant_id, scopes, key_hash, status)) = row else {
            return Err(refused());
        };
        if status != "active" {
            return Err(EnterpriseError::Unauthenticated(
                "the service principal is revoked".to_string(),
            ));
        }
        let digest = super::jwt::sha256_hex(key);
        let cached = self
            .verified_keys
            .lock()
            .ok()
            .and_then(|cache| cache.get(&digest).cloned());
        if cached != Some((id.clone(), key_hash.clone())) {
            let presented = key.to_string();
            let stored = key_hash.clone();
            let verified = tokio::task::spawn_blocking(move || {
                PasswordHash::new(&stored).is_ok_and(|parsed| {
                    parsed.algorithm == argon2::ARGON2ID_IDENT
                        && Argon2::default()
                            .verify_password(presented.as_bytes(), &parsed)
                            .is_ok()
                })
            })
            .await
            .unwrap_or(false);
            if !verified {
                return Err(refused());
            }
            if let Ok(mut cache) = self.verified_keys.lock() {
                if cache.len() >= VERIFIED_CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(digest, (id.clone(), key_hash));
            }
        }
        sqlx::query("UPDATE service_principals SET last_used_at = now() WHERE id = $1")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(PrincipalScope {
            id,
            tenant_id,
            scopes: scopes
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Audit one command a principal attempted, applied or refused.
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn audit_principal_command(
        &self,
        principal: &PrincipalScope,
        command: &str,
        decision: &str,
        reason: &str,
        correlation: &str,
    ) -> Result<(), EnterpriseError> {
        let mut tx = self.tenant_transaction(&principal.tenant_id).await?;
        let mut acting = Acting::service(&principal.id);
        if let Ok(id) = correlation.parse() {
            acting.correlation_id = id;
        }
        audit_in(
            &mut tx,
            &principal.tenant_id,
            &acting,
            command,
            &format!("service_principal:{}", principal.id),
            decision,
            reason,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_key_shaped_value_names_a_principal() {
        let ulid = "01J8Z3K6F1N8VQ2X5W9Y0PS001";
        let secret = "A".repeat(43);
        assert_eq!(
            principal_of(&format!("qsk_{ulid}_{secret}")).as_deref(),
            Some("sp_01J8Z3K6F1N8VQ2X5W9Y0PS001")
        );
        for bad in [
            format!("qsk_{ulid}{secret}"),
            format!("qsk_{ulid}_{}", "A".repeat(42)),
            format!("qsk_{ulid}_{}!", "A".repeat(42)),
            format!("qrt_{ulid}_{secret}"),
            format!("qsk_01J8Z3K6F1N8VQ2X5W9Y0PSOOO_{secret}"),
            "qsk_".to_string(),
        ] {
            assert!(principal_of(&bad).is_none(), "{bad}");
        }
    }
}
