//! Typed policy, RBAC, privacy, sequence-guard and approval failures (DOMAIN.md §15).
//!
//! Every failure variant is a *failure*: there is no variant that means "proceed".
//! Missing or ambiguous security input is reported here so the caller denies rather
//! than allowing (DOMAIN.md §6.3, §7.3, §12).

use quansio_core::CoreError;
use quansio_events::EventError;

use crate::control::schema::SchemaError;

/// Why an approval receipt could not authorize a dispatch (DOMAIN.md §7.3, §16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalFailure {
    /// No server signing key is configured, so no signature can be trusted.
    SignatureKeyMissing,
    /// The receipt's signature does not verify against the server key: tampered.
    SignatureInvalid,
    /// The receipt names a different EffectRecord than the dispatch.
    EffectMismatch {
        /// Effect the dispatch is for.
        expected: String,
        /// Effect the receipt is bound to.
        receipt: String,
    },
    /// The dispatch parameters differ from the parameters the receipt was granted for.
    ParamsChanged {
        /// Digest the dispatch carries.
        expected: String,
        /// Digest the receipt is bound to.
        receipt: String,
    },
    /// The dispatch generation differs from the generation the receipt was granted for.
    GenerationStale {
        /// Generation the dispatch carries.
        expected: u64,
        /// Generation the receipt is bound to.
        receipt: u64,
    },
    /// The receipt is past its expiry.
    Expired,
    /// The receipt is single-use and has already authorized a dispatch.
    AlreadyUsed,
    /// The request is not in the `granted` state.
    RequestNotGranted {
        /// Status the request held.
        status: String,
    },
}

impl ApprovalFailure {
    /// The DOMAIN.md §15 error code this failure maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Expired => "APPROVAL_EXPIRED",
            Self::SignatureKeyMissing
            | Self::SignatureInvalid
            | Self::EffectMismatch { .. }
            | Self::ParamsChanged { .. }
            | Self::GenerationStale { .. }
            | Self::AlreadyUsed
            | Self::RequestNotGranted { .. } => "APPROVAL_INVALID",
        }
    }

    /// The canonical reason string recorded on the decision row.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::SignatureKeyMissing => "signature_key_missing",
            Self::SignatureInvalid => "signature_invalid",
            Self::EffectMismatch { .. } => "effect_mismatch",
            Self::ParamsChanged { .. } => "params_changed",
            Self::GenerationStale { .. } => "generation_stale",
            Self::Expired => "expired",
            Self::AlreadyUsed => "already_used",
            Self::RequestNotGranted { .. } => "request_not_granted",
        }
    }
}

impl core::fmt::Display for ApprovalFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for ApprovalFailure {}

/// A policy-module failure (DOMAIN.md §15).
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// PostgreSQL rejected a statement or the transaction could not complete.
    #[error("policy database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store rejected the commit; nothing was written.
    #[error("policy event store: {0}")]
    Event(#[from] EventError),
    /// The tenant context could not be established.
    #[error("policy schema error: {0}")]
    Schema(#[from] SchemaError),
    /// A canonical identity could not be parsed or generated.
    #[error("policy identity error: {0}")]
    Core(#[from] CoreError),
    /// A capability effect class or resource selector was not canonical.
    #[error("policy capability input error: {0}")]
    Capability(#[from] quansio_capability::CapabilityError),
    /// A JSON value could not be decoded.
    #[error("policy JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// A persisted policy rule is not canonical; the policy is unusable and fails closed.
    #[error("policy {policy_id} has an unparseable rule: {reason}")]
    MalformedRule {
        /// Policy the rule belongs to.
        policy_id: String,
        /// Why the rule could not be parsed.
        reason: String,
    },
    /// A `UserRule` attempts `always` on a tier-4 effect class (DOMAIN.md §7.1).
    #[error("user rule {rule_id} cannot set `always` on tier-4 {effect_class}")]
    TierFourAlwaysRejected {
        /// Rule that was rejected.
        rule_id: String,
        /// Tier-4 effect class it named.
        effect_class: String,
    },
    /// A `UserRule` attempts `always` where policy does not permit it (DOMAIN.md §7.3).
    #[error("user rule {rule_id} cannot widen `ask` to `always` for {effect_class}")]
    UserRuleWideningNotPermitted {
        /// Rule that was rejected.
        rule_id: String,
        /// Effect class it named.
        effect_class: String,
    },
    /// The approved request or receipt does not exist in this tenant.
    #[error("approval {id} not found for tenant {tenant_id}")]
    ApprovalNotFound {
        /// Request or receipt id.
        id: String,
        /// Tenant that was searched.
        tenant_id: String,
    },
    /// The EffectRecord a receipt authorizes does not exist in this tenant.
    #[error("effect {effect_id} not found for tenant {tenant_id}")]
    EffectNotFound {
        /// Effect record id.
        effect_id: String,
        /// Tenant that was searched.
        tenant_id: String,
    },
    /// A receipt failed verification; nothing was written.
    #[error("approval invalid: {0}")]
    ApprovalInvalid(#[from] ApprovalFailure),
    /// The request is no longer pending (granted, denied, expired or superseded).
    #[error("approval request {request_id} is {status}")]
    ApprovalNotPending {
        /// Request id.
        request_id: String,
        /// Status it holds.
        status: String,
    },
    /// A run is not in the state the operation requires.
    #[error("run {run_id} is {status}, expected {expected}")]
    RunStateConflict {
        /// Run id.
        run_id: String,
        /// State it held.
        status: String,
        /// State the operation required.
        expected: String,
    },
    /// A caller-supplied argument is not valid.
    #[error("invalid policy argument: {0}")]
    InvalidArgument(String),
    /// The canonical runtime refused a transition; nothing was written.
    #[error("runtime conflict: {0}")]
    RuntimeConflict(String),
}

impl PolicyError {
    /// The DOMAIN.md §15 error code this error maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ApprovalInvalid(failure) => failure.code(),
            Self::ApprovalNotPending { .. } => "APPROVAL_SUPERSEDED",
            Self::TierFourAlwaysRejected { .. } | Self::UserRuleWideningNotPermitted { .. } => {
                "POLICY_DENIED"
            }
            Self::MalformedRule { .. } | Self::InvalidArgument(_) | Self::Json(_) => {
                "VALIDATION_SCHEMA"
            }
            Self::Capability(_) => "VALIDATION_SCHEMA",
            Self::RunStateConflict { .. } => "CONFLICT_STATE",
            Self::RuntimeConflict(_) => "CONFLICT_STATE",
            Self::ApprovalNotFound { .. } | Self::EffectNotFound { .. } => "NOT_FOUND",
            Self::Database(_) | Self::Event(_) | Self::Schema(_) | Self::Core(_) => "INTERNAL",
        }
    }
}
