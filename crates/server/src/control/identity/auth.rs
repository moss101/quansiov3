//! Sessions and passwordless sign-in (APP-002, DOMAIN.md §2).
//!
//! A session is a short-lived ES256 access token (≤ 15 minutes) plus an opaque, rotating
//! refresh token. The refresh token is stored hashed and belongs to a rotation *family*: a
//! refresh that presents a token which was already rotated is a replay, and the whole
//! family is revoked so a leaked token cannot outlive its theft. Magic-link tokens are
//! likewise stored hashed and consumed exactly once.
//!
//! The signing key is P-256 from secret custody (`QUANSIO_AUTH_SIGNING_KEY_PKCS8_B64`,
//! a base64 PKCS#8 document) or, when unset, generated for this process with a warning:
//! sessions then do not survive a restart, which is acceptable for development only.

use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{
    EcdsaKeyPair, KeyPair, UnparsedPublicKey, ECDSA_P256_SHA256_FIXED,
    ECDSA_P256_SHA256_FIXED_SIGNING,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

use super::{IdentityError, IdentityStore, SignUpMethod};

/// A stored challenge row: id, email, display name, expiry, consumption.
type ChallengeRow = (String, String, String, DateTime<Utc>, Option<DateTime<Utc>>);
/// A stored session row for rotation: id, user, tenant, family, method, refresh expiry, rotated, revoked.
type SessionRow = (
    String,
    String,
    String,
    String,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

/// Environment variable carrying the PKCS#8 (base64) P-256 signing key.
pub const SIGNING_KEY_ENV: &str = "QUANSIO_AUTH_SIGNING_KEY_PKCS8_B64";

/// Errors from authentication.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No credential, or a credential that does not verify.
    #[error("auth invalid token: {0}")]
    InvalidToken(String),
    /// A credential that verified but has expired.
    #[error("auth expired: {0}")]
    Expired(String),
    /// The signing key material is unusable.
    #[error("auth signing key: {0}")]
    SigningKey(String),
    /// Identity/onboarding failure.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Database failure.
    #[error("auth database: {0}")]
    Database(#[from] sqlx::Error),
}

/// Token lifetimes.
#[derive(Debug, Clone, Copy)]
pub struct AuthConfig {
    /// Access token lifetime; DOMAIN.md §2 caps it at 15 minutes.
    pub access_ttl: Duration,
    /// Refresh token lifetime (idle sessions end here).
    pub refresh_ttl: Duration,
    /// Magic-link validity.
    pub magic_link_ttl: Duration,
    /// OAuth authorization request validity.
    pub oauth_state_ttl: Duration,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            access_ttl: Duration::from_secs(15 * 60),
            refresh_ttl: Duration::from_secs(30 * 24 * 60 * 60),
            magic_link_ttl: Duration::from_secs(15 * 60),
            oauth_state_ttl: Duration::from_secs(10 * 60),
        }
    }
}

/// The P-256 key that signs access tokens.
pub struct SigningKey {
    pair: EcdsaKeyPair,
    public: Vec<u8>,
    kid: String,
    ephemeral: bool,
    rng: SystemRandom,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field("kid", &self.kid)
            .field("ephemeral", &self.ephemeral)
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// The key from secret custody, or an ephemeral one when custody has none.
    ///
    /// # Errors
    /// Returns [`AuthError::SigningKey`] when the configured material is not a P-256 PKCS#8 key.
    pub fn from_env() -> Result<Self, AuthError> {
        match std::env::var(SIGNING_KEY_ENV) {
            Ok(value) if !value.trim().is_empty() => {
                let pkcs8 = base64::engine::general_purpose::STANDARD
                    .decode(value.trim())
                    .map_err(|error| {
                        AuthError::SigningKey(format!("{SIGNING_KEY_ENV}: {error}"))
                    })?;
                Self::from_pkcs8(&pkcs8, false)
            }
            _ => {
                eprintln!(
                    "quansio-server: {SIGNING_KEY_ENV} is not set; using an ephemeral signing key, sessions will not survive a restart"
                );
                Self::ephemeral()
            }
        }
    }

    /// A key that lives only in this process.
    ///
    /// # Errors
    /// Returns [`AuthError::SigningKey`] if the platform RNG fails.
    pub fn ephemeral() -> Result<Self, AuthError> {
        let rng = SystemRandom::new();
        let document = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| AuthError::SigningKey("cannot generate a P-256 key".to_string()))?;
        Self::from_pkcs8(document.as_ref(), true)
    }

    fn from_pkcs8(pkcs8: &[u8], ephemeral: bool) -> Result<Self, AuthError> {
        let rng = SystemRandom::new();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng)
            .map_err(|_| AuthError::SigningKey("not a P-256 PKCS#8 key".to_string()))?;
        let public = pair.public_key().as_ref().to_vec();
        let kid = URL_SAFE_NO_PAD.encode(&digest(&SHA256, &public).as_ref()[..8]);
        Ok(Self {
            pair,
            public,
            kid,
            ephemeral,
            rng,
        })
    }

    /// Key identifier placed in the token header.
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// Whether this key was generated for the process rather than read from custody.
    #[must_use]
    pub const fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthError> {
        self.pair
            .sign(&self.rng, message)
            .map(|signature| signature.as_ref().to_vec())
            .map_err(|_| AuthError::SigningKey("signing failed".to_string()))
    }

    fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &self.public)
            .verify(message, signature)
            .is_ok()
    }
}

/// Claims carried by an access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessClaims {
    /// `usr_` id.
    pub sub: String,
    /// `tn_` id the session acts in.
    pub tid: String,
    /// `ses_` id, so revocation is checkable.
    pub sid: String,
    /// Issued at (unix seconds).
    pub iat: i64,
    /// Expiry (unix seconds).
    pub exp: i64,
}

#[derive(Serialize, Deserialize)]
struct Header<'a> {
    alg: &'a str,
    typ: &'a str,
    kid: &'a str,
}

/// Mint a compact JWS (`header.claims.signature`), ES256.
///
/// # Errors
/// Returns [`AuthError::SigningKey`] if signing fails.
pub fn mint_access_token(key: &SigningKey, claims: &AccessClaims) -> Result<String, AuthError> {
    let header = serde_json::to_vec(&Header {
        alg: "ES256",
        typ: "JWT",
        kid: key.kid(),
    })
    .map_err(|error| AuthError::SigningKey(error.to_string()))?;
    let body =
        serde_json::to_vec(claims).map_err(|error| AuthError::SigningKey(error.to_string()))?;
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(body)
    );
    let signature = key.sign(signing_input.as_bytes())?;
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

/// Verify a compact JWS against `key` at `now`.
///
/// # Errors
/// [`AuthError::InvalidToken`] for a malformed, foreign-key or tampered token,
/// [`AuthError::Expired`] for a valid token past `exp`.
pub fn verify_access_token(
    key: &SigningKey,
    token: &str,
    now: DateTime<Utc>,
) -> Result<AccessClaims, AuthError> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(claims_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(AuthError::InvalidToken("not a compact JWS".to_string()));
    };
    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| AuthError::InvalidToken("header is not base64url".to_string()))?;
    let header: Header<'_> = serde_json::from_slice(&header_bytes)
        .map_err(|_| AuthError::InvalidToken("header is not JSON".to_string()))?;
    if header.alg != "ES256" || header.typ != "JWT" {
        return Err(AuthError::InvalidToken("unsupported algorithm".to_string()));
    }
    if header.kid != key.kid() {
        return Err(AuthError::InvalidToken("unknown signing key".to_string()));
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| AuthError::InvalidToken("signature is not base64url".to_string()))?;
    let signing_input = format!("{header_b64}.{claims_b64}");
    if !key.verify(signing_input.as_bytes(), &signature) {
        return Err(AuthError::InvalidToken(
            "signature does not verify".to_string(),
        ));
    }
    let claims_bytes = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|_| AuthError::InvalidToken("claims are not base64url".to_string()))?;
    let claims: AccessClaims = serde_json::from_slice(&claims_bytes)
        .map_err(|_| AuthError::InvalidToken("claims are not the access shape".to_string()))?;
    if claims.exp <= now.timestamp() {
        return Err(AuthError::Expired("access token expired".to_string()));
    }
    Ok(claims)
}

/// A freshly issued or rotated session.
#[derive(Debug, Clone, Serialize)]
pub struct IssuedSession {
    /// `ses_` id.
    pub session_id: String,
    /// `usr_` id.
    pub user_id: String,
    /// `tn_` id the tokens act in.
    pub tenant_id: String,
    /// Compact ES256 JWT.
    pub access_token: String,
    /// When the access token stops verifying.
    pub access_expires_at: DateTime<Utc>,
    /// Opaque refresh token; the only copy is the one returned here.
    pub refresh_token: String,
    /// When the refresh token stops rotating.
    pub refresh_expires_at: DateTime<Utc>,
}

/// A magic-link challenge: the token is delivered out of band and never stored in clear.
#[derive(Debug, Clone)]
pub struct MagicLinkChallenge {
    /// `ach_` id.
    pub challenge_id: String,
    /// The one-time token to deliver.
    pub token: String,
    /// Normalized email.
    pub email: String,
    /// When the token stops working.
    pub expires_at: DateTime<Utc>,
}

/// The identity a verified access token resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScope {
    /// `usr_` id.
    pub user_id: String,
    /// `tn_` id.
    pub tenant_id: String,
    /// `ses_` id.
    pub session_id: String,
}

/// Where a user landed after sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedInUser {
    /// `usr_` id.
    pub user_id: String,
    /// The tenant the session acts in (the personal tenant by default).
    pub tenant_id: String,
    /// The default workspace in that tenant.
    pub workspace_id: String,
    /// Whether this sign-in created the user.
    pub onboarded: bool,
}

/// Session and sign-in authority over the identity tables.
#[derive(Clone)]
pub struct AuthStore {
    pool: PgPool,
    identity: IdentityStore,
    key: Arc<SigningKey>,
    config: AuthConfig,
    rng: SystemRandom,
}

impl std::fmt::Debug for AuthStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthStore")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

fn sha256_hex(value: &str) -> String {
    hex::encode(digest(&SHA256, value.as_bytes()))
}

impl AuthStore {
    /// Bind to a pool with `key` and `config`.
    #[must_use]
    pub fn new(pool: PgPool, key: Arc<SigningKey>, config: AuthConfig) -> Self {
        Self {
            identity: IdentityStore::new(pool.clone()),
            pool,
            key,
            config,
            rng: SystemRandom::new(),
        }
    }

    /// The signing key (for verification elsewhere).
    #[must_use]
    pub fn key(&self) -> &Arc<SigningKey> {
        &self.key
    }

    /// Lifetimes in force.
    #[must_use]
    pub const fn config(&self) -> AuthConfig {
        self.config
    }

    /// The identity store this authority onboards through.
    #[must_use]
    pub const fn identity(&self) -> &IdentityStore {
        &self.identity
    }

    /// The pool (for the OAuth half of this authority).
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// A transaction with the `quansio.auth_flow` gate open: the only way the pre-authentication
    /// tables are reachable under row-level security, and closed again at commit.
    async fn begin_auth_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, AuthError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('quansio.auth_flow', 'on', true)")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    fn opaque_token(&self, prefix: &str) -> Result<String, AuthError> {
        let mut bytes = [0u8; 32];
        self.rng
            .fill(&mut bytes)
            .map_err(|_| AuthError::SigningKey("platform RNG failed".to_string()))?;
        Ok(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// Begin a magic-link sign-in; the caller delivers `token` to `email`.
    ///
    /// # Errors
    /// Returns a database error, or [`AuthError::InvalidToken`] for an email that is not one.
    pub async fn start_magic_link(
        &self,
        email: &str,
        display_name: &str,
    ) -> Result<MagicLinkChallenge, AuthError> {
        let email = normalize_email(email)?;
        let token = self.opaque_token("qml_")?;
        let challenge_id = format!(
            "ach_{}",
            CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new())
                .to_string()
                .trim_start_matches("cmd_")
        );
        let expires_at = Utc::now()
            + chrono::Duration::from_std(self.config.magic_link_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(15));
        let mut tx = self.begin_auth_transaction().await?;
        sqlx::query(
            "INSERT INTO auth_challenges (id, email, display_name, token_hash, expires_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&challenge_id)
        .bind(&email)
        .bind(display_name.trim())
        .bind(sha256_hex(&token))
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(MagicLinkChallenge {
            challenge_id,
            token,
            email,
            expires_at,
        })
    }

    /// Complete a magic-link sign-in with the delivered token; consumes it.
    ///
    /// # Errors
    /// [`AuthError::InvalidToken`] for an unknown or already-consumed token,
    /// [`AuthError::Expired`] for an expired one.
    pub async fn complete_magic_link(
        &self,
        token: &str,
        client: &str,
    ) -> Result<(IssuedSession, SignedInUser), AuthError> {
        let mut tx = self.begin_auth_transaction().await?;
        let row: Option<ChallengeRow> = sqlx::query_as(
            "SELECT id, email, display_name, expires_at, consumed_at \
             FROM auth_challenges WHERE token_hash = $1",
        )
        .bind(sha256_hex(token))
        .fetch_optional(&mut *tx)
        .await?;
        let Some((challenge_id, email, display_name, expires_at, consumed_at)) = row else {
            return Err(AuthError::InvalidToken(
                "magic-link token is not recognized".to_string(),
            ));
        };
        if consumed_at.is_some() {
            return Err(AuthError::InvalidToken(
                "magic-link token was already used".to_string(),
            ));
        }
        if expires_at <= Utc::now() {
            return Err(AuthError::Expired("magic-link token expired".to_string()));
        }
        let consumed = sqlx::query(
            "UPDATE auth_challenges SET consumed_at = now() WHERE id = $1 AND consumed_at IS NULL",
        )
        .bind(&challenge_id)
        .execute(&mut *tx)
        .await?;
        if consumed.rows_affected() == 0 {
            return Err(AuthError::InvalidToken(
                "magic-link token was already used".to_string(),
            ));
        }
        tx.commit().await?;
        let user = self
            .find_or_onboard(&email, &display_name, SignUpMethod::EmailMagicLink)
            .await?;
        let session = self
            .issue_session(&user.user_id, &user.tenant_id, "email_magic_link", client)
            .await?;
        Ok((session, user))
    }

    /// Find the user for `email` or onboard one with a personal tenant.
    ///
    /// # Errors
    /// Returns identity or database errors.
    pub async fn find_or_onboard(
        &self,
        email: &str,
        display_name: &str,
        method: SignUpMethod,
    ) -> Result<SignedInUser, AuthError> {
        let email = normalize_email(email)?;
        if let Some(existing) = self.lookup_by_email(&email).await? {
            return Ok(existing);
        }
        let display_name = if display_name.trim().is_empty() {
            email.split('@').next().unwrap_or("user").to_string()
        } else {
            display_name.trim().to_string()
        };
        let onboarded = self.identity.onboard(&email, &display_name, method).await?;
        Ok(SignedInUser {
            user_id: onboarded.user_id,
            tenant_id: onboarded.tenant_id,
            workspace_id: onboarded.workspace_id,
            onboarded: true,
        })
    }

    async fn lookup_by_email(&self, email: &str) -> Result<Option<SignedInUser>, AuthError> {
        let mut tx = self.begin_auth_transaction().await?;
        let user: Option<(String,)> =
            sqlx::query_as("SELECT id FROM users WHERE primary_email = $1 AND status = 'active'")
                .bind(email)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((user_id,)) = user else {
            return Ok(None);
        };
        sqlx::query("SELECT set_config('quansio.user_id', $1, true)")
            .bind(&user_id)
            .execute(&mut *tx)
            .await?;
        // The personal tenant first, else the earliest active membership; an invitation
        // (`invited`, APP-012's InviteMember) counts, and signing in is what accepts it.
        let tenant: Option<(String,)> = sqlx::query_as(
            "SELECT t.id FROM tenant_memberships m JOIN tenants t ON t.id = m.tenant_id \
             WHERE m.user_id = $1 AND m.status IN ('active', 'invited') \
             ORDER BY t.personal DESC, m.status ASC, m.created_at ASC LIMIT 1",
        )
        .bind(&user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((tenant_id,)) = tenant else {
            return Ok(None);
        };
        sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
            .bind(&tenant_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE tenant_memberships SET status = 'active', updated_at = now() \
             WHERE tenant_id = $1 AND user_id = $2 AND status = 'invited'",
        )
        .bind(&tenant_id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE workspace_memberships SET status = 'active', updated_at = now() \
             WHERE tenant_id = $1 AND user_id = $2 AND status = 'invited'",
        )
        .bind(&tenant_id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
        let workspace: Option<(String,)> = sqlx::query_as(
            "SELECT w.id FROM workspaces w \
             JOIN workspace_memberships wm ON wm.workspace_id = w.id AND wm.user_id = $2 AND wm.status = 'active' \
             WHERE w.tenant_id = $1 ORDER BY w.created_at ASC LIMIT 1",
        )
        .bind(&tenant_id)
        .bind(&user_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some((workspace_id,)) = workspace else {
            return Ok(None);
        };
        Ok(Some(SignedInUser {
            user_id,
            tenant_id,
            workspace_id,
            onboarded: false,
        }))
    }

    /// Issue a new session (a new rotation family) for `user_id` in `tenant_id`.
    ///
    /// # Errors
    /// Returns signing or database errors.
    pub async fn issue_session(
        &self,
        user_id: &str,
        tenant_id: &str,
        auth_method: &str,
        client: &str,
    ) -> Result<IssuedSession, AuthError> {
        let family_id =
            CanonicalId::generate(Prefix::Session, &mut UlidGenerator::new()).to_string();
        self.insert_session(user_id, tenant_id, &family_id, auth_method, client, None)
            .await
    }

    async fn insert_session(
        &self,
        user_id: &str,
        tenant_id: &str,
        family_id: &str,
        auth_method: &str,
        client: &str,
        rotated_from: Option<&str>,
    ) -> Result<IssuedSession, AuthError> {
        let client = match client {
            "desktop" | "web" | "cli" => client,
            _ => "desktop",
        };
        let session_id =
            CanonicalId::generate(Prefix::Session, &mut UlidGenerator::new()).to_string();
        let refresh_token = self.opaque_token("qrt_")?;
        let now = Utc::now();
        let refresh_expires_at = now
            + chrono::Duration::from_std(self.config.refresh_ttl)
                .unwrap_or_else(|_| chrono::Duration::days(30));
        let access_expires_at = now
            + chrono::Duration::from_std(self.config.access_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(15));
        let mut tx = self.begin_auth_transaction().await?;
        sqlx::query(
            "INSERT INTO sessions (id, user_id, tenant_id, family_id, client, auth_method, refresh_hash, \
             refresh_expires_at, rotated_from, last_used_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())",
        )
        .bind(&session_id)
        .bind(user_id)
        .bind(tenant_id)
        .bind(family_id)
        .bind(client)
        .bind(auth_method)
        .bind(sha256_hex(&refresh_token))
        .bind(refresh_expires_at)
        .bind(rotated_from)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let access_token = mint_access_token(
            &self.key,
            &AccessClaims {
                sub: user_id.to_string(),
                tid: tenant_id.to_string(),
                sid: session_id.clone(),
                iat: now.timestamp(),
                exp: access_expires_at.timestamp(),
            },
        )?;
        Ok(IssuedSession {
            session_id,
            user_id: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            access_token,
            access_expires_at,
            refresh_token,
            refresh_expires_at,
        })
    }

    /// Rotate a refresh token: the presented token is retired and a new pair is issued.
    ///
    /// # Errors
    /// [`AuthError::InvalidToken`] for an unknown token, a revoked session, or a token that was
    /// already rotated (a replay: the whole family is revoked); [`AuthError::Expired`] past
    /// the refresh lifetime.
    pub async fn refresh(
        &self,
        refresh_token: &str,
        client: &str,
    ) -> Result<IssuedSession, AuthError> {
        let mut tx = self.begin_auth_transaction().await?;
        let row: Option<SessionRow> = sqlx::query_as(
            "SELECT id, user_id, tenant_id, family_id, auth_method, refresh_expires_at, rotated_at, revoked_at \
             FROM sessions WHERE refresh_hash = $1",
        )
        .bind(sha256_hex(refresh_token))
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some((
            session_id,
            user_id,
            tenant_id,
            family_id,
            auth_method,
            refresh_expires_at,
            rotated_at,
            revoked_at,
        )) = row
        else {
            return Err(AuthError::InvalidToken(
                "refresh token is not recognized".to_string(),
            ));
        };
        if revoked_at.is_some() {
            return Err(AuthError::InvalidToken("session was revoked".to_string()));
        }
        if rotated_at.is_some() {
            // Replay of a retired token: someone holds a copy. Nothing in the family survives.
            self.revoke_family(&family_id, "refresh token reuse")
                .await?;
            return Err(AuthError::InvalidToken(
                "refresh token was already rotated; the session family is revoked".to_string(),
            ));
        }
        if refresh_expires_at <= Utc::now() {
            return Err(AuthError::Expired("refresh token expired".to_string()));
        }
        let mut tx = self.begin_auth_transaction().await?;
        let retired = sqlx::query(
            "UPDATE sessions SET rotated_at = now(), last_used_at = now() WHERE id = $1 AND rotated_at IS NULL",
        )
        .bind(&session_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if retired.rows_affected() == 0 {
            self.revoke_family(&family_id, "concurrent refresh").await?;
            return Err(AuthError::InvalidToken(
                "refresh token was rotated concurrently; the session family is revoked".to_string(),
            ));
        }
        self.insert_session(
            &user_id,
            &tenant_id,
            &family_id,
            &auth_method,
            client,
            Some(&session_id),
        )
        .await
    }

    /// Revoke every session in the family of `session_id` (sign-out).
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn sign_out(&self, session_id: &str) -> Result<(), AuthError> {
        let mut tx = self.begin_auth_transaction().await?;
        let family: Option<(String,)> =
            sqlx::query_as("SELECT family_id FROM sessions WHERE id = $1")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        if let Some((family_id,)) = family {
            self.revoke_family(&family_id, "signed out").await?;
        }
        Ok(())
    }

    async fn revoke_family(&self, family_id: &str, reason: &str) -> Result<(), AuthError> {
        let mut tx = self.begin_auth_transaction().await?;
        sqlx::query(
            "UPDATE sessions SET revoked_at = now(), revoked_reason = $2 \
             WHERE family_id = $1 AND revoked_at IS NULL",
        )
        .bind(family_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Resolve a bearer access token to a live session scope.
    ///
    /// # Errors
    /// [`AuthError::InvalidToken`] when the token does not verify or its session is revoked,
    /// [`AuthError::Expired`] when the access token is past its lifetime.
    pub async fn authenticate(&self, bearer: &str) -> Result<SessionScope, AuthError> {
        let claims = verify_access_token(&self.key, bearer, Utc::now())?;
        let mut tx = self.begin_auth_transaction().await?;
        let revoked: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
            "SELECT revoked_at FROM sessions WHERE id = $1 AND tenant_id = $2 AND user_id = $3",
        )
        .bind(&claims.sid)
        .bind(&claims.tid)
        .bind(&claims.sub)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        match revoked {
            None => Err(AuthError::InvalidToken(
                "session does not exist".to_string(),
            )),
            Some((Some(_),)) => Err(AuthError::InvalidToken("session was revoked".to_string())),
            Some((None,)) => Ok(SessionScope {
                user_id: claims.sub,
                tenant_id: claims.tid,
                session_id: claims.sid,
            }),
        }
    }

    /// The signed-in user's own view: identity, tenants, workspaces, effective settings.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn me(&self, scope: &SessionScope) -> Result<Value, AuthError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
            .bind(&scope.tenant_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT set_config('quansio.user_id', $1, true)")
            .bind(&scope.user_id)
            .execute(&mut *tx)
            .await?;
        let user: (String, String, Value) = sqlx::query_as(
            "SELECT primary_email, display_name, auth_methods FROM users WHERE id = $1",
        )
        .bind(&scope.user_id)
        .fetch_one(&mut *tx)
        .await?;
        let tenants: Vec<(String, String, bool, String)> = sqlx::query_as(
            "SELECT t.id, t.name, t.personal, m.role FROM tenant_memberships m \
             JOIN tenants t ON t.id = m.tenant_id WHERE m.user_id = $1 AND m.status = 'active' \
             ORDER BY t.personal DESC, m.created_at ASC",
        )
        .bind(&scope.user_id)
        .fetch_all(&mut *tx)
        .await?;
        let workspaces: Vec<(String, String, String, Value)> = sqlx::query_as(
            "SELECT w.id, w.name, wm.role, w.settings FROM workspace_memberships wm \
             JOIN workspaces w ON w.id = wm.workspace_id \
             WHERE wm.user_id = $1 AND wm.tenant_id = $2 AND wm.status = 'active' \
             ORDER BY w.created_at ASC",
        )
        .bind(&scope.user_id)
        .bind(&scope.tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut workspace_views = Vec::with_capacity(workspaces.len());
        for (id, name, role, _settings) in &workspaces {
            let effective = self
                .identity
                .effective_for_workspace(&scope.tenant_id, id, &scope.user_id)
                .await?;
            workspace_views.push(json!({
                "id": id,
                "name": name,
                "role": role,
                "effective_settings": effective,
            }));
        }
        let methods: Vec<Value> = user
            .2
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.get("method").cloned())
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({
            "user": { "id": scope.user_id, "primary_email": user.0, "display_name": user.1, "auth_methods": methods },
            "session": { "id": scope.session_id, "tenant_id": scope.tenant_id },
            "tenants": tenants.iter().map(|(id, name, personal, role)| json!({
                "id": id, "name": name, "personal": personal, "role": role,
            })).collect::<Vec<_>>(),
            "workspaces": workspace_views,
        }))
    }
}

/// Lower-case, trimmed, and shaped like an address; nothing else is accepted.
///
/// # Errors
/// [`AuthError::InvalidToken`] for a value that is not an email address.
pub fn normalize_email(email: &str) -> Result<String, AuthError> {
    let trimmed = email.trim().to_ascii_lowercase();
    let Some((local, domain)) = trimmed.split_once('@') else {
        return Err(AuthError::InvalidToken("not an email address".to_string()));
    };
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || trimmed.chars().any(char::is_whitespace)
        || trimmed.len() > 254
    {
        return Err(AuthError::InvalidToken("not an email address".to_string()));
    }
    Ok(trimmed)
}
