//! Typed failures of the Universal Effect Ledger.
//!
//! Every refusal is typed and maps to a code from DOMAIN.md §15. A refusal is returned
//! before the first write or rolls the transaction back, so a refused transition never
//! changes a record or emits an `effect.*` event.

use quansio_capability::CapabilityError;
use quansio_core::CoreError;
use quansio_events::EventError;

use crate::control::schema::SchemaError;

/// An Effect Ledger failure.
#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    /// PostgreSQL rejected a statement or the transaction could not complete.
    #[error("effect database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store rejected the commit; nothing was written.
    #[error("effect event store: {0}")]
    Event(#[from] EventError),
    /// The tenant context could not be established.
    #[error("effect schema error: {0}")]
    Schema(#[from] SchemaError),
    /// A canonical identity could not be parsed or generated.
    #[error("effect identity error: {0}")]
    Core(#[from] CoreError),
    /// A capability input was malformed.
    #[error("effect capability error: {0}")]
    Capability(#[from] CapabilityError),
    /// A JSON column or payload could not be decoded.
    #[error("effect JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// `config/effects.yaml` could not be read or parsed.
    #[error("effect taxonomy error: {0}")]
    Taxonomy(String),
    /// A stored status is not one of DOMAIN.md §7.2.
    #[error("unknown effect status {0}")]
    UnknownStatus(String),
    /// A stored target kind is not one of DOMAIN.md §7.2.
    #[error("unknown effect target kind {0}")]
    UnknownTargetKind(String),
    /// A reconciliation strategy cell could not be classified.
    #[error("unknown reconciliation strategy {0}")]
    UnknownReconciliationStrategy(String),
    /// The requested effect is not visible in this tenant.
    #[error("effect {id} not found for tenant {tenant_id}")]
    NotFound {
        /// Requested effect identity.
        id: String,
        /// Tenant scope that was searched.
        tenant_id: String,
    },
    /// DOMAIN.md §7.2 does not allow the transition; nothing was written.
    #[error("illegal effect transition: {from} -> {to}")]
    IllegalTransition {
        /// State the row held.
        from: String,
        /// Requested state.
        to: String,
    },
    /// A settled record cannot be settled again (DOMAIN.md §7.2).
    #[error("effect {effect_id} is already settled as {status}")]
    AlreadySettled {
        /// Effect identity.
        effect_id: String,
        /// Terminal state the record holds.
        status: String,
    },
    /// A second in-flight effect with the same idempotency key was refused.
    #[error(
        "effect {effect_class} with idempotency key {idempotency_key} is already in flight \
         as {existing_effect_id}"
    )]
    DuplicateInFlight {
        /// Effect class of the racing action.
        effect_class: String,
        /// Idempotency key both actions derived.
        idempotency_key: String,
        /// The record that already holds the key.
        existing_effect_id: String,
    },
    /// Unknown or in-flight outcomes may not be retried (DOMAIN.md §7.2, D-014).
    #[error("effect {effect_id} in status {status} must be reconciled, not retried")]
    RetryUnsafe {
        /// Effect identity.
        effect_id: String,
        /// State that forbids a retry.
        status: String,
    },
    /// A settled failure is not classified as retryable.
    #[error("effect {effect_id} failed permanently; a retry is not allowed")]
    RetryNotRetryable {
        /// Effect identity.
        effect_id: String,
    },
    /// A `DENIED`/`EXPIRED` record needs a fresh authorization before a retry.
    #[error("effect {effect_id} in status {status} needs a fresh authorization to retry")]
    RetryNotAuthorized {
        /// Effect identity.
        effect_id: String,
        /// State that requires re-authorization.
        status: String,
    },
    /// The record cannot be retried.
    #[error("effect {effect_id} in status {status} cannot be retried")]
    RetryNotAllowed {
        /// Effect identity.
        effect_id: String,
        /// State that forbids a retry.
        status: String,
    },
    /// The record is not awaiting reconciliation.
    #[error("effect {effect_id} is not awaiting reconciliation (status {status})")]
    ReconcileNotRequired {
        /// Effect identity.
        effect_id: String,
        /// Current state.
        status: String,
    },
    /// The evidence does not match the class's reconciliation strategy (fail closed).
    #[error("reconciliation strategy {strategy} cannot accept {evidence} evidence")]
    ReconciliationStrategyMismatch {
        /// Strategy the class declared.
        strategy: String,
        /// Evidence the caller supplied.
        evidence: String,
    },
    /// The class is not registered in the taxonomy.
    #[error("effect class {0} is not registered in the effect taxonomy")]
    UnknownEffectClass(String),
    /// The declared tier disagrees with the registered tier for the class.
    #[error("effect class {effect_class} declares tier {declared}, taxonomy requires {expected}")]
    TierMismatch {
        /// Effect class.
        effect_class: String,
        /// Tier the caller declared.
        declared: u8,
        /// Tier the taxonomy registers.
        expected: u8,
    },
    /// An effect that requires approval was offered as policy-authorized.
    #[error(
        "effect class {0} requires an approval receipt; propose and consume the receipt before reserving"
    )]
    ApprovalRequired(String),
    /// A reservation or dispatch carried a stale generation.
    #[error("fenced: effect generation {received} is behind current generation {current}")]
    FencedStaleGeneration {
        /// Generation the caller carried.
        received: u64,
        /// Generation the record holds.
        current: u64,
    },
    /// The presented dispatch token does not match the reservation.
    #[error("effect {effect_id} dispatch token does not match the reservation")]
    DispatchTokenMismatch {
        /// Effect identity.
        effect_id: String,
    },
    /// The record was reserved without the durability needed to dispatch it.
    #[error("effect {effect_id} has no dispatch token")]
    MissingDispatchToken {
        /// Effect identity.
        effect_id: String,
    },
    /// The run's durable protocol state could not be updated.
    #[error("effect protocol state: {0}")]
    ProtocolState(String),
    /// A mutation was rejected inside the transaction with the reason recorded above.
    #[error("effect mutation rejected: {0}")]
    MutationRejected(String),
}

impl EffectError {
    /// The DOMAIN.md §15 error code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "NOT_FOUND",
            Self::IllegalTransition { .. }
            | Self::RetryNotRetryable { .. }
            | Self::RetryNotAllowed { .. }
            | Self::TierMismatch { .. }
            | Self::UnknownEffectClass(_)
            | Self::UnknownReconciliationStrategy(_)
            | Self::Taxonomy(_) => "RUNTIME_ILLEGAL_TRANSITION",
            Self::AlreadySettled { .. }
            | Self::ReconcileNotRequired { .. }
            | Self::ReconciliationStrategyMismatch { .. }
            | Self::DispatchTokenMismatch { .. } => "CONFLICT_STATE",
            Self::DuplicateInFlight { .. } => "CONFLICT_IDEMPOTENCY_MISMATCH",
            Self::RetryUnsafe { .. } => "EFFECT_UNKNOWN_PENDING_RECONCILIATION",
            Self::RetryNotAuthorized { .. } | Self::ApprovalRequired(_) => "APPROVAL_REQUIRED",
            Self::FencedStaleGeneration { .. } => "FENCED_STALE_GENERATION",
            Self::UnknownStatus(_)
            | Self::UnknownTargetKind(_)
            | Self::MissingDispatchToken { .. } => "VALIDATION_SCHEMA",
            Self::Database(_)
            | Self::Event(_)
            | Self::Schema(_)
            | Self::Core(_)
            | Self::Capability(_)
            | Self::Json(_)
            | Self::ProtocolState(_)
            | Self::MutationRejected(_) => "INTERNAL",
        }
    }
}
