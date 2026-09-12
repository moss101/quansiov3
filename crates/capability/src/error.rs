//! Typed capability failures (DOMAIN.md §6.3, §15).
//!
//! Every failure that can hide an authority decision is typed here. `code()` returns
//! the canonical error code from `schemas/catalog/errors.yaml` so a caller cannot map a
//! security failure to a generic database error.

use chrono::{DateTime, Utc};

use crate::algebra::WideningRejection;
use crate::grant::{EffectClass, Tier};
use crate::layer::Layer;
use crate::projection::CapabilityProjection;

/// A capability input could not be resolved, so the projection failed closed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("capability input unavailable at layer {layer}: {reason}")]
pub struct InputsUnavailable {
    /// The layer whose input could not be resolved.
    pub layer: Layer,
    /// Why the input could not be resolved.
    pub reason: InputUnavailableReason,
    /// The empty projection produced in place of a partial one (never grants more).
    pub projection: CapabilityProjection,
}

/// Why one projection layer could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InputUnavailableReason {
    /// The layer's source could not be fetched.
    #[error("source unavailable: {detail}")]
    SourceUnavailable {
        /// Human-readable detail.
        detail: String,
    },
    /// The layer's payload could not be parsed into canonical grants.
    #[error("unparseable input: {detail}")]
    Unparseable {
        /// Human-readable detail.
        detail: String,
    },
    /// A policy layer has no rule for a running grant whose effect class is tier ≥ 1.
    #[error("policy gap for tier {tier}: no rule matches '{effect_class}'")]
    PolicyGap {
        /// The effect class left unmatched.
        effect_class: EffectClass,
        /// The catalog tier of that effect class.
        tier: Tier,
    },
    /// The catalog tier of a running grant's effect class is unknown, so policy cannot
    /// be evaluated fail-closed.
    #[error("unknown catalog tier for effect class '{effect_class}'")]
    UnknownTier {
        /// The effect class without a known tier.
        effect_class: EffectClass,
    },
}

/// Why a projection can no longer authorize dispatch (DOMAIN.md §6.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StaleReason {
    /// The projection is past its `expires_at`.
    #[error("projection expired at {expires_at} (checked at {checked_at})")]
    Expired {
        /// The projection expiry.
        expires_at: DateTime<Utc>,
        /// When the check ran.
        checked_at: DateTime<Utc>,
    },
    /// The current inputs no longer produce the projection's `inputs_digest`.
    #[error("projection inputs changed (expected {expected}, current {actual})")]
    InputsChanged {
        /// The digest recorded on the projection.
        expected: String,
        /// The digest of the current inputs.
        actual: String,
    },
}

/// A stale projection that must be recomputed before it can authorize anything.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("stale capability projection '{projection_id}': {reason}")]
pub struct StaleProjection {
    /// The stale projection identity.
    pub projection_id: String,
    /// Why it is stale.
    pub reason: StaleReason,
}

/// A capability failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    /// A layer input could not be resolved; the projection is empty.
    #[error("{0}")]
    InputsUnavailable(Box<InputsUnavailable>),
    /// The projection is stale and must be recomputed before it can authorize.
    #[error("{0}")]
    StaleProjection(Box<StaleProjection>),
    /// A grant value is not a canonical grant.
    #[error("malformed grant: {0}")]
    MalformedGrant(String),
    /// A resource selector value is not canonical.
    #[error("malformed resource selector ({kind}): {reason}")]
    MalformedSelector {
        /// The offending selector kind.
        kind: String,
        /// Human-readable detail.
        reason: String,
    },
    /// A delegation or narrowing check rejected a widening child.
    #[error("authority widening rejected: {0}")]
    WideningRejected(Box<WideningRejection>),
}

impl CapabilityError {
    /// The canonical error code from `schemas/catalog/errors.yaml`.
    ///
    /// A stale projection maps to `CAPABILITY_INPUTS_UNAVAILABLE`: the authoritative
    /// inputs for the decision are not the current ones, so dispatch must recompute.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InputsUnavailable(_) | Self::StaleProjection(_) => {
                "CAPABILITY_INPUTS_UNAVAILABLE"
            }
            Self::WideningRejected(_) => "CAPABILITY_DENIED",
            Self::MalformedGrant(_) | Self::MalformedSelector { .. } => "VALIDATION_SCHEMA",
        }
    }
}
