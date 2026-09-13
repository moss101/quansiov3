//! The secret broker: opaque handles, and the one boundary where material is resolved (EXEC-007,
//! DOMAIN.md §7.1 `credential.access`, §16).
//!
//! A credential reaches a caller as a **handle** — a `sec_` identity with a provider, a label and a
//! status — and never as material. Material exists in three places only: at the sealed row in
//! `secret_handles`, inside a [`Materialization`] at the approved boundary, and in the memory of
//! whoever the materialization was handed to. Two properties are the point:
//!
//! * **the value cannot be logged, because it has no rendering.** [`SecretMaterial`] prints as a
//!   redacted marker and carries no `Display` or `Serialize`, and [`SecretHandle`] — the type that
//!   travels on the wire — has no field the material could occupy. A secret therefore cannot reach a
//!   model prompt, a tool log or an RPC payload by being formatted into one; the test that scans for a
//!   canary value is a check on that design, not on a filter that has to keep up with it.
//! * **a materialization is fenced, not just time-limited.** It carries the handle's `generation`, so
//!   revoking or rotating the handle invalidates every materialization already handed out — including
//!   one a disconnected worker cached — by a comparison rather than by a search for outstanding copies,
//!   which is not knowable. A materialization is also *scoped*: the connector or target it was issued
//!   for is recorded, and presenting it for another scope is refused.
//!
//! What this does not claim: revocation cannot un-deliver material that was already handed to a
//! worker. It stops the *materialization* being used, and the lifetime bounds how long a delivered
//! value is worth anything; that is why materializations are short-lived rather than convenient.

mod key_provider;

use sqlx::{PgConnection, Row};

pub use key_provider::{
    open, seal, KeyProvider, KmsClient, KmsKeyProvider, LocalMasterKeyProvider, SealedSecret,
    ENVELOPE_VERSION,
};

/// The effect class a materialization settles (DOMAIN.md §7.1: tier 4, always audited).
pub const CREDENTIAL_EFFECT_CLASS: &str = "credential.access";

/// How long a materialization is good for, in seconds, when the caller does not say.
///
/// Short by default: the value is only meant to survive the operation that needed it, and a long
/// materialization is just a copied credential with extra steps.
pub const DEFAULT_MATERIALIZATION_SECONDS: i64 = 60;

/// A handle's lifecycle (the schema's `secret_handles.status` vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStatus {
    /// Usable.
    Active,
    /// Revoked; materializations under it are fenced.
    Revoked,
    /// Past its expiry; materializations under it are fenced.
    Expired,
}

impl SecretStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "revoked" => Some(Self::Revoked),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }

    /// Whether a handle in this state may be materialized.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// A credential handle: everything about a secret except the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHandle {
    /// `sec_…` identity.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace, when it is workspace-scoped.
    pub workspace_id: Option<String>,
    /// Where the credential comes from (`github`, `openai`, …).
    pub provider: String,
    /// Human label.
    pub label: String,
    /// Which key-encryption key sealed it.
    pub data_key_ref: Option<String>,
    /// Lifecycle state.
    pub status: SecretStatus,
    /// The fence: bumped by rotation and revocation.
    pub generation: i64,
    /// When it stops being usable, when it is scheduled to.
    pub expires_at: Option<String>,
    /// Last materialization, or never.
    pub last_used_at: Option<String>,
}

impl SecretHandle {
    /// Whether the handle may be materialized at `now`.
    ///
    /// # Errors
    /// Returns the refusal naming the rule: a revoked handle, an expired one, or one whose scheduled
    /// expiry has passed. A scheduled expiry is checked here rather than only by the sweep, so the
    /// answer does not depend on the sweep having run.
    pub fn usable_at(&self, now: &str) -> Result<(), SecretError> {
        match self.status {
            SecretStatus::Revoked => Err(SecretError::HandleRevoked {
                handle_id: self.id.clone(),
            }),
            SecretStatus::Expired => Err(SecretError::HandleExpired {
                handle_id: self.id.clone(),
            }),
            SecretStatus::Active => match &self.expires_at {
                Some(expires_at) if expires_at.as_str() <= now => Err(SecretError::HandleExpired {
                    handle_id: self.id.clone(),
                }),
                _ => Ok(()),
            },
        }
    }
}

/// The registration of a new secret.
#[derive(Debug, Clone)]
pub struct NewSecret<'a> {
    /// `sec_…` identity, minted by the owner.
    pub id: &'a str,
    /// Owning tenant.
    pub tenant_id: &'a str,
    /// Owning workspace, when workspace-scoped.
    pub workspace_id: Option<&'a str>,
    /// Where the credential comes from.
    pub provider: &'a str,
    /// Human label.
    pub label: &'a str,
    /// When it stops being usable, when that is scheduled.
    pub expires_at: Option<&'a str>,
}

/// Where a materialization may be used. The scope is what a materialized value is bound to, so a
/// credential fetched for one connector cannot be presented to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializationScope {
    /// A connector instance.
    Connector {
        /// `cnx_…` identity.
        connector_id: String,
    },
    /// An execution target.
    Target {
        /// `tgt_…` identity.
        target_id: String,
    },
    /// A tool call.
    Tool {
        /// Registered tool name.
        tool: String,
    },
}

impl MaterializationScope {
    /// Canonical `kind:value` form, for the stored and logged record.
    #[must_use]
    pub fn as_str(&self) -> String {
        match self {
            Self::Connector { connector_id } => format!("connector:{connector_id}"),
            Self::Target { target_id } => format!("target:{target_id}"),
            Self::Tool { tool } => format!("tool:{tool}"),
        }
    }
}

/// The raw material, at the boundary and nowhere else.
///
/// There is deliberately no `Display`, no `Serialize` and no `Clone`: the only way to get the bytes is
/// [`Self::expose`], which names the act, and every rendering of a materialization is redacted.
#[derive(PartialEq, Eq)]
pub struct SecretMaterial(Vec<u8>);

impl SecretMaterial {
    /// Take the bytes. The caller is at the boundary; nothing else should be.
    ///
    /// The returned slice borrows, so the material is not copied into a second owned buffer that would
    /// outlive the materialization.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// How long the material is, which is safe to log.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the material is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretMaterial {
    /// Never renders the value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretMaterial(<{} bytes redacted>)", self.0.len())
    }
}

/// A materialized credential: the value, the scope it is bound to, and the fence it was issued under.
pub struct Materialization {
    /// The handle it came from.
    pub handle_id: String,
    /// The value. Redacted in every rendering.
    pub material: SecretMaterial,
    /// What it may be used for.
    pub scope: MaterializationScope,
    /// The handle generation it was issued under; the fence.
    pub generation: i64,
    /// When it stops being usable, always set: materializations are short-lived.
    pub expires_at: String,
}

impl std::fmt::Debug for Materialization {
    /// Renders everything except the value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Materialization")
            .field("handle_id", &self.handle_id)
            .field("material", &self.material)
            .field("scope", &self.scope.as_str())
            .field("generation", &self.generation)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The audit record of one materialization. This is what the audit owner persists; the broker does not
/// write an audit store of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretAccess {
    /// The handle.
    pub handle_id: String,
    /// The tenant.
    pub tenant_id: String,
    /// Who asked.
    pub actor: String,
    /// The `credential.access` effect this was resolved for.
    pub effect_id: String,
    /// What it was scoped to.
    pub scope: String,
    /// The handle generation it was issued under.
    pub generation: i64,
    /// When.
    pub at: String,
    /// Whether the request was allowed or refused, and by which rule.
    pub decision: String,
}

/// Why a secret operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The database refused or was unreachable.
    #[error("secret broker database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The master key is not usable.
    #[error("the master key is not usable: {detail}")]
    MasterKeyInvalid {
        /// What is wrong with it.
        detail: String,
    },
    /// The key provider refused.
    #[error("the key provider refused: {detail}")]
    KeyProvider {
        /// What the provider said.
        detail: String,
    },
    /// The stored frame is from a version this broker does not know.
    #[error("secret envelope version {version} is not one this broker writes")]
    EnvelopeUnsupported {
        /// The version found.
        version: u8,
    },
    /// The stored frame is structurally wrong.
    #[error("the secret envelope is malformed: {detail}")]
    EnvelopeMalformed {
        /// What is wrong with it.
        detail: String,
    },
    /// The material did not authenticate under its own envelope.
    #[error("the secret material did not authenticate under its envelope")]
    SecretMaterialUnreadable,
    /// The tenant has no such handle.
    #[error("secret handle {0} does not exist for this tenant")]
    HandleNotFound(String),
    /// The handle was revoked.
    #[error("secret handle {handle_id} is revoked")]
    HandleRevoked {
        /// The handle.
        handle_id: String,
    },
    /// The handle expired.
    #[error("secret handle {handle_id} is expired")]
    HandleExpired {
        /// The handle.
        handle_id: String,
    },
    /// The materialization is beyond its own lifetime.
    #[error("the materialization lapsed at {expires_at}")]
    MaterializationExpired {
        /// When it lapsed.
        expires_at: String,
    },
    /// The handle moved on: revoked or rotated after the materialization was issued.
    #[error("the materialization was issued at generation {issued}, the handle is at {current}")]
    FencedStaleGeneration {
        /// The generation the materialization recorded.
        issued: i64,
        /// The handle's generation now.
        current: i64,
    },
    /// The materialization was presented for something other than what it was issued for.
    #[error("the materialization is scoped to {issued}, not to {presented}")]
    ScopeSubstitution {
        /// The scope it was issued for.
        issued: String,
        /// The scope it was presented for.
        presented: String,
    },
    /// The handle has no material stored.
    #[error("secret handle {0} has no material")]
    NoMaterial(String),
    /// The handle identity is not a `sec_` id.
    #[error("{0:?} is not a secret handle id")]
    HandleIdInvalid(String),
    /// The handle is scoped to another workspace.
    #[error("secret handle {handle_id} belongs to workspace {owner}, not {asked}")]
    WorkspaceMismatch {
        /// The handle.
        handle_id: String,
        /// The workspace that owns it.
        owner: String,
        /// The workspace asking.
        asked: String,
    },
}

const COLUMNS: &str = "id, tenant_id, workspace_id, provider, label, data_key_ref, status, generation, \
                       to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at, \
                       to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS last_used_at";

/// The secret broker's durable operations.
pub struct SecretBroker;

impl SecretBroker {
    /// Register a secret, sealing its material before it reaches the database.
    ///
    /// # Errors
    /// Returns [`SecretError`] when the id is not a `sec_` id, the provider refuses to wrap, or the
    /// write fails. The material is sealed first, so a plaintext value is never part of an INSERT.
    pub async fn register(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        secret: &NewSecret<'_>,
        material: &[u8],
        entropy: &dyn quansio_core::EntropySource,
    ) -> Result<SecretHandle, SecretError> {
        if !secret.id.starts_with("sec_") {
            return Err(SecretError::HandleIdInvalid(secret.id.to_string()));
        }
        let sealed = seal(provider, material, entropy)?;
        sqlx::query(
            "INSERT INTO secret_handles (id, tenant_id, workspace_id, provider, label, ciphertext, \
                                         data_key_ref, status, generation, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'active', 1, $8::timestamptz)",
        )
        .bind(secret.id)
        .bind(secret.tenant_id)
        .bind(secret.workspace_id)
        .bind(secret.provider)
        .bind(secret.label)
        .bind(&sealed.ciphertext)
        .bind(&sealed.key_ref)
        .bind(secret.expires_at)
        .execute(&mut *conn)
        .await?;
        Self::load(conn, secret.tenant_id, secret.id).await
    }

    /// Load a handle's metadata. The material is not loaded and cannot be.
    ///
    /// # Errors
    /// Returns [`SecretError::HandleNotFound`] when the tenant has no such handle.
    pub async fn load(
        conn: &mut PgConnection,
        tenant_id: &str,
        handle_id: &str,
    ) -> Result<SecretHandle, SecretError> {
        let sql = format!("SELECT {COLUMNS} FROM secret_handles WHERE id = $1 AND tenant_id = $2");
        let row = sqlx::query(&sql)
            .bind(handle_id)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| SecretError::HandleNotFound(handle_id.to_string()))?;
        row_to_handle(&row)
    }

    /// Every handle of a tenant, newest first.
    ///
    /// # Errors
    /// Returns [`SecretError::Database`] when the read fails.
    pub async fn list(
        conn: &mut PgConnection,
        tenant_id: &str,
    ) -> Result<Vec<SecretHandle>, SecretError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM secret_handles WHERE tenant_id = $1 ORDER BY created_at DESC, id"
        );
        let rows = sqlx::query(&sql)
            .bind(tenant_id)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter().map(row_to_handle).collect()
    }

    /// Resolve a handle's material at the approved boundary, and record the access.
    ///
    /// This is the only operation that returns material. It refuses a handle that is not usable, checks
    /// the workspace scope, and writes `last_used_at` in the same transaction as the read, so an access
    /// that happened is an access that is recorded.
    ///
    /// # Errors
    /// Returns [`SecretError::HandleNotFound`], [`SecretError::HandleRevoked`],
    /// [`SecretError::HandleExpired`] or [`SecretError::WorkspaceMismatch`] for a request the broker
    /// refuses, and [`SecretError`] when the frame does not open.
    pub async fn materialize(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        request: &MaterializeRequest<'_>,
    ) -> Result<(Materialization, SecretAccess), SecretError> {
        let handle = Self::load(conn, request.tenant_id, request.handle_id).await?;
        handle.usable_at(request.at)?;
        if let (Some(owner), Some(asked)) = (&handle.workspace_id, request.workspace_id) {
            if owner != asked {
                return Err(SecretError::WorkspaceMismatch {
                    handle_id: handle.id.clone(),
                    owner: owner.clone(),
                    asked: asked.to_string(),
                });
            }
        }

        let ciphertext: Option<Vec<u8>> =
            sqlx::query("SELECT ciphertext FROM secret_handles WHERE id = $1 AND tenant_id = $2")
                .bind(request.handle_id)
                .bind(request.tenant_id)
                .fetch_one(&mut *conn)
                .await?
                .try_get("ciphertext")?;
        let frame = ciphertext.ok_or_else(|| SecretError::NoMaterial(handle.id.clone()))?;
        let material = open(provider, &frame)?;

        sqlx::query(
            "UPDATE secret_handles SET last_used_at = $3::timestamptz \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(request.handle_id)
        .bind(request.tenant_id)
        .bind(request.at)
        .execute(&mut *conn)
        .await?;

        let scope = request.scope.as_str();
        let access = SecretAccess {
            handle_id: handle.id.clone(),
            tenant_id: handle.tenant_id.clone(),
            actor: request.actor.to_string(),
            effect_id: request.effect_id.to_string(),
            scope: scope.clone(),
            generation: handle.generation,
            at: request.at.to_string(),
            decision: "allowed".to_string(),
        };
        Ok((
            Materialization {
                handle_id: handle.id,
                material: SecretMaterial(material),
                scope: request.scope.clone(),
                generation: handle.generation,
                expires_at: request.expires_at.to_string(),
            },
            access,
        ))
    }

    /// Whether a materialization may still be used, for the caller that cached one.
    ///
    /// The check is against the handle as it is *now*, which is the whole point: the fence moves when
    /// the handle is revoked or rotated, so a worker holding a materialization from before the change
    /// is refused rather than trusted. A disconnected worker cannot be told, so it has to ask.
    ///
    /// # Errors
    /// Returns [`SecretError::FencedStaleGeneration`] when the handle moved past the materialization's
    /// generation, [`SecretError::HandleRevoked`] or [`SecretError::HandleExpired`] when the handle is
    /// no longer usable, [`SecretError::MaterializationExpired`] when the materialization's own
    /// lifetime lapsed, and [`SecretError::ScopeSubstitution`] when it is presented for another scope.
    pub async fn validate_materialization(
        conn: &mut PgConnection,
        tenant_id: &str,
        materialization: &Materialization,
        presented_for: &MaterializationScope,
        now: &str,
    ) -> Result<(), SecretError> {
        if materialization.expires_at.as_str() <= now {
            return Err(SecretError::MaterializationExpired {
                expires_at: materialization.expires_at.clone(),
            });
        }
        if &materialization.scope != presented_for {
            return Err(SecretError::ScopeSubstitution {
                issued: materialization.scope.as_str(),
                presented: presented_for.as_str(),
            });
        }
        let handle = Self::load(conn, tenant_id, &materialization.handle_id).await?;
        handle.usable_at(now)?;
        if handle.generation != materialization.generation {
            return Err(SecretError::FencedStaleGeneration {
                issued: materialization.generation,
                current: handle.generation,
            });
        }
        Ok(())
    }

    /// Revoke a handle.
    ///
    /// The status change and the generation bump are one statement, so a write that revokes is also the
    /// write that fences: there is no instant in which the handle is revoked but its materializations
    /// still validate.
    ///
    /// # Errors
    /// Returns [`SecretError::HandleNotFound`] when the tenant has no such handle.
    pub async fn revoke(
        conn: &mut PgConnection,
        tenant_id: &str,
        handle_id: &str,
    ) -> Result<SecretHandle, SecretError> {
        let updated = sqlx::query(
            "UPDATE secret_handles SET status = 'revoked', generation = generation + 1 \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(handle_id)
        .bind(tenant_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(SecretError::HandleNotFound(handle_id.to_string()));
        }
        Self::load(conn, tenant_id, handle_id).await
    }

    /// Rotate a handle's material, keeping its identity.
    ///
    /// Rotation re-seals under a fresh data key and bumps the generation, so every materialization
    /// issued before it is fenced — a rotation that left old materializations valid would defeat the
    /// reason for rotating.
    ///
    /// # Errors
    /// Returns [`SecretError::HandleNotFound`] when the tenant has no such handle, and
    /// [`SecretError`] when the provider refuses to wrap.
    pub async fn rotate(
        conn: &mut PgConnection,
        provider: &dyn KeyProvider,
        tenant_id: &str,
        handle_id: &str,
        material: &[u8],
        entropy: &dyn quansio_core::EntropySource,
    ) -> Result<SecretHandle, SecretError> {
        let sealed = seal(provider, material, entropy)?;
        let updated = sqlx::query(
            "UPDATE secret_handles SET ciphertext = $3, data_key_ref = $4, \
                                        generation = generation + 1 \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(handle_id)
        .bind(tenant_id)
        .bind(&sealed.ciphertext)
        .bind(&sealed.key_ref)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(SecretError::HandleNotFound(handle_id.to_string()));
        }
        Self::load(conn, tenant_id, handle_id).await
    }

    /// Move every active handle past its scheduled expiry to `expired`, fencing it as it goes.
    ///
    /// # Errors
    /// Returns [`SecretError::Database`] when the write fails.
    pub async fn expire_due(
        conn: &mut PgConnection,
        tenant_id: &str,
        now: &str,
    ) -> Result<u64, SecretError> {
        let expired = sqlx::query(
            "UPDATE secret_handles SET status = 'expired', generation = generation + 1 \
             WHERE tenant_id = $1 AND status = 'active' AND expires_at IS NOT NULL \
               AND expires_at <= $2::timestamptz",
        )
        .bind(tenant_id)
        .bind(now)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        Ok(expired)
    }
}

/// One materialization request.
#[derive(Debug)]
pub struct MaterializeRequest<'a> {
    /// The tenant.
    pub tenant_id: &'a str,
    /// The workspace asking, when the request is workspace-scoped.
    pub workspace_id: Option<&'a str>,
    /// The handle to resolve.
    pub handle_id: &'a str,
    /// Who is asking.
    pub actor: &'a str,
    /// The `credential.access` effect this resolves for.
    pub effect_id: &'a str,
    /// What the material may be used for.
    pub scope: MaterializationScope,
    /// When the materialization lapses (canonical ISO-8601 UTC).
    pub expires_at: &'a str,
    /// The instant of the request (canonical ISO-8601 UTC).
    pub at: &'a str,
}

impl<'a> MaterializeRequest<'a> {
    /// The instant a materialization starting at `at` and lasting `seconds` lapses at.
    ///
    /// A helper rather than a constructor: the expiry has to outlive the request that borrows it, so
    /// the caller computes it and owns it.
    ///
    /// # Errors
    /// Returns [`SecretError::EnvelopeMalformed`] when `at` is not a canonical
    /// `YYYY-MM-DDTHH:MM:SSZ`, because an expiry computed from a malformed instant would be nonsense
    /// rather than merely wrong.
    pub fn expiry_after(at: &str, seconds: i64) -> Result<String, SecretError> {
        add_seconds(at, seconds)
    }
}

/// Add `seconds` to a canonical instant, staying in the canonical shape.
///
/// # Errors
/// Returns [`SecretError::EnvelopeMalformed`] when `at` is not canonical, or when the result would
/// leave the four-digit-year range the canonical shape can express.
pub fn add_seconds(at: &str, seconds: i64) -> Result<String, SecretError> {
    let malformed = || SecretError::EnvelopeMalformed {
        detail: format!("{at:?} is not a canonical instant"),
    };
    if at.len() != 20 || !at.ends_with('Z') {
        return Err(malformed());
    }
    let year: i64 = at[0..4].parse().map_err(|_| malformed())?;
    let month: i64 = at[5..7].parse().map_err(|_| malformed())?;
    let day: i64 = at[8..10].parse().map_err(|_| malformed())?;
    let hour: i64 = at[11..13].parse().map_err(|_| malformed())?;
    let minute: i64 = at[14..16].parse().map_err(|_| malformed())?;
    let second: i64 = at[17..19].parse().map_err(|_| malformed())?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(malformed());
    }
    // Days from the civil epoch, then back: enough arithmetic for a lifetime, and it refuses to invent
    // a calendar.
    let days = days_from_civil(year, month, day);
    let total = days * 86_400 + hour * 3600 + minute * 60 + second + seconds;
    let (days, rest) = (total.div_euclid(86_400), total.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    if !(0..=9999).contains(&year) {
        return Err(malformed());
    }
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    ))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn row_to_handle(row: &sqlx::postgres::PgRow) -> Result<SecretHandle, SecretError> {
    let status: String = row.try_get("status")?;
    Ok(SecretHandle {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        provider: row.try_get("provider")?,
        label: row.try_get("label")?,
        data_key_ref: row.try_get("data_key_ref")?,
        status: SecretStatus::parse(&status).ok_or_else(|| SecretError::EnvelopeMalformed {
            detail: format!("stored status {status:?} is not one the domain defines"),
        })?,
        generation: row.try_get("generation")?,
        expires_at: row.try_get("expires_at")?,
        last_used_at: row.try_get("last_used_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quansio_core::EntropySource;

    /// Deterministic entropy, so a seal in a test is reproducible while the shipped path uses the OS.
    struct CountingEntropy {
        next: std::sync::atomic::AtomicU8,
    }

    impl EntropySource for CountingEntropy {
        fn fill(&self, buf: &mut [u8]) {
            use std::sync::atomic::Ordering;
            for byte in buf.iter_mut() {
                *byte = self.next.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn entropy() -> CountingEntropy {
        CountingEntropy {
            next: std::sync::atomic::AtomicU8::new(7),
        }
    }

    fn provider(seed: u8) -> LocalMasterKeyProvider {
        LocalMasterKeyProvider::from_master_key(&[seed; 32], format!("local:kek-v{seed}"))
            .expect("a 32-byte master key")
    }

    // ------------------------------------------------------------------ envelope

    #[test]
    fn a_sealed_secret_opens_under_its_own_provider_and_under_no_other() {
        let mine = provider(1);
        let theirs = provider(2);
        let material = b"ghp_a_github_token_value";
        let sealed = seal(&mine, material, &entropy()).expect("seal");
        assert_eq!(sealed.key_ref, "local:kek-v1");
        assert_eq!(
            open(&mine, &sealed.ciphertext).expect("open"),
            material.to_vec()
        );

        // Another master key cannot open it, and neither can it unwrap the data key.
        assert!(open(&theirs, &sealed.ciphertext).is_err());

        // The frame does not contain the material in the clear, which is the entire point.
        assert!(
            !sealed
                .ciphertext
                .windows(material.len())
                .any(|window| window == material),
            "the plaintext is in the frame"
        );
    }

    #[test]
    fn a_frame_that_was_changed_does_not_open() {
        let key = provider(3);
        let sealed = seal(&key, b"a-token", &entropy()).expect("seal");

        // A changed byte of sealed material fails authentication.
        let mut tampered = sealed.ciphertext.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(matches!(
            open(&key, &tampered),
            Err(SecretError::SecretMaterialUnreadable)
        ));

        // A changed byte of the wrapped data key cannot be unwrapped.
        let mut rekeyed = sealed.ciphertext.clone();
        rekeyed[key_provider::HEADER_BYTES] ^= 0x01;
        assert!(matches!(
            open(&key, &rekeyed),
            Err(SecretError::KeyProvider { .. })
        ));

        // The version is checked before anything is decrypted.
        let mut future = sealed.ciphertext.clone();
        future[0] = 9;
        assert!(matches!(
            open(&key, &future),
            Err(SecretError::EnvelopeUnsupported { version: 9 })
        ));

        // A truncated frame is refused rather than interpreted.
        assert!(matches!(
            open(&key, &sealed.ciphertext[..4]),
            Err(SecretError::EnvelopeMalformed { .. })
        ));
        let mut short = sealed.ciphertext.clone();
        short.truncate(key_provider::HEADER_BYTES + 2);
        assert!(matches!(
            open(&key, &short),
            Err(SecretError::EnvelopeMalformed { .. })
        ));
    }

    #[test]
    fn sealing_twice_produces_different_frames_for_the_same_secret() {
        // A fresh data key and nonce per seal: a frame is not a fingerprint of the value it holds, so
        // two handles for the same credential are not linkable by comparing ciphertext.
        let key = provider(4);
        let first = seal(&key, b"same-value", &entropy()).expect("seal");
        let second = seal(&key, b"same-value", &entropy()).expect("seal");
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_eq!(
            open(&key, &first.ciphertext).expect("open"),
            open(&key, &second.ciphertext).expect("open")
        );
        assert!(
            first.ciphertext.len() > 16,
            "the frame carries its envelope"
        );
    }

    #[test]
    fn a_master_key_file_that_is_not_a_key_is_refused() {
        for bytes in [&b"short"[..], &[0u8; 31][..], &[0u8; 33][..], &[][..]] {
            assert!(matches!(
                LocalMasterKeyProvider::from_master_key(bytes, "local:kek"),
                Err(SecretError::MasterKeyInvalid { .. })
            ));
        }
        // A provider never renders its key.
        let debug = format!("{:?}", provider(5));
        assert!(debug.contains("redacted"), "{debug}");
        assert!(!debug.contains("5, 5, 5"), "{debug}");
    }

    // ------------------------------------------------------------------ the log

    #[test]
    fn material_cannot_be_rendered_into_a_log() {
        let material = SecretMaterial(b"canary-value-9f3a".to_vec());
        let shown = format!("{material:?}");
        assert!(shown.contains("redacted"), "{shown}");
        assert!(!shown.contains("canary-value-9f3a"), "{shown}");
        assert_eq!(material.len(), 17);

        let materialization = Materialization {
            handle_id: "sec_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            material,
            scope: MaterializationScope::Connector {
                connector_id: "cnx_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            },
            generation: 3,
            expires_at: "2026-09-13T10:01:00Z".to_string(),
        };
        let shown = format!("{materialization:?}");
        assert!(!shown.contains("canary-value-9f3a"), "{shown}");
        assert!(shown.contains("connector:cnx_"), "{shown}");
        assert!(shown.contains("generation: 3"), "{shown}");
        assert!(shown.contains("expires_at"), "{shown}");

        // And the access record, which is the thing that gets persisted and logged.
        let access = SecretAccess {
            handle_id: "sec_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            actor: "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            scope: "connector:cnx_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            generation: 3,
            at: "2026-09-13T10:00:00Z".to_string(),
            decision: "allowed".to_string(),
        };
        let shown = format!("{access:?}");
        assert!(!shown.contains("canary-value-9f3a"), "{shown}");
        assert!(shown.contains("decision: \"allowed\""), "{shown}");
    }

    #[test]
    fn a_handle_is_usable_only_while_it_is_active_and_unexpired() {
        let handle = |status, expires_at: Option<&str>| SecretHandle {
            id: "sec_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            workspace_id: None,
            provider: "github".to_string(),
            label: "deploy key".to_string(),
            data_key_ref: Some("local:kek-v1".to_string()),
            status,
            generation: 1,
            expires_at: expires_at.map(str::to_string),
            last_used_at: None,
        };
        const NOW: &str = "2026-09-13T10:00:00Z";

        assert!(handle(SecretStatus::Active, None).usable_at(NOW).is_ok());
        assert!(handle(SecretStatus::Active, Some("2026-09-13T10:00:01Z"))
            .usable_at(NOW)
            .is_ok());
        // Expiry is exclusive, whether it is scheduled or already swept.
        assert!(matches!(
            handle(SecretStatus::Active, Some(NOW)).usable_at(NOW),
            Err(SecretError::HandleExpired { .. })
        ));
        assert!(matches!(
            handle(SecretStatus::Expired, None).usable_at(NOW),
            Err(SecretError::HandleExpired { .. })
        ));
        assert!(matches!(
            handle(SecretStatus::Revoked, None).usable_at(NOW),
            Err(SecretError::HandleRevoked { .. })
        ));
        assert_eq!(SecretStatus::parse("revoked"), Some(SecretStatus::Revoked));
        assert_eq!(SecretStatus::parse("unknown"), None);
        assert!(SecretStatus::Active.is_usable());
        assert!(!SecretStatus::Revoked.is_usable());
    }

    // ------------------------------------------------------------------ instants

    #[test]
    fn a_lifetime_is_added_in_the_canonical_shape() {
        let cases: [(&str, i64, &str); 10] = [
            ("2026-09-13T10:00:00Z", 60, "2026-09-13T10:01:00Z"),
            ("2026-09-13T10:00:59Z", 1, "2026-09-13T10:01:00Z"),
            ("2026-09-13T10:59:59Z", 1, "2026-09-13T11:00:00Z"),
            ("2026-09-13T23:59:59Z", 1, "2026-09-14T00:00:00Z"),
            ("2026-09-30T23:59:59Z", 1, "2026-10-01T00:00:00Z"),
            ("2026-12-31T23:59:59Z", 1, "2027-01-01T00:00:00Z"),
            // A leap day, and the day after it.
            ("2028-02-28T23:59:59Z", 1, "2028-02-29T00:00:00Z"),
            ("2028-02-29T23:59:59Z", 1, "2028-03-01T00:00:00Z"),
            // A non-leap century.
            ("2100-02-28T23:59:59Z", 1, "2100-03-01T00:00:00Z"),
            // Backwards, because a negative lifetime is arithmetic rather than an error.
            ("2026-09-13T10:00:00Z", -60, "2026-09-13T09:59:00Z"),
        ];
        for (at, seconds, expected) in cases {
            assert_eq!(
                add_seconds(at, seconds).expect("canonical"),
                expected,
                "{at} {seconds}"
            );
        }
        assert_eq!(
            MaterializeRequest::expiry_after(
                "2026-09-13T10:00:00Z",
                DEFAULT_MATERIALIZATION_SECONDS
            )
            .expect("canonical"),
            "2026-09-13T10:01:00Z"
        );

        for malformed in [
            "",
            "2026-09-13",
            "2026-09-13T10:00:00",
            "2026-09-13T10:00:00+00:00",
            "2026-13-01T10:00:00Z",
            "2026-09-32T10:00:00Z",
            "2026-09-13T25:00:00Z",
            "not-an-instant-at-all",
        ] {
            assert!(add_seconds(malformed, 60).is_err(), "{malformed}");
        }
    }

    // ------------------------------------------------------------------ the KMS seam

    struct RecordingKms {
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl KmsClient for RecordingKms {
        fn encrypt(&self, key_ref: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
            self.seen
                .lock()
                .expect("lock")
                .push(format!("encrypt:{key_ref}"));
            // A stand-in for the KMS's own operation; the point of the test is that the broker reaches
            // it rather than sealing locally.
            Ok(plaintext.iter().rev().copied().collect())
        }

        fn decrypt(&self, key_ref: &str, ciphertext: &[u8]) -> Result<Vec<u8>, SecretError> {
            self.seen
                .lock()
                .expect("lock")
                .push(format!("decrypt:{key_ref}"));
            Ok(ciphertext.iter().rev().copied().collect())
        }
    }

    #[test]
    fn a_kms_provider_wraps_through_the_kms_and_not_locally() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = KmsKeyProvider::new(
            RecordingKms {
                seen: std::sync::Arc::clone(&seen),
            },
            "arn:aws:kms:eu-west-1:111122223333:key/abcd",
        );
        assert_eq!(
            provider.key_ref(),
            "arn:aws:kms:eu-west-1:111122223333:key/abcd"
        );
        let wrapped = provider.wrap(b"data-key").expect("wrap");
        assert_eq!(wrapped, b"yek-atad".to_vec(), "the KMS did the work");
        assert_eq!(
            provider.unwrap(&wrapped).expect("unwrap"),
            b"data-key".to_vec()
        );
        let calls = seen.lock().expect("lock").clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].starts_with("encrypt:arn:aws:kms"), "{calls:?}");
        assert!(calls[1].starts_with("decrypt:arn:aws:kms"), "{calls:?}");

        // And a secret sealed through the KMS provider opens through it.
        let sealed = seal(&provider, b"a-token", &entropy()).expect("seal");
        assert_eq!(
            sealed.key_ref,
            "arn:aws:kms:eu-west-1:111122223333:key/abcd"
        );
        assert_eq!(
            open(&provider, &sealed.ciphertext).expect("open"),
            b"a-token".to_vec()
        );
    }

    #[test]
    fn a_scope_renders_as_a_kind_and_a_value() {
        assert_eq!(
            MaterializationScope::Tool {
                tool: "connector.github.comment".to_string()
            }
            .as_str(),
            "tool:connector.github.comment"
        );
        assert_eq!(
            MaterializationScope::Target {
                target_id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string()
            }
            .as_str(),
            "target:tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF"
        );
    }
}
