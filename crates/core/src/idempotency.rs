//! Idempotency keys, parameter digests and duplicate-command classification
//! (DOMAIN.md §1.2, §7.2).
//!
//! Replay safety has two halves:
//!
//! * a **command** is idempotent by `command_id`: re-submitting the same id returns the
//!   original result, while the same id with different parameters is a conflict
//!   (`CONFLICT_IDEMPOTENCY_MISMATCH`) rather than a silent second mutation;
//! * an **effect** is idempotent by `idempotency_key`, derived from the effect class
//!   and the stable inputs of the action, so a retry cannot duplicate a consequence.

use core::fmt;
use core::str::FromStr;

// Bind the trait anonymously so it does not collide with this module's `Digest`.
use sha2::digest::Digest as _;
use sha2::Sha256;

use crate::error::CoreError;
use crate::id::CommandId;

/// Hex-encoded SHA-256 digest of canonical, stable inputs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(String);

impl Digest {
    /// Digest the exact bytes supplied by the caller.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hex(&hasher.finalize()))
    }

    /// Digest a structured value encoded as canonical JSON.
    ///
    /// The caller is responsible for canonical encoding (sorted keys, no insignificant
    /// whitespace); this function deliberately does not guess an encoding, because two
    /// encodings of the same value must never produce two different keys.
    #[must_use]
    pub fn of_canonical_json(canonical_json: &str) -> Self {
        Self::of(canonical_json.as_bytes())
    }

    /// The lowercase hex digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Digest {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if valid {
            Ok(Self(s.to_string()))
        } else {
            Err(CoreError::InvalidCursor(s.to_string()))
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0F) as usize] as char);
    }
    out
}

/// An effect-level idempotency key: unique per `(tenant, effect_class, key)`
/// (DOMAIN.md §1.2, §7.2).
///
/// The key binds the effect class, the materialized resource and the digest of the
/// action parameters, so two semantically different actions can never collide while a
/// retry of the same action always maps to the same key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey {
    effect_class: String,
    resource: String,
    params_digest: Digest,
}

impl IdempotencyKey {
    /// Derive a key for one action.
    #[must_use]
    pub fn derive(effect_class: &str, resource: &str, params_digest: Digest) -> Self {
        Self {
            effect_class: effect_class.to_string(),
            resource: resource.to_string(),
            params_digest,
        }
    }

    /// The effect class this key belongs to.
    #[must_use]
    pub fn effect_class(&self) -> &str {
        &self.effect_class
    }

    /// The materialized resource (fs path, domain, connector resource, …).
    #[must_use]
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// The digest of the action parameters.
    #[must_use]
    pub const fn params_digest(&self) -> &Digest {
        &self.params_digest
    }

    /// Canonical wire form: `<effect_class>:<resource>:<digest>`.
    #[must_use]
    pub fn canonical(&self) -> String {
        format!(
            "{}:{}:{}",
            self.effect_class, self.resource, self.params_digest
        )
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Outcome of matching a duplicate command submission against the stored one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateOutcome {
    /// Same `command_id` and same parameters: return the recorded result unchanged.
    Replay,
    /// Same `command_id` but different parameters: refuse with a typed conflict.
    ConflictMismatch(CommandId),
}

/// Classify a duplicate command submission (DOMAIN.md §1.2).
///
/// # Errors
/// Returns [`CoreError::IdempotencyMismatch`] when the parameters differ, which the API
/// surfaces as `CONFLICT_IDEMPOTENCY_MISMATCH`.
pub fn classify_duplicate(
    command_id: &CommandId,
    stored: &Digest,
    incoming: &Digest,
) -> Result<DuplicateOutcome, CoreError> {
    if stored == incoming {
        Ok(DuplicateOutcome::Replay)
    } else {
        Err(CoreError::IdempotencyMismatch {
            command_id: command_id.to_string(),
        })
    }
}
