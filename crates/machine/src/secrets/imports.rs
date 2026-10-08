//! Staged browser-data imports (ADP-009, DOMAIN.md §13.9 B).
//!
//! A person who brings cookies from another browser into the agent's browser does it in two steps, and this module is the first:
//! **staging**. The bundle (`ImportCookie`s) is sealed as one ordinary [`SecretHandle`] (provider `browser_import`), under the
//! tenant's data key, and waits. Staging is not importing: the second step is the approved `browser.import_session` call
//! (effect class `browser.session.import`, tier 4, always asked), whose arguments carry the bundle's **digest, count and domains**
//! and never a value. The worker takes the bundle with [`BrowserImports::take`], which checks the digest it was approved for,
//! returns the cookies once and **overwrites** the stored seal in the same transaction.
//!
//! There is no second secret store: everything is expressed with [`SecretBroker`], and the label of the handle
//! (`import:<digest>`) is the only thing besides the sealed bytes that names the bundle.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};

use quansio_core::{CanonicalId, Prefix, UlidGenerator};

use super::{
    add_seconds, KeyProvider, MaterializationScope, MaterializeRequest, NewSecret, SecretBroker,
    SecretError,
};

/// The provider name of a staged bundle's handle.
pub const PROVIDER: &str = "browser_import";

/// The tool a take is scoped to: the material may be used for nothing else.
pub const TAKEN_FOR: &str = "browser.import_session";

const SHIPPED: &str = include_str!("../../../../config/browser-import.yaml");

/// `config/browser-import.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportConfig {
    /// Always 1.
    pub version: u32,
    /// Off: staging is refused.
    pub enabled: bool,
    /// How long a staged bundle waits for its approved use.
    pub ttl_seconds: i64,
    /// The most cookies in one bundle.
    pub max_cookies: usize,
    /// The longest cookie value, in bytes.
    pub max_value_bytes: usize,
    /// The largest bundle, in bytes of JSON.
    pub max_bundle_bytes: usize,
    /// How many domains an approval lists before it says "and N more".
    pub max_domains_listed: usize,
}

impl ImportConfig {
    /// The shipped configuration.
    ///
    /// # Panics
    /// Panics only if the repository's own `config/browser-import.yaml` is invalid, which its tests catch first.
    #[must_use]
    pub fn builtin() -> Self {
        Self::from_yaml(SHIPPED).expect("config/browser-import.yaml is valid")
    }

    /// Parse and validate.
    ///
    /// # Errors
    /// Returns the rule that is broken.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let config: Self = serde_yaml::from_str(text).map_err(|error| error.to_string())?;
        if config.version != 1 {
            return Err("version must be 1".to_string());
        }
        if !(60..=3600).contains(&config.ttl_seconds) {
            return Err("ttl_seconds is 60 to 3600".to_string());
        }
        if config.max_cookies == 0
            || config.max_cookies > 100_000
            || !(64..=8192).contains(&config.max_value_bytes)
            || !(4096..=33_554_432).contains(&config.max_bundle_bytes)
            || !(1..=200).contains(&config.max_domains_listed)
        {
            return Err(
                "max_cookies is 1 to 100000, max_value_bytes 64 to 8192, max_bundle_bytes 4096 to 33554432 and max_domains_listed 1 to 200"
                    .to_string(),
            );
        }
        Ok(config)
    }
}

/// A cookie's `SameSite` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SameSite {
    /// `SameSite=None`.
    None,
    /// `SameSite=Lax`.
    Lax,
    /// `SameSite=Strict`.
    Strict,
    /// The attribute was not set.
    Unspecified,
}

/// One cookie of a bundle.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportCookie {
    /// The cookie's name.
    pub name: String,
    /// The cookie's value. Never rendered: [`Debug`] redacts it.
    pub value: String,
    /// The domain, with a leading dot for a domain cookie (`.example.com`).
    pub domain: String,
    /// The path.
    pub path: String,
    /// `Secure`.
    pub secure: bool,
    /// `HttpOnly`.
    pub http_only: bool,
    /// `SameSite`.
    pub same_site: SameSite,
    /// Expiry as Unix seconds; `None` for a session cookie.
    pub expires: Option<i64>,
}

impl std::fmt::Debug for ImportCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportCookie")
            .field("name", &self.name)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field(
                "value",
                &format_args!("<{} bytes redacted>", self.value.len()),
            )
            .finish_non_exhaustive()
    }
}

impl ImportCookie {
    /// The canonical line a digest covers. Fields in a fixed order, the value included: a bundle with one value changed has a
    /// different digest.
    fn canonical(&self) -> String {
        let same_site = match self.same_site {
            SameSite::None => "none",
            SameSite::Lax => "lax",
            SameSite::Strict => "strict",
            SameSite::Unspecified => "unspecified",
        };
        serde_json::json!([
            self.domain,
            self.path,
            self.name,
            self.value,
            self.secure,
            self.http_only,
            same_site,
            self.expires
        ])
        .to_string()
    }
}

/// The digest of a bundle: SHA-256 over the canonical lines of its cookies in `(domain, path, name)` order, as lowercase hex.
/// The same bundle has the same digest in whatever order it was read, and the digest names the bundle without containing it.
#[must_use]
pub fn bundle_digest(cookies: &[ImportCookie]) -> String {
    let mut lines: Vec<(&str, &str, &str, String)> = cookies
        .iter()
        .map(|cookie| {
            (
                cookie.domain.as_str(),
                cookie.path.as_str(),
                cookie.name.as_str(),
                cookie.canonical(),
            )
        })
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    for (_, _, _, line) in &lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// The registrable-looking domains of a bundle, sorted and unique, without the leading dot: what an approval shows.
#[must_use]
pub fn bundle_domains(cookies: &[ImportCookie]) -> Vec<String> {
    let mut domains: Vec<String> = cookies
        .iter()
        .map(|cookie| cookie.domain.trim_start_matches('.').to_ascii_lowercase())
        .collect();
    domains.sort();
    domains.dedup();
    domains
}

/// Why a bundle could not be staged or taken.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The configuration switches imports off.
    #[error("browser-data imports are switched off")]
    Disabled,
    /// The bundle breaks a rule.
    #[error("the bundle is not acceptable: {0}")]
    Invalid(String),
    /// The digest the caller named is not the bundle's.
    #[error("the bundle's digest is not the one named")]
    DigestMismatch,
    /// Nothing usable is staged under this id: never staged, already used, or out of time.
    #[error("no staged bundle is waiting under this id")]
    Unavailable,
    /// The broker refused.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The database refused.
    #[error("browser import database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// A bundle that was sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The handle that holds the bundle sealed. The handle is not the bundle.
    pub import_id: String,
    /// The bundle's digest.
    pub digest: String,
    /// How many cookies.
    pub cookie_count: usize,
    /// The domains, sorted and unique (all of them; the caller truncates for display).
    pub domains: Vec<String>,
    /// When it stops being usable.
    pub expires_at: String,
}

/// What a take returns: the cookies, once.
pub struct Taken {
    /// The cookies. Their values are redacted in every rendering.
    pub cookies: Vec<ImportCookie>,
    /// The digest they were verified against.
    pub digest: String,
}

impl std::fmt::Debug for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Taken")
            .field("cookies", &self.cookies.len())
            .field("digest", &self.digest)
            .finish()
    }
}

/// What a person stages.
#[derive(Debug)]
pub struct StageRequest<'a> {
    /// The tenant.
    pub tenant_id: &'a str,
    /// The workspace whose browser the bundle is for: a bundle is usable only there.
    pub workspace_id: &'a str,
    /// The cookies.
    pub cookies: &'a [ImportCookie],
    /// The digest the person's surface computed; the store recomputes it and refuses a bundle that is not the one named.
    pub claimed_digest: &'a str,
    /// The instant (canonical ISO-8601 UTC).
    pub at: &'a str,
}

/// What the approved call takes.
#[derive(Debug)]
pub struct TakeRequest<'a> {
    /// The tenant.
    pub tenant_id: &'a str,
    /// The workspace of the session the call runs in.
    pub workspace_id: &'a str,
    /// The staged bundle's handle id.
    pub import_id: &'a str,
    /// The digest the approval carried.
    pub digest: &'a str,
    /// The effect (or action) the take belongs to.
    pub effect_id: &'a str,
    /// The instant (canonical ISO-8601 UTC).
    pub at: &'a str,
}

/// The staged-import store.
pub struct BrowserImports;

impl BrowserImports {
    /// Seal a bundle the person staged. Whatever else was staged for the workspace is overwritten first (latest wins).
    ///
    /// # Errors
    /// [`ImportError::Disabled`], [`ImportError::Invalid`], [`ImportError::DigestMismatch`] and the broker's and database's
    /// refusals. Nothing is sealed unless everything is acceptable.
    pub async fn stage(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        config: &ImportConfig,
        request: &StageRequest<'_>,
    ) -> Result<Staged, ImportError> {
        let StageRequest {
            tenant_id,
            workspace_id,
            cookies,
            claimed_digest,
            at,
        } = *request;
        if !config.enabled {
            return Err(ImportError::Disabled);
        }
        if cookies.is_empty() || cookies.len() > config.max_cookies {
            return Err(ImportError::Invalid(format!(
                "a bundle carries 1 to {} cookies",
                config.max_cookies
            )));
        }
        for cookie in cookies {
            if cookie.name.is_empty()
                || cookie.domain.trim_start_matches('.').is_empty()
                || !cookie.path.starts_with('/')
                || cookie
                    .domain
                    .chars()
                    .any(|c| c.is_whitespace() || c == '/' || c == ':' || c.is_control())
                || cookie.name.chars().any(char::is_control)
            {
                return Err(ImportError::Invalid(
                    "a cookie needs a name, a domain (a host, no scheme or port) and a path"
                        .to_string(),
                ));
            }
            if cookie.value.len() > config.max_value_bytes {
                return Err(ImportError::Invalid(format!(
                    "a cookie value is at most {} bytes",
                    config.max_value_bytes
                )));
            }
        }
        let digest = bundle_digest(cookies);
        if digest != claimed_digest {
            return Err(ImportError::DigestMismatch);
        }
        let body =
            serde_json::to_vec(cookies).map_err(|error| ImportError::Invalid(error.to_string()))?;
        if body.len() > config.max_bundle_bytes {
            return Err(ImportError::Invalid(format!(
                "a bundle is at most {} bytes",
                config.max_bundle_bytes
            )));
        }
        let expires_at = add_seconds(at, config.ttl_seconds)?;
        Self::sweep(conn, provider, tenant_id, workspace_id, at).await?;
        for old in Self::waiting(conn, tenant_id, workspace_id, at).await? {
            Self::overwrite(conn, provider, tenant_id, &old).await?;
        }
        let id = CanonicalId::generate(Prefix::SecretHandle, &mut UlidGenerator::new()).to_string();
        let handle = SecretBroker::register(
            conn,
            provider,
            &NewSecret {
                id: &id,
                tenant_id,
                workspace_id: Some(workspace_id),
                provider: PROVIDER,
                label: &format!("import:{digest}"),
                expires_at: Some(&expires_at),
            },
            &body,
            &quansio_core::OsEntropy,
        )
        .await?;
        Ok(Staged {
            import_id: handle.id,
            digest,
            cookie_count: cookies.len(),
            domains: bundle_domains(cookies),
            expires_at,
        })
    }

    /// Take a staged bundle, once, for the call that was approved with `digest`.
    ///
    /// The row is locked, the bundle is opened for `browser.import_session` only, its digest is checked against the one the
    /// approval carried, and the stored seal is **overwritten** before the transaction ends: a take that returned a bundle is
    /// a take that deleted it, including when the digest did not match.
    ///
    /// # Errors
    /// [`ImportError::Unavailable`] when nothing usable is staged under the id in this workspace, [`ImportError::DigestMismatch`]
    /// when the bundle is not the one approved, and the broker's and database's refusals.
    pub async fn take(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        request: &TakeRequest<'_>,
    ) -> Result<Taken, ImportError> {
        let TakeRequest {
            tenant_id,
            workspace_id,
            import_id,
            digest,
            effect_id,
            at,
        } = *request;
        Self::sweep(conn, provider, tenant_id, workspace_id, at).await?;
        let locked = sqlx::query(
            "SELECT id FROM secret_handles \
             WHERE tenant_id = $1 AND workspace_id = $2 AND provider = $3 AND id = $4 AND status = 'active' \
               AND (expires_at IS NULL OR expires_at > $5::timestamptz) FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(PROVIDER)
        .bind(import_id)
        .bind(at)
        .fetch_optional(&mut *conn)
        .await?;
        if locked.is_none() {
            return Err(ImportError::Unavailable);
        }
        let expires_at = add_seconds(at, 30)?;
        let (materialization, _access) = SecretBroker::materialize(
            conn,
            provider,
            &MaterializeRequest {
                tenant_id,
                workspace_id: Some(workspace_id),
                handle_id: import_id,
                actor: "browser-import",
                effect_id,
                scope: MaterializationScope::Tool {
                    tool: TAKEN_FOR.to_string(),
                },
                expires_at: &expires_at,
                at,
            },
        )
        .await?;
        // Spent from here on: whatever the bundle turns out to be, it does not wait for another try.
        Self::overwrite(conn, provider, tenant_id, import_id).await?;
        let cookies: Vec<ImportCookie> = serde_json::from_slice(materialization.material.expose())
            .map_err(|_| {
                ImportError::Invalid("the staged bundle is not a cookie list".to_string())
            })?;
        if bundle_digest(&cookies) != digest {
            return Err(ImportError::DigestMismatch);
        }
        Ok(Taken {
            cookies,
            digest: digest.to_string(),
        })
    }

    /// Overwrite every bundle of the workspace that has run out of time.
    ///
    /// # Errors
    /// The broker's and database's refusals.
    pub async fn sweep(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        tenant_id: &str,
        workspace_id: &str,
        at: &str,
    ) -> Result<u64, ImportError> {
        let lapsed = sqlx::query(
            "SELECT id FROM secret_handles \
             WHERE tenant_id = $1 AND workspace_id = $2 AND provider = $3 AND status = 'active' \
               AND expires_at IS NOT NULL AND expires_at <= $4::timestamptz FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(PROVIDER)
        .bind(at)
        .fetch_all(&mut *conn)
        .await?;
        let mut cleared = 0;
        for row in lapsed {
            let id: String = row.try_get("id")?;
            Self::overwrite(conn, provider, tenant_id, &id).await?;
            cleared += 1;
        }
        Ok(cleared)
    }

    /// The staged, unexpired bundles of a workspace: handle ids.
    async fn waiting(
        conn: &mut PgConnection,
        tenant_id: &str,
        workspace_id: &str,
        at: &str,
    ) -> Result<Vec<String>, ImportError> {
        let rows = sqlx::query(
            "SELECT id FROM secret_handles \
             WHERE tenant_id = $1 AND workspace_id = $2 AND provider = $3 AND status = 'active' \
               AND (expires_at IS NULL OR expires_at > $4::timestamptz) FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(PROVIDER)
        .bind(at)
        .fetch_all(&mut *conn)
        .await?;
        rows.iter()
            .map(|row| row.try_get("id"))
            .collect::<Result<Vec<String>, sqlx::Error>>()
            .map_err(ImportError::from)
    }

    /// Replace the stored seal with a seal of nothing and revoke the handle.
    async fn overwrite(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        tenant_id: &str,
        handle_id: &str,
    ) -> Result<(), ImportError> {
        SecretBroker::rotate(
            conn,
            provider,
            tenant_id,
            handle_id,
            b"",
            &quansio_core::OsEntropy,
        )
        .await?;
        SecretBroker::revoke(conn, tenant_id, handle_id).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: &str, name: &str, value: &str) -> ImportCookie {
        ImportCookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: domain.to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: true,
            same_site: SameSite::Lax,
            expires: Some(1_900_000_000),
        }
    }

    #[test]
    fn the_shipped_configuration_is_valid_and_bounded() {
        let config = ImportConfig::builtin();
        assert_eq!((config.ttl_seconds, config.max_cookies), (900, 5_000));
        for (from, to) in [
            ("ttl_seconds: 900", "ttl_seconds: 90000"),
            ("ttl_seconds: 900", "ttl_seconds: 5"),
            ("max_cookies: 5000", "max_cookies: 0"),
            ("max_value_bytes: 4096", "max_value_bytes: 4"),
        ] {
            assert!(
                ImportConfig::from_yaml(&SHIPPED.replace(from, to)).is_err(),
                "{to}"
            );
        }
    }

    #[test]
    fn a_digest_names_the_bundle_in_any_order_and_changes_with_any_value() {
        let a = cookie("example.test", "session", "s3cr3t");
        let b = cookie(".example.test", "prefs", "dark");
        let forward = bundle_digest(&[a.clone(), b.clone()]);
        assert_eq!(forward, bundle_digest(&[b.clone(), a.clone()]));
        assert_eq!(forward.len(), 64);
        let mut changed = a;
        changed.value = "s3cr3t!".to_string();
        assert_ne!(forward, bundle_digest(&[changed, b]));
        assert!(!forward.contains("s3cr3t"));
    }

    /// The desktop computes the same digest (`apps/desktop/src/main/secrets/bundle.ts`); both tests hold this value.
    pub(crate) fn golden_bundle() -> Vec<ImportCookie> {
        vec![
            ImportCookie {
                name: "session".to_string(),
                value: "v\"a\\l/ü€\u{1F600}".to_string(),
                domain: ".example.test".to_string(),
                path: "/app".to_string(),
                secure: true,
                http_only: true,
                same_site: SameSite::Strict,
                expires: Some(1_900_000_000),
            },
            ImportCookie {
                name: "prefs".to_string(),
                value: "dark".to_string(),
                domain: "example.test".to_string(),
                path: "/".to_string(),
                secure: false,
                http_only: false,
                same_site: SameSite::Unspecified,
                expires: None,
            },
            ImportCookie {
                name: "a".to_string(),
                value: String::new(),
                domain: "\u{00e9}.test".to_string(),
                path: "/".to_string(),
                secure: false,
                http_only: false,
                same_site: SameSite::None,
                expires: Some(0),
            },
        ]
    }

    #[test]
    fn the_digest_of_a_fixed_bundle_is_the_one_the_desktop_computes() {
        assert_eq!(
            bundle_digest(&golden_bundle()),
            "ceda87a0e712adc728b9002e8bffd7a1f5e040e6cc8d96c6b4a4595b437cb2d5",
            "the desktop's bundle.ts must give this same digest"
        );
    }

    #[test]
    fn a_cookie_never_renders_its_value() {
        let text = format!("{:?}", cookie("example.test", "session", "s3cr3t-canary"));
        assert!(!text.contains("s3cr3t-canary"), "{text}");
        assert!(text.contains("redacted"));
    }

    #[test]
    fn domains_are_sorted_unique_and_without_the_leading_dot() {
        let domains = bundle_domains(&[
            cookie(".B.example", "a", "1"),
            cookie("a.example", "a", "1"),
            cookie("b.example", "b", "2"),
        ]);
        assert_eq!(domains, ["a.example", "b.example"]);
    }
}
