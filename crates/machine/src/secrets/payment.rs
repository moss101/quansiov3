//! One-time payment inputs (ADP-009, DOMAIN.md §13.9 C).
//!
//! A person who wants the agent to check out supplies the values — a card number, a security code, an expiry, an address — **out
//! of band**: to the runtime, from their own surface, never through a model. This module is the runtime's half. There is no
//! second secret store: each value is an ordinary [`SecretHandle`] (provider `payment_input`), sealed under the tenant's data
//! key, and everything below is expressed with [`SecretBroker`].
//!
//! * **Supplied for one origin, one use.** A handle's label is `<field>@<origin>`. [`PaymentInputs::take`] returns the value and
//!   the origin it was supplied for, and in the same transaction **overwrites** it: the sealed bytes are replaced with a seal of
//!   nothing and the handle is revoked (which bumps its generation, fencing any materialization). A second take of the same
//!   field finds no active handle.
//! * **Latest wins.** Supplying a field again revokes and clears the older value for that workspace and field.
//! * **A value does not wait.** Each carries an expiry (`ttl_seconds`, at most `max_ttl_seconds`). Every supply and take first
//!   clears the workspace's lapsed values, so a card number is not left sealed in the database after its time.
//! * **No value outside the seal.** The function that registers a handle receives the plaintext as bytes and seals it first;
//!   no event, audit entry or row other than the sealed `ciphertext` ever holds it.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use serde::Deserialize;
use sqlx::{PgConnection, Row};

use super::{
    add_seconds, KeyProvider, Materialization, MaterializationScope, MaterializeRequest, NewSecret,
    SecretBroker, SecretError,
};

/// The provider name of a payment input's handle.
pub const PROVIDER: &str = "payment_input";

/// The tool a take is scoped to: the material may be used for nothing else.
pub const TAKEN_FOR: &str = "browser.fill_payment_field";

/// The payment fields a person may supply, in the order the tool's schema lists them. A test holds this to the worker's
/// `PaymentField` and to `config/tools.yaml`'s `enum`.
pub const PAYMENT_FIELDS: [&str; 11] = [
    "card_number",
    "cvc",
    "expiry",
    "expiry_month",
    "expiry_year",
    "card_holder_name",
    "address_line",
    "city",
    "region",
    "postal_code",
    "country",
];

const SHIPPED: &str = include_str!("../../../../config/payment-inputs.yaml");

/// `config/payment-inputs.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentInputConfig {
    /// Always 1.
    pub version: u32,
    /// Off: a supply is refused.
    pub enabled: bool,
    /// Default lifetime of a supplied value.
    pub ttl_seconds: i64,
    /// The longest a caller may ask for.
    pub max_ttl_seconds: i64,
    /// Fields in one supply.
    pub max_fields: usize,
    /// Characters in one value.
    pub max_value_chars: usize,
}

impl PaymentInputConfig {
    /// The shipped configuration.
    ///
    /// # Panics
    /// Panics only if the repository's own `config/payment-inputs.yaml` is invalid, which its tests catch first.
    #[must_use]
    pub fn builtin() -> Self {
        Self::from_yaml(SHIPPED).expect("config/payment-inputs.yaml is valid")
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
        if config.ttl_seconds < 10
            || config.ttl_seconds > config.max_ttl_seconds
            || config.max_ttl_seconds > 900
        {
            return Err(
                "ttl_seconds must be 10 or more and not pass max_ttl_seconds, which is at most 900"
                    .to_string(),
            );
        }
        if config.max_fields == 0
            || config.max_fields > 16
            || config.max_value_chars < 4
            || config.max_value_chars > 256
        {
            return Err("max_fields is 1 to 16 and max_value_chars is 4 to 256".to_string());
        }
        Ok(config)
    }
}

/// Why a payment input could not be supplied or taken.
#[derive(Debug, thiserror::Error)]
pub enum PaymentInputError {
    /// The configuration switches payment inputs off.
    #[error("payment inputs are switched off")]
    Disabled,
    /// The supply breaks a rule (an unknown field, a duplicate, an empty or over-long value, an origin that is not an origin).
    #[error("the supply is not acceptable: {0}")]
    Invalid(String),
    /// Nothing usable was supplied for this field: it was never supplied, was already used, or ran out of time.
    #[error("no payment value is waiting for {field}")]
    Unavailable {
        /// The field asked for.
        field: String,
    },
    /// The broker refused.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The database refused.
    #[error("payment input database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// What a person supplies for one checkout.
#[derive(Debug)]
pub struct Supply<'a> {
    /// The tenant.
    pub tenant_id: &'a str,
    /// The workspace the browser session belongs to: a value is usable only there.
    pub workspace_id: &'a str,
    /// The origin the values are for (`https://shop.example`), and no other.
    pub origin: &'a str,
    /// Field → value, as the person typed them. Each field once.
    pub fields: Vec<(&'a str, &'a [u8])>,
    /// How long they wait, in seconds; the configured default when `None`.
    pub ttl_seconds: Option<i64>,
    /// The instant (canonical ISO-8601 UTC).
    pub at: &'a str,
}

/// A value that was sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supplied {
    /// The field.
    pub field: String,
    /// The handle that holds it sealed. The handle is not the value.
    pub handle_id: String,
    /// When it stops being usable.
    pub expires_at: String,
}

/// What a take returns: the material at the boundary, and the origin it was supplied for.
pub struct Taken {
    /// The material. Redacted in every rendering.
    pub materialization: Materialization,
    /// The origin the person supplied it for.
    pub origin: String,
}

impl std::fmt::Debug for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Taken")
            .field("origin", &self.origin)
            .field("materialization", &self.materialization)
            .finish()
    }
}

/// The origin of a URL or origin string: `scheme://host[:port]`, lowercase, no path, no credentials; `http` only on a loopback host.
fn canonical_origin(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') || authority.contains(char::is_whitespace) {
        return None;
    }
    // Only an origin: a value supplied "for https://shop.example/checkout" is for the origin, but a path-bearing input is
    // refused so a person does not believe a path is part of the binding.
    if rest.len() != authority.len() && rest != format!("{authority}/") {
        return None;
    }
    let authority = authority.to_ascii_lowercase();
    // Plain http carries a card in the clear: it is allowed only for a loopback host, the local test page.
    if scheme == "http" {
        let host = match authority.strip_prefix('[') {
            Some(inner) => format!("[{}", inner.split(']').next().unwrap_or_default()) + "]",
            None => authority.split(':').next().unwrap_or_default().to_string(),
        };
        if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
            return None;
        }
    }
    Some(format!("{scheme}://{authority}"))
}

/// The label of a field's handle.
fn label(field: &str, origin: &str) -> String {
    format!("{field}@{origin}")
}

/// The payment-input store.
pub struct PaymentInputs;

impl PaymentInputs {
    /// Seal the values a person supplies. Older values for the same fields in the workspace are cleared first.
    ///
    /// # Errors
    /// [`PaymentInputError::Disabled`], [`PaymentInputError::Invalid`] and the broker's and database's refusals. Nothing is
    /// sealed unless everything is acceptable.
    pub async fn supply(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        config: &PaymentInputConfig,
        supply: &Supply<'_>,
    ) -> Result<Vec<Supplied>, PaymentInputError> {
        if !config.enabled {
            return Err(PaymentInputError::Disabled);
        }
        let invalid = |detail: String| Err(PaymentInputError::Invalid(detail));
        let Some(origin) = canonical_origin(supply.origin) else {
            return invalid("the values are for one http or https origin".to_string());
        };
        if supply.fields.is_empty() || supply.fields.len() > config.max_fields {
            return invalid(format!(
                "a supply carries 1 to {} fields",
                config.max_fields
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for (field, value) in &supply.fields {
            if !PAYMENT_FIELDS.contains(field) {
                return invalid(format!("{field:?} is not a payment field"));
            }
            if seen.contains(field) {
                return invalid(format!("{field} is supplied twice"));
            }
            seen.push(field);
            let characters = std::str::from_utf8(value).map(|text| text.chars().count());
            match characters {
                Ok(count) if count > 0 && count <= config.max_value_chars => {}
                Ok(_) => {
                    return invalid(format!(
                        "a value is 1 to {} characters",
                        config.max_value_chars
                    ))
                }
                Err(_) => return invalid("a value is text".to_string()),
            }
        }
        let ttl = supply.ttl_seconds.unwrap_or(config.ttl_seconds);
        if ttl < 10 || ttl > config.max_ttl_seconds {
            return invalid(format!(
                "a value waits 10 to {} seconds",
                config.max_ttl_seconds
            ));
        }
        let expires_at = add_seconds(supply.at, ttl)?;
        Self::sweep(
            conn,
            provider,
            supply.tenant_id,
            supply.workspace_id,
            supply.at,
        )
        .await?;

        let mut supplied = Vec::new();
        for (field, value) in &supply.fields {
            // Latest wins: whatever was waiting for this field in this workspace is overwritten.
            for old in Self::waiting(
                conn,
                supply.tenant_id,
                supply.workspace_id,
                Some(field),
                supply.at,
            )
            .await?
            {
                Self::overwrite(conn, provider, supply.tenant_id, &old.0).await?;
            }
            let id =
                CanonicalId::generate(Prefix::SecretHandle, &mut UlidGenerator::new()).to_string();
            let handle = SecretBroker::register(
                conn,
                provider,
                &NewSecret {
                    id: &id,
                    tenant_id: supply.tenant_id,
                    workspace_id: Some(supply.workspace_id),
                    provider: PROVIDER,
                    label: &label(field, &origin),
                    expires_at: Some(&expires_at),
                },
                value,
                &quansio_core::OsEntropy,
            )
            .await?;
            supplied.push(Supplied {
                field: (*field).to_string(),
                handle_id: handle.id,
                expires_at: expires_at.clone(),
            });
        }
        Ok(supplied)
    }

    /// Take the value waiting for a field, once.
    ///
    /// The row is locked, the value is opened for the tool that asked for it, and the stored seal is **overwritten** (re-sealed
    /// empty, revoked) before the transaction ends: a take that returned a value is a take that deleted it.
    ///
    /// # Errors
    /// [`PaymentInputError::Unavailable`] when nothing usable is waiting for the field in this workspace, and the broker's and
    /// database's refusals.
    pub async fn take(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        tenant_id: &str,
        workspace_id: &str,
        field: &str,
        effect_id: &str,
        at: &str,
    ) -> Result<Taken, PaymentInputError> {
        if !PAYMENT_FIELDS.contains(&field) {
            return Err(PaymentInputError::Invalid(format!(
                "{field:?} is not a payment field"
            )));
        }
        Self::sweep(conn, provider, tenant_id, workspace_id, at).await?;
        let locked = sqlx::query(
            "SELECT id, label FROM secret_handles \
             WHERE tenant_id = $1 AND workspace_id = $2 AND provider = $3 AND status = 'active' \
               AND label LIKE $4 AND (expires_at IS NULL OR expires_at > $5::timestamptz) \
             ORDER BY created_at DESC, id DESC LIMIT 1 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(PROVIDER)
        .bind(format!("{field}@%"))
        .bind(at)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(row) = locked else {
            return Err(PaymentInputError::Unavailable {
                field: field.to_string(),
            });
        };
        let handle_id: String = row.try_get("id")?;
        let handle_label: String = row.try_get("label")?;
        let origin = handle_label
            .split_once('@')
            .map(|(_, origin)| origin.to_string())
            .unwrap_or_default();
        let expires_at = add_seconds(at, 30)?;
        let (materialization, _access) = SecretBroker::materialize(
            conn,
            provider,
            &MaterializeRequest {
                tenant_id,
                workspace_id: Some(workspace_id),
                handle_id: &handle_id,
                actor: "payment-input",
                effect_id,
                scope: MaterializationScope::Tool {
                    tool: TAKEN_FOR.to_string(),
                },
                expires_at: &expires_at,
                at,
            },
        )
        .await?;
        Self::overwrite(conn, provider, tenant_id, &handle_id).await?;
        Ok(Taken {
            materialization,
            origin,
        })
    }

    /// Clear every value of the workspace that has run out of time.
    ///
    /// # Errors
    /// The broker's and database's refusals.
    pub async fn sweep(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        tenant_id: &str,
        workspace_id: &str,
        at: &str,
    ) -> Result<u64, PaymentInputError> {
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

    /// The active, unexpired values of a workspace (optionally one field): `(handle id, label)`.
    async fn waiting(
        conn: &mut PgConnection,
        tenant_id: &str,
        workspace_id: &str,
        field: Option<&str>,
        at: &str,
    ) -> Result<Vec<(String, String)>, PaymentInputError> {
        let pattern = field.map_or_else(|| "%".to_string(), |field| format!("{field}@%"));
        let rows = sqlx::query(
            "SELECT id, label FROM secret_handles \
             WHERE tenant_id = $1 AND workspace_id = $2 AND provider = $3 AND status = 'active' AND label LIKE $4 \
               AND (expires_at IS NULL OR expires_at > $5::timestamptz) FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(PROVIDER)
        .bind(pattern)
        .bind(at)
        .fetch_all(&mut *conn)
        .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("label")?)))
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(PaymentInputError::from)
    }

    /// How many values wait for the workspace now: what a surface can show without a value.
    ///
    /// # Errors
    /// The database's refusals.
    pub async fn count(
        conn: &mut PgConnection,
        tenant_id: &str,
        workspace_id: &str,
        at: &str,
    ) -> Result<Vec<String>, PaymentInputError> {
        Ok(Self::waiting(conn, tenant_id, workspace_id, None, at)
            .await?
            .into_iter()
            .map(|(_, label)| label.split('@').next().unwrap_or_default().to_string())
            .collect())
    }

    /// Replace the stored seal with a seal of nothing and revoke the handle.
    async fn overwrite(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        tenant_id: &str,
        handle_id: &str,
    ) -> Result<(), PaymentInputError> {
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

    #[test]
    fn the_shipped_configuration_is_valid_and_bounded() {
        let config = PaymentInputConfig::builtin();
        assert_eq!((config.ttl_seconds, config.max_ttl_seconds), (300, 900));
        let text = SHIPPED.replace("max_ttl_seconds: 900", "max_ttl_seconds: 90000");
        assert!(PaymentInputConfig::from_yaml(&text).is_err());
        let text = SHIPPED.replace("ttl_seconds: 300", "ttl_seconds: 2");
        assert!(PaymentInputConfig::from_yaml(&text).is_err());
    }

    #[test]
    fn a_value_is_for_one_origin_and_nothing_with_a_path_or_credentials() {
        assert_eq!(
            canonical_origin("https://Shop.Example"),
            Some("https://shop.example".to_string())
        );
        assert_eq!(
            canonical_origin("http://127.0.0.1:8080/"),
            Some("http://127.0.0.1:8080".to_string())
        );
        assert_eq!(
            canonical_origin("http://localhost:3000"),
            Some("http://localhost:3000".to_string())
        );
        assert_eq!(
            canonical_origin("http://[::1]:3000"),
            Some("http://[::1]:3000".to_string())
        );
        // Plain http carries a card in the clear: only a loopback host may have it.
        for bad in [
            "http://shop.example",
            "http://127.0.0.1.shop.example",
            "http://localhost.example:80",
            "shop.example",
            "ftp://shop.example",
            "https://u:p@shop.example",
            "https://shop.example/checkout",
            "https://",
            "https://a b",
        ] {
            assert_eq!(canonical_origin(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_field_list_is_the_tools() {
        let tools = include_str!("../../../../config/tools.yaml");
        let start = tools
            .find("name: browser.fill_payment_field")
            .expect("declared");
        let section = &tools[start..start + 4000];
        for field in PAYMENT_FIELDS {
            assert!(section.contains(field), "{field} is not in the tool's enum");
        }
    }
}
