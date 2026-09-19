//! OAuth sign-in with Google, Microsoft and GitHub (APP-002, OD-002).
//!
//! Authorization-code flow with PKCE (S256). The server starts the request (state and
//! verifier stored, never handed to the client), the client sends the user to the
//! provider, and the returned code is exchanged here. Endpoints and client ids come from
//! `config/auth.yaml`; the client secret is read from the environment variable the config
//! names, which is the same custody rule the model gateway follows. An unconfigured provider
//! is not offered and fails closed rather than pretending.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::Value;

use super::auth::{AuthError, AuthStore, IssuedSession, SignedInUser};
use super::SignUpMethod;

/// A stored OAuth request row: provider, verifier, redirect, expiry, consumption.
type OAuthStateRow = (String, String, String, DateTime<Utc>, Option<DateTime<Utc>>);

/// Providers the baseline supports.
pub const PROVIDERS: &[&str] = &["google", "microsoft", "github"];

/// One provider's endpoints, as configured.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthProviderConfig {
    /// Where the user is sent.
    pub authorization_url: String,
    /// Where the code is exchanged.
    pub token_url: String,
    /// Where the identity is read.
    pub userinfo_url: String,
    /// Scopes requested.
    pub scopes: Vec<String>,
    /// Environment variable holding the public client id.
    pub client_id_env: String,
    /// Environment variable holding the client secret (secret custody).
    pub client_secret_env: String,
}

/// The auth configuration file.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthFile {
    /// Schema version.
    pub version: u32,
    /// Access token lifetime.
    pub access_token_ttl_seconds: u64,
    /// Refresh token lifetime.
    pub refresh_token_ttl_seconds: u64,
    /// Magic-link lifetime.
    pub magic_link_ttl_seconds: u64,
    /// OAuth request lifetime.
    pub oauth_state_ttl_seconds: u64,
    /// Providers by name.
    pub providers: BTreeMap<String, OAuthProviderConfig>,
}

impl AuthFile {
    /// Load `config/auth.yaml` from the repository/deployment root.
    ///
    /// # Errors
    /// Returns a message when the file is missing or malformed.
    pub fn load() -> Result<Self, String> {
        let path = locate("config/auth.yaml").ok_or("config/auth.yaml not found")?;
        Self::load_from(&path)
    }

    /// Load a specific file.
    ///
    /// # Errors
    /// Returns a message when the file is missing or malformed.
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let file: Self =
            serde_yaml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        if file.version != 1 {
            return Err(format!(
                "{}: unsupported version {}",
                path.display(),
                file.version
            ));
        }
        if file.access_token_ttl_seconds > 900 {
            return Err(format!(
                "{}: access_token_ttl_seconds exceeds the 15-minute cap (DOMAIN.md §2)",
                path.display()
            ));
        }
        for name in file.providers.keys() {
            if !PROVIDERS.contains(&name.as_str()) {
                return Err(format!("{}: unknown provider {name}", path.display()));
            }
        }
        Ok(file)
    }

    /// Lifetimes as an [`super::AuthConfig`].
    #[must_use]
    pub fn auth_config(&self) -> super::AuthConfig {
        super::AuthConfig {
            access_ttl: std::time::Duration::from_secs(self.access_token_ttl_seconds),
            refresh_ttl: std::time::Duration::from_secs(self.refresh_token_ttl_seconds),
            magic_link_ttl: std::time::Duration::from_secs(self.magic_link_ttl_seconds),
            oauth_state_ttl: std::time::Duration::from_secs(self.oauth_state_ttl_seconds),
        }
    }
}

fn locate(relative: &str) -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(relative);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    manifest.is_file().then_some(manifest)
}

/// A configured provider with its client id present in the environment.
#[derive(Debug, Clone)]
pub struct ReadyProvider {
    /// `google` | `microsoft` | `github`.
    pub name: String,
    /// Endpoints and scopes.
    pub config: OAuthProviderConfig,
    /// The public client id.
    pub client_id: String,
}

/// The start of an authorization-code flow.
#[derive(Debug, Clone)]
pub struct OAuthStart {
    /// The URL the client opens in the system browser.
    pub authorization_url: String,
    /// The state the provider will echo back.
    pub state: String,
    /// When the request stops being valid.
    pub expires_at: DateTime<Utc>,
}

/// The identity a provider asserted after the code exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentity {
    /// The provider's stable subject.
    pub subject: String,
    /// A verified email address.
    pub email: String,
    /// Display name, if the provider gave one.
    pub display_name: String,
}

/// The outbound half of the flow, injectable so the exchange can be tested against a stub.
#[async_trait::async_trait]
pub trait OAuthExchange: Send + Sync {
    /// Exchange `code` at `token_url` and read the identity from `userinfo_url`.
    async fn exchange(
        &self,
        provider: &ReadyProvider,
        client_secret: &str,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<ProviderIdentity, AuthError>;
}

/// The real exchange over HTTPS.
#[derive(Debug, Default, Clone)]
pub struct HttpExchange;

#[async_trait::async_trait]
impl OAuthExchange for HttpExchange {
    async fn exchange(
        &self,
        provider: &ReadyProvider,
        client_secret: &str,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<ProviderIdentity, AuthError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| AuthError::InvalidToken(format!("oauth client: {error}")))?;
        let token: Value = client
            .post(&provider.config.token_url)
            .header("accept", "application/json")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", provider.client_id.as_str()),
                ("client_secret", client_secret),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await
            .map_err(|error| {
                AuthError::InvalidToken(format!(
                    "{}: token exchange failed: {error}",
                    provider.name
                ))
            })?
            .json()
            .await
            .map_err(|error| {
                AuthError::InvalidToken(format!("{}: token response: {error}", provider.name))
            })?;
        let access = token
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AuthError::InvalidToken(format!(
                    "{}: no access_token in the exchange",
                    provider.name
                ))
            })?;
        let info: Value = client
            .get(&provider.config.userinfo_url)
            .bearer_auth(access)
            .header("accept", "application/json")
            .header("user-agent", "quansio-server")
            .send()
            .await
            .map_err(|error| {
                AuthError::InvalidToken(format!("{}: userinfo failed: {error}", provider.name))
            })?
            .json()
            .await
            .map_err(|error| {
                AuthError::InvalidToken(format!("{}: userinfo response: {error}", provider.name))
            })?;
        identity_from_userinfo(&provider.name, &info)
    }
}

/// Map a provider's userinfo document onto the identity the product needs.
///
/// # Errors
/// [`AuthError::InvalidToken`] when the document lacks a subject or a verified email.
pub fn identity_from_userinfo(provider: &str, info: &Value) -> Result<ProviderIdentity, AuthError> {
    let text = |key: &str| info.get(key).and_then(Value::as_str).map(str::to_string);
    let (subject, email, verified) = match provider {
        "github" => (
            info.get("id")
                .map(|id| id.to_string().trim_matches('"').to_string()),
            text("email"),
            true,
        ),
        _ => (
            text("sub"),
            text("email"),
            info.get("email_verified")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
    };
    let Some(subject) = subject.filter(|s| !s.is_empty()) else {
        return Err(AuthError::InvalidToken(format!(
            "{provider}: userinfo carries no subject"
        )));
    };
    let Some(email) = email.filter(|e| !e.is_empty()) else {
        return Err(AuthError::InvalidToken(format!(
            "{provider}: userinfo carries no email address (a public email is required)"
        )));
    };
    if !verified {
        return Err(AuthError::InvalidToken(format!(
            "{provider}: the email address is not verified"
        )));
    }
    Ok(ProviderIdentity {
        subject,
        email,
        display_name: text("name").or_else(|| text("login")).unwrap_or_default(),
    })
}

impl AuthStore {
    /// The providers offered on this deployment: those with a client id in the environment.
    #[must_use]
    pub fn ready_providers(file: &AuthFile) -> Vec<ReadyProvider> {
        file.providers
            .iter()
            .filter_map(|(name, config)| {
                let client_id = std::env::var(&config.client_id_env).ok()?;
                (!client_id.trim().is_empty()).then(|| ReadyProvider {
                    name: name.clone(),
                    config: config.clone(),
                    client_id: client_id.trim().to_string(),
                })
            })
            .collect()
    }

    /// Start an authorization-code flow; the verifier stays server-side.
    ///
    /// # Errors
    /// [`AuthError::InvalidToken`] for a redirect URI that is not http(s) or a custom scheme.
    pub async fn start_oauth(
        &self,
        provider: &ReadyProvider,
        redirect_uri: &str,
    ) -> Result<OAuthStart, AuthError> {
        if !(redirect_uri.starts_with("https://")
            || redirect_uri.starts_with("http://127.0.0.1")
            || redirect_uri.starts_with("http://localhost")
            || redirect_uri.starts_with("quansio://"))
        {
            return Err(AuthError::InvalidToken(
                "redirect_uri is not an acceptable callback".to_string(),
            ));
        }
        let rng = SystemRandom::new();
        let mut bytes = [0u8; 32];
        rng.fill(&mut bytes)
            .map_err(|_| AuthError::SigningKey("platform RNG failed".to_string()))?;
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        rng.fill(&mut bytes)
            .map_err(|_| AuthError::SigningKey("platform RNG failed".to_string()))?;
        let state = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()));
        let expires_at = Utc::now()
            + chrono::Duration::from_std(self.config().oauth_state_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(10));
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT set_config('quansio.auth_flow', 'on', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO oauth_states (state, provider, pkce_verifier, redirect_uri, expires_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&state)
        .bind(&provider.name)
        .bind(&verifier)
        .bind(redirect_uri)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut url = reqwest::Url::parse(&provider.config.authorization_url)
            .map_err(|error| AuthError::InvalidToken(format!("authorization_url: {error}")))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &provider.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &provider.config.scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(OAuthStart {
            authorization_url: url.to_string(),
            state,
            expires_at,
        })
    }

    /// Complete the flow: consume the state, exchange the code, sign the user in.
    ///
    /// # Errors
    /// [`AuthError::InvalidToken`] for an unknown/consumed state, a redirect mismatch or a failed
    /// exchange; [`AuthError::Expired`] for a stale request.
    pub async fn complete_oauth(
        &self,
        provider: &ReadyProvider,
        exchange: &dyn OAuthExchange,
        code: &str,
        state: &str,
        redirect_uri: &str,
        client: &str,
    ) -> Result<(IssuedSession, SignedInUser), AuthError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT set_config('quansio.auth_flow', 'on', true)")
            .execute(&mut *tx)
            .await?;
        let row: Option<OAuthStateRow> = sqlx::query_as(
            "SELECT provider, pkce_verifier, redirect_uri, expires_at, consumed_at \
                 FROM oauth_states WHERE state = $1",
        )
        .bind(state)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((stored_provider, verifier, stored_redirect, expires_at, consumed_at)) = row
        else {
            return Err(AuthError::InvalidToken(
                "oauth state is not recognized".to_string(),
            ));
        };
        if consumed_at.is_some() {
            return Err(AuthError::InvalidToken(
                "oauth state was already used".to_string(),
            ));
        }
        if stored_provider != provider.name || stored_redirect != redirect_uri {
            return Err(AuthError::InvalidToken(
                "oauth state does not match this request".to_string(),
            ));
        }
        if expires_at <= Utc::now() {
            return Err(AuthError::Expired("oauth request expired".to_string()));
        }
        let consumed = sqlx::query(
            "UPDATE oauth_states SET consumed_at = now() WHERE state = $1 AND consumed_at IS NULL",
        )
        .bind(state)
        .execute(&mut *tx)
        .await?;
        if consumed.rows_affected() == 0 {
            return Err(AuthError::InvalidToken(
                "oauth state was already used".to_string(),
            ));
        }
        tx.commit().await?;
        let client_secret = std::env::var(&provider.config.client_secret_env)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                AuthError::InvalidToken(format!(
                    "{}: client secret {} is not materialized",
                    provider.name, provider.config.client_secret_env
                ))
            })?;
        let identity = exchange
            .exchange(provider, &client_secret, code, &verifier, redirect_uri)
            .await?;
        let user = self
            .find_or_onboard(
                &identity.email,
                &identity.display_name,
                SignUpMethod::OAuth {
                    provider: provider.name.clone(),
                    subject: identity.subject,
                },
            )
            .await?;
        let session = self
            .issue_session(
                &user.user_id,
                &user.tenant_id,
                &format!("oauth_{}", provider.name),
                client,
            )
            .await?;
        Ok((session, user))
    }
}
