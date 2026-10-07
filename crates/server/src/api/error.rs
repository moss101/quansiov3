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
use serde::{Deserialize, Serialize};

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
    // Browser (ADP-001)
    /// The revision, snapshot, capture, fingerprint or hit test no longer matches the page.
    BrowserStalePage,
    /// The dialog id or revision is not the pending dialog.
    BrowserStaleDialog,
    /// A dialog is pending; it must be answered first.
    BrowserDialogPending,
    /// An action id was reused with different arguments.
    BrowserActionReplayConflict,
    /// The outcome of a dispatched action could not be established.
    BrowserActionInDoubt,
    /// A secret was about to be typed on an origin outside its allow-list.
    BrowserSecretOriginMismatch,
    /// A secret placeholder could not be resolved.
    BrowserSecretUnavailable,
    /// Plain text or a printable key aimed at a password, one-time-code or card field.
    BrowserSensitiveInputDenied,
    /// The destination is not an http, https or about:blank URL without credentials.
    BrowserNavigationDenied,
    /// A certificate error waits for a decision only a user can make.
    BrowserCertificateDecisionRequired,
    /// The action needs a genuine user gesture.
    BrowserGestureRequired,
    /// A user holds the session.
    BrowserInputFenced,
    /// The classified consequence is above the approved tier.
    BrowserEffectTierExceeded,
    /// The browser Tool request is malformed.
    BrowserInvalidRequest,
    /// The tab, ref or selector does not exist.
    BrowserTargetNotFound,
    /// The page or browser failed with a known outcome.
    BrowserPageFailure,
    // Computer (ADP-002)
    /// A person holds, or has asked to hold, the native computer.
    ComputerTakeoverActive,
    /// An OS permission the sequence had was withdrawn mid-sequence.
    ComputerPermissionRevoked,
    /// A computer action sequence passed its action, wall-time or typed-byte limit.
    ComputerLimitExceeded,
    /// The person stopped the run.
    ComputerStoppedByUser,
    /// An OS permission is missing at the first action; the run parks.
    ComputerPermissionRequired,
    /// The person is using the keyboard or pointer; the run parks and asks them to pause.
    ComputerForegroundControlLapsed,
    /// The native helper's signature does not match the configured pin.
    ComputerHelperIdentityRefused,
    // Recording (ADP-003)
    /// The call's user, session family or epoch is not the recording's.
    RecordingOwnerChanged,
    /// Compile or submit on a recording the person has not finished.
    RecordingNotFinished,
    /// An archive text field still matches a redaction rule.
    RecordingUnredacted,
    /// The plane's proposal broke a skill-recording rule.
    RecordingProposalRejected,
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
    pub const ALL: [ApiErrorCode; 65] = [
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
        ApiErrorCode::BrowserStalePage,
        ApiErrorCode::BrowserStaleDialog,
        ApiErrorCode::BrowserDialogPending,
        ApiErrorCode::BrowserActionReplayConflict,
        ApiErrorCode::BrowserActionInDoubt,
        ApiErrorCode::BrowserSecretOriginMismatch,
        ApiErrorCode::BrowserSecretUnavailable,
        ApiErrorCode::BrowserSensitiveInputDenied,
        ApiErrorCode::BrowserNavigationDenied,
        ApiErrorCode::BrowserCertificateDecisionRequired,
        ApiErrorCode::BrowserGestureRequired,
        ApiErrorCode::BrowserInputFenced,
        ApiErrorCode::BrowserEffectTierExceeded,
        ApiErrorCode::BrowserInvalidRequest,
        ApiErrorCode::BrowserTargetNotFound,
        ApiErrorCode::BrowserPageFailure,
        ApiErrorCode::ComputerTakeoverActive,
        ApiErrorCode::ComputerPermissionRevoked,
        ApiErrorCode::ComputerLimitExceeded,
        ApiErrorCode::ComputerStoppedByUser,
        ApiErrorCode::ComputerPermissionRequired,
        ApiErrorCode::ComputerForegroundControlLapsed,
        ApiErrorCode::ComputerHelperIdentityRefused,
        ApiErrorCode::RecordingOwnerChanged,
        ApiErrorCode::RecordingNotFinished,
        ApiErrorCode::RecordingUnredacted,
        ApiErrorCode::RecordingProposalRejected,
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
            Self::BrowserStalePage => "BROWSER_STALE_PAGE",
            Self::BrowserStaleDialog => "BROWSER_STALE_DIALOG",
            Self::BrowserDialogPending => "BROWSER_DIALOG_PENDING",
            Self::BrowserActionReplayConflict => "BROWSER_ACTION_REPLAY_CONFLICT",
            Self::BrowserActionInDoubt => "BROWSER_ACTION_IN_DOUBT",
            Self::BrowserSecretOriginMismatch => "BROWSER_SECRET_ORIGIN_MISMATCH",
            Self::BrowserSecretUnavailable => "BROWSER_SECRET_UNAVAILABLE",
            Self::BrowserSensitiveInputDenied => "BROWSER_SENSITIVE_INPUT_DENIED",
            Self::BrowserNavigationDenied => "BROWSER_NAVIGATION_DENIED",
            Self::BrowserCertificateDecisionRequired => "BROWSER_CERTIFICATE_DECISION_REQUIRED",
            Self::BrowserGestureRequired => "BROWSER_GESTURE_REQUIRED",
            Self::BrowserInputFenced => "BROWSER_INPUT_FENCED",
            Self::BrowserEffectTierExceeded => "BROWSER_EFFECT_TIER_EXCEEDED",
            Self::BrowserInvalidRequest => "BROWSER_INVALID_REQUEST",
            Self::BrowserTargetNotFound => "BROWSER_TARGET_NOT_FOUND",
            Self::BrowserPageFailure => "BROWSER_PAGE_FAILURE",
            Self::ComputerTakeoverActive => "COMPUTER_TAKEOVER_ACTIVE",
            Self::ComputerPermissionRevoked => "COMPUTER_PERMISSION_REVOKED",
            Self::ComputerLimitExceeded => "COMPUTER_LIMIT_EXCEEDED",
            Self::ComputerStoppedByUser => "COMPUTER_STOPPED_BY_USER",
            Self::ComputerPermissionRequired => "COMPUTER_PERMISSION_REQUIRED",
            Self::ComputerForegroundControlLapsed => "COMPUTER_FOREGROUND_CONTROL_LAPSED",
            Self::ComputerHelperIdentityRefused => "COMPUTER_HELPER_IDENTITY_REFUSED",
            Self::RecordingOwnerChanged => "RECORDING_OWNER_CHANGED",
            Self::RecordingNotFinished => "RECORDING_NOT_FINISHED",
            Self::RecordingUnredacted => "RECORDING_UNREDACTED",
            Self::RecordingProposalRejected => "RECORDING_PROPOSAL_REJECTED",
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
            Self::BrowserStalePage
            | Self::BrowserStaleDialog
            | Self::BrowserDialogPending
            | Self::BrowserActionReplayConflict
            | Self::BrowserActionInDoubt
            | Self::BrowserSecretOriginMismatch
            | Self::BrowserSecretUnavailable
            | Self::BrowserSensitiveInputDenied
            | Self::BrowserNavigationDenied
            | Self::BrowserCertificateDecisionRequired
            | Self::BrowserGestureRequired
            | Self::BrowserInputFenced
            | Self::BrowserEffectTierExceeded
            | Self::BrowserInvalidRequest
            | Self::BrowserTargetNotFound
            | Self::BrowserPageFailure => "Browser",
            Self::ComputerTakeoverActive
            | Self::ComputerPermissionRevoked
            | Self::ComputerLimitExceeded
            | Self::ComputerStoppedByUser
            | Self::ComputerPermissionRequired
            | Self::ComputerForegroundControlLapsed
            | Self::ComputerHelperIdentityRefused => "Computer",
            Self::RecordingOwnerChanged
            | Self::RecordingNotFinished
            | Self::RecordingUnredacted
            | Self::RecordingProposalRejected => "Recording",
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
            // Browser (409/422): a page that moved, a dialog, a replay or a fenced session is a
            // conflict with the state the call assumed; a denial or a malformed request is
            // unprocessable as asked.
            Self::BrowserStalePage
            | Self::BrowserStaleDialog
            | Self::BrowserDialogPending
            | Self::BrowserActionReplayConflict
            | Self::BrowserActionInDoubt
            | Self::BrowserCertificateDecisionRequired
            | Self::BrowserInputFenced => StatusCode::CONFLICT,
            Self::BrowserSecretOriginMismatch
            | Self::BrowserSecretUnavailable
            | Self::BrowserSensitiveInputDenied
            | Self::BrowserNavigationDenied
            | Self::BrowserGestureRequired
            | Self::BrowserEffectTierExceeded
            | Self::BrowserInvalidRequest
            | Self::BrowserTargetNotFound
            | Self::BrowserPageFailure => StatusCode::UNPROCESSABLE_ENTITY,
            // Computer (409/422): a person holding the machine, a withdrawn permission or a parked run
            // conflicts with the state the call assumed; a spent limit or a refused helper is
            // unprocessable as asked.
            Self::ComputerTakeoverActive
            | Self::ComputerPermissionRevoked
            | Self::ComputerStoppedByUser
            | Self::ComputerPermissionRequired
            | Self::ComputerForegroundControlLapsed => StatusCode::CONFLICT,
            Self::ComputerLimitExceeded | Self::ComputerHelperIdentityRefused => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            // Recording (409/422): a changed owner or an unfinished recording conflicts with the
            // state the call assumed; an archive that still holds what redaction removes, or a
            // proposal that breaks a rule, is unprocessable as sent.
            Self::RecordingOwnerChanged | Self::RecordingNotFinished => StatusCode::CONFLICT,
            Self::RecordingUnredacted | Self::RecordingProposalRejected => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
