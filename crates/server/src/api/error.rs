//! The public API's error surface (APP-001, DOMAIN.md §15).
//!
//! §15 fixes both the shape and the vocabulary, and both are enforced here rather than translated per
//! handler:
//!
//! * the shape is `{code, message, correlation_id, retryable, details?}` — every refusal an HTTP client
//!   sees has it, including the ones that never reach a module;
//! * the vocabulary is the §15 table, held as a closed enum so a handler cannot invent a code, and checked
//!   against `schemas/catalog/errors.yaml` — the same table the bindings are generated from — by a test, so
//!   an added or renamed code fails in the suite rather than drifting.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// Every code in DOMAIN.md §15, with the family and HTTP status the table gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorCode {
    // Auth
    /// No credential was presented.
    AuthRequired,
    /// The credential is not one this deployment issued.
    AuthInvalidToken,
    /// The credential was valid and is past its expiry.
    AuthExpired,
    /// The credential does not reach the scope the call needs.
    ScopeForbidden,
    /// The call names a tenant the credential does not belong to.
    TenantMismatch,
    // Validation
    /// The body is not the shape the endpoint accepts.
    ValidationSchema,
    /// The body carries a field the endpoint does not accept.
    ValidationUnknownField,
    /// A value is outside the bounds the endpoint declares.
    ValidationBounds,
    // Conflict
    /// The caller's revision is stale.
    ConflictRevision,
    /// The same idempotency key arrived with different content.
    ConflictIdempotencyMismatch,
    /// The entity is not in a state that allows the operation.
    ConflictState,
    // Authority
    /// The capability projection does not include the action.
    CapabilityDenied,
    /// A security input the decision needs is missing.
    CapabilityInputsUnavailable,
    /// Policy refused the action.
    PolicyDenied,
    /// The action needs an approval that has not been granted.
    ApprovalRequired,
    /// The approval does not match the action.
    ApprovalInvalid,
    /// The approval has expired.
    ApprovalExpired,
    /// The action's parameters changed after the approval was granted.
    ApprovalSuperseded,
    // Runtime
    /// The state machine has no such edge.
    RuntimeIllegalTransition,
    /// The caller's generation is stale.
    FencedStaleGeneration,
    /// The lease is no longer held.
    LeaseLost,
    /// The effect's outcome is unknown and must be reconciled before any retry.
    EffectUnknownPendingReconciliation,
    /// The budget is spent.
    BudgetExhausted,
    /// Effects are frozen.
    EffectsFrozen,
    // Execution
    /// No execution target is available.
    TargetUnavailable,
    /// Provisioning the target failed.
    TargetProvisionFailed,
    /// The tool did not finish in time.
    ToolTimeout,
    /// The tool's output was cut to its bound. A warning rather than a failure.
    ToolOutputTruncated,
    /// Egress was refused.
    EgressDenied,
    // Model
    /// The provider could not be reached.
    ProviderUnavailable,
    /// The provider rate-limited the call.
    ProviderRateLimited,
    /// The provider refused the call.
    ProviderRefusal,
    /// No route serves the request.
    RouteUnavailable,
    /// The data-loss guard refused the payload.
    DlpDenied,
    // Generic
    /// There is no such entity.
    NotFound,
    /// The caller is over its rate limit.
    RateLimited,
    /// A stream could not keep up.
    StreamBackpressure,
    /// Something failed that is not the caller's fault.
    Internal,
}

impl ApiErrorCode {
    /// Every code in the table, so a caller can check one against it without a list of its own.
    pub const ALL: [ApiErrorCode; 38] = [
        ApiErrorCode::AuthRequired,
        ApiErrorCode::AuthInvalidToken,
        ApiErrorCode::AuthExpired,
        ApiErrorCode::ScopeForbidden,
        ApiErrorCode::TenantMismatch,
        ApiErrorCode::ValidationSchema,
        ApiErrorCode::ValidationUnknownField,
        ApiErrorCode::ValidationBounds,
        ApiErrorCode::ConflictRevision,
        ApiErrorCode::ConflictIdempotencyMismatch,
        ApiErrorCode::ConflictState,
        ApiErrorCode::CapabilityDenied,
        ApiErrorCode::CapabilityInputsUnavailable,
        ApiErrorCode::PolicyDenied,
        ApiErrorCode::ApprovalRequired,
        ApiErrorCode::ApprovalInvalid,
        ApiErrorCode::ApprovalExpired,
        ApiErrorCode::ApprovalSuperseded,
        ApiErrorCode::RuntimeIllegalTransition,
        ApiErrorCode::FencedStaleGeneration,
        ApiErrorCode::LeaseLost,
        ApiErrorCode::EffectUnknownPendingReconciliation,
        ApiErrorCode::BudgetExhausted,
        ApiErrorCode::EffectsFrozen,
        ApiErrorCode::TargetUnavailable,
        ApiErrorCode::TargetProvisionFailed,
        ApiErrorCode::ToolTimeout,
        ApiErrorCode::ToolOutputTruncated,
        ApiErrorCode::EgressDenied,
        ApiErrorCode::ProviderUnavailable,
        ApiErrorCode::ProviderRateLimited,
        ApiErrorCode::ProviderRefusal,
        ApiErrorCode::RouteUnavailable,
        ApiErrorCode::DlpDenied,
        ApiErrorCode::NotFound,
        ApiErrorCode::RateLimited,
        ApiErrorCode::StreamBackpressure,
        ApiErrorCode::Internal,
    ];

    /// The canonical code string, exactly as §15 and `errors.yaml` spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthRequired => "AUTH_REQUIRED",
            Self::AuthInvalidToken => "AUTH_INVALID_TOKEN",
            Self::AuthExpired => "AUTH_EXPIRED",
            Self::ScopeForbidden => "SCOPE_FORBIDDEN",
            Self::TenantMismatch => "TENANT_MISMATCH",
            Self::ValidationSchema => "VALIDATION_SCHEMA",
            Self::ValidationUnknownField => "VALIDATION_UNKNOWN_FIELD",
            Self::ValidationBounds => "VALIDATION_BOUNDS",
            Self::ConflictRevision => "CONFLICT_REVISION",
            Self::ConflictIdempotencyMismatch => "CONFLICT_IDEMPOTENCY_MISMATCH",
            Self::ConflictState => "CONFLICT_STATE",
            Self::CapabilityDenied => "CAPABILITY_DENIED",
            Self::CapabilityInputsUnavailable => "CAPABILITY_INPUTS_UNAVAILABLE",
            Self::PolicyDenied => "POLICY_DENIED",
            Self::ApprovalRequired => "APPROVAL_REQUIRED",
            Self::ApprovalInvalid => "APPROVAL_INVALID",
            Self::ApprovalExpired => "APPROVAL_EXPIRED",
            Self::ApprovalSuperseded => "APPROVAL_SUPERSEDED",
            Self::RuntimeIllegalTransition => "RUNTIME_ILLEGAL_TRANSITION",
            Self::FencedStaleGeneration => "FENCED_STALE_GENERATION",
            Self::LeaseLost => "LEASE_LOST",
            Self::EffectUnknownPendingReconciliation => "EFFECT_UNKNOWN_PENDING_RECONCILIATION",
            Self::BudgetExhausted => "BUDGET_EXHAUSTED",
            Self::EffectsFrozen => "EFFECTS_FROZEN",
            Self::TargetUnavailable => "TARGET_UNAVAILABLE",
            Self::TargetProvisionFailed => "TARGET_PROVISION_FAILED",
            Self::ToolTimeout => "TOOL_TIMEOUT",
            Self::ToolOutputTruncated => "TOOL_OUTPUT_TRUNCATED",
            Self::EgressDenied => "EGRESS_DENIED",
            Self::ProviderUnavailable => "PROVIDER_UNAVAILABLE",
            Self::ProviderRateLimited => "PROVIDER_RATE_LIMITED",
            Self::ProviderRefusal => "PROVIDER_REFUSAL",
            Self::RouteUnavailable => "ROUTE_UNAVAILABLE",
            Self::DlpDenied => "DLP_DENIED",
            Self::NotFound => "NOT_FOUND",
            Self::RateLimited => "RATE_LIMITED",
            Self::StreamBackpressure => "STREAM_BACKPRESSURE",
            Self::Internal => "INTERNAL",
        }
    }

    /// The §15 family this code belongs to.
    #[must_use]
    pub const fn family(self) -> &'static str {
        match self {
            Self::AuthRequired
            | Self::AuthInvalidToken
            | Self::AuthExpired
            | Self::ScopeForbidden
            | Self::TenantMismatch => "Auth",
            Self::ValidationSchema | Self::ValidationUnknownField | Self::ValidationBounds => {
                "Validation"
            }
            Self::ConflictRevision | Self::ConflictIdempotencyMismatch | Self::ConflictState => {
                "Conflict"
            }
            Self::CapabilityDenied
            | Self::CapabilityInputsUnavailable
            | Self::PolicyDenied
            | Self::ApprovalRequired
            | Self::ApprovalInvalid
            | Self::ApprovalExpired
            | Self::ApprovalSuperseded => "Authority",
            Self::RuntimeIllegalTransition
            | Self::FencedStaleGeneration
            | Self::LeaseLost
            | Self::EffectUnknownPendingReconciliation
            | Self::BudgetExhausted
            | Self::EffectsFrozen => "Runtime",
            Self::TargetUnavailable
            | Self::TargetProvisionFailed
            | Self::ToolTimeout
            | Self::ToolOutputTruncated
            | Self::EgressDenied => "Execution",
            Self::ProviderUnavailable
            | Self::ProviderRateLimited
            | Self::ProviderRefusal
            | Self::RouteUnavailable
            | Self::DlpDenied => "Model",
            Self::NotFound | Self::RateLimited | Self::StreamBackpressure | Self::Internal => {
                "Generic"
            }
        }
    }

    /// The HTTP status §15 maps this code to. Where the table gives two, the first is used for a request
    /// that carries no usable credential and the second for one that is present but insufficient.
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::AuthRequired => StatusCode::UNAUTHORIZED,
            Self::AuthInvalidToken | Self::AuthExpired => StatusCode::UNAUTHORIZED,
            Self::ScopeForbidden | Self::TenantMismatch => StatusCode::FORBIDDEN,
            Self::ValidationSchema | Self::ValidationUnknownField | Self::ValidationBounds => {
                StatusCode::BAD_REQUEST
            }
            Self::ConflictRevision | Self::ConflictIdempotencyMismatch | Self::ConflictState => {
                StatusCode::CONFLICT
            }
            Self::CapabilityDenied
            | Self::CapabilityInputsUnavailable
            | Self::PolicyDenied
            | Self::ApprovalRequired
            | Self::ApprovalInvalid
            | Self::ApprovalExpired
            | Self::ApprovalSuperseded => StatusCode::FORBIDDEN,
            Self::RuntimeIllegalTransition | Self::FencedStaleGeneration | Self::LeaseLost => {
                StatusCode::CONFLICT
            }
            Self::EffectUnknownPendingReconciliation
            | Self::BudgetExhausted
            | Self::EffectsFrozen => StatusCode::UNPROCESSABLE_ENTITY,
            Self::TargetUnavailable
            | Self::TargetProvisionFailed
            | Self::ProviderUnavailable
            | Self::ProviderRateLimited
            | Self::ProviderRefusal
            | Self::RouteUnavailable
            | Self::DlpDenied => StatusCode::SERVICE_UNAVAILABLE,
            Self::ToolTimeout => StatusCode::GATEWAY_TIMEOUT,
            // A bounded output is a warning the caller can act on, not a refusal.
            Self::ToolOutputTruncated => StatusCode::OK,
            Self::EgressDenied => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::StreamBackpressure => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Whether retrying the same call could succeed without the caller changing anything.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::ProviderUnavailable
                | Self::ProviderRateLimited
                | Self::RouteUnavailable
                | Self::TargetUnavailable
                | Self::RateLimited
                | Self::StreamBackpressure
                | Self::Internal
        )
    }
}

/// The refusal a client sees (DOMAIN.md §15).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApiError {
    /// The canonical code.
    pub code: String,
    /// A human-readable sentence. Never carries a secret or another tenant's data.
    pub message: String,
    /// The id shared by everything caused by one external input, so a report can be traced.
    pub correlation_id: String,
    /// Whether the same call could succeed later.
    pub retryable: bool,
    /// Structured detail, when the code has any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    /// Build a refusal.
    #[must_use]
    pub fn new(
        code: ApiErrorCode,
        message: impl Into<String>,
        correlation_id: impl Into<String>,
    ) -> Self {
        Self {
            code: code.as_str().to_string(),
            message: message.into(),
            correlation_id: correlation_id.into(),
            retryable: code.retryable(),
            details: None,
        }
    }

    /// Attach structured detail.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The code as an enum, for a caller that wants to match rather than compare strings. `None` means the
    /// stored code is not one §15 defines, which cannot happen for an error this module built.
    #[must_use]
    pub fn code(&self) -> Option<ApiErrorCode> {
        ApiErrorCode::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == self.code)
    }

    /// The status this refusal is served with.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.code()
            .map_or(StatusCode::INTERNAL_SERVER_ERROR, |code| code.status())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status(), Json(self)).into_response()
    }
}
