//! The typed failures of a hardened browser action (ADP-001, DOMAIN.md §15).
//!
//! Every refusal the driver makes before it touches a page, and every outcome it cannot establish
//! afterwards, is one of these. Each code carries two flags the model-facing result repeats, because a
//! caller that guesses them wrong is the failure mode this module exists to remove:
//!
//! * `retryable` — whether *the same request* may be sent again once the stated condition clears. It is
//!   never true for an unknown outcome: a click that may have happened is not a click to repeat.
//! * `requires_new_observation` — whether the model must read the page again before acting on it. A
//!   stale revision, a stale dialog and an in-doubt action all mean the page is not what the model last
//!   saw.

use serde_json::{json, Value};

/// A browser failure code (the `BROWSER_*` rows of DOMAIN.md §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultCode {
    /// The snapshot, capture or document revision the action named is no longer the page.
    StalePage,
    /// The dialog the action named is not the one pending.
    StaleDialog,
    /// A dialog is pending; only `browser.handle_dialog` may run.
    DialogPending,
    /// The same action key was replayed with different arguments.
    ActionReplayConflict,
    /// The outcome of a dispatched action could not be established.
    ActionInDoubt,
    /// A secret was about to be inserted on an origin outside its allow-list.
    SecretOriginMismatch,
    /// A secret placeholder could not be resolved.
    SecretUnavailable,
    /// Plain text was typed into a password, one-time-code or card field.
    SensitiveInputDenied,
    /// The destination is not an `http`/`https`/`about:blank` URL without credentials.
    NavigationDenied,
    /// A certificate error is waiting for a decision only a user can make.
    CertificateDecisionRequired,
    /// The action needs a genuine user gesture and agent input is not one.
    GestureRequired,
    /// A user holds the session; agent input is fenced.
    InputFenced,
    /// The classified consequence is above the tier the runtime approved.
    EffectTierExceeded,
    /// The request is not one the tool accepts (unknown field, missing target, bad shape).
    InvalidRequest,
    /// The tab, snapshot reference or element the request named does not exist.
    TargetNotFound,
    /// The page or the browser failed in a way that is known and carries no outcome doubt.
    PageFailure,
}

impl FaultCode {
    /// Every code, for the contract test that DOMAIN.md names each of them.
    pub const ALL: [Self; 16] = [
        Self::StalePage,
        Self::StaleDialog,
        Self::DialogPending,
        Self::ActionReplayConflict,
        Self::ActionInDoubt,
        Self::SecretOriginMismatch,
        Self::SecretUnavailable,
        Self::SensitiveInputDenied,
        Self::NavigationDenied,
        Self::CertificateDecisionRequired,
        Self::GestureRequired,
        Self::InputFenced,
        Self::EffectTierExceeded,
        Self::InvalidRequest,
        Self::TargetNotFound,
        Self::PageFailure,
    ];

    /// Read a canonical code back, for a result a journal stored.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == code)
    }

    /// The canonical code, as DOMAIN.md §15 spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StalePage => "BROWSER_STALE_PAGE",
            Self::StaleDialog => "BROWSER_STALE_DIALOG",
            Self::DialogPending => "BROWSER_DIALOG_PENDING",
            Self::ActionReplayConflict => "BROWSER_ACTION_REPLAY_CONFLICT",
            Self::ActionInDoubt => "BROWSER_ACTION_IN_DOUBT",
            Self::SecretOriginMismatch => "BROWSER_SECRET_ORIGIN_MISMATCH",
            Self::SecretUnavailable => "BROWSER_SECRET_UNAVAILABLE",
            Self::SensitiveInputDenied => "BROWSER_SENSITIVE_INPUT_DENIED",
            Self::NavigationDenied => "BROWSER_NAVIGATION_DENIED",
            Self::CertificateDecisionRequired => "BROWSER_CERTIFICATE_DECISION_REQUIRED",
            Self::GestureRequired => "BROWSER_GESTURE_REQUIRED",
            Self::InputFenced => "BROWSER_INPUT_FENCED",
            Self::EffectTierExceeded => "BROWSER_EFFECT_TIER_EXCEEDED",
            Self::InvalidRequest => "BROWSER_INVALID_REQUEST",
            Self::TargetNotFound => "BROWSER_TARGET_NOT_FOUND",
            Self::PageFailure => "BROWSER_PAGE_FAILURE",
        }
    }

    /// Whether the same request may be sent again once the stated condition clears.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::StalePage | Self::StaleDialog | Self::DialogPending | Self::TargetNotFound
        )
    }

    /// Whether the model must observe the page again before acting on it.
    #[must_use]
    pub const fn requires_new_observation(self) -> bool {
        matches!(
            self,
            Self::StalePage
                | Self::StaleDialog
                | Self::ActionInDoubt
                | Self::InputFenced
                | Self::TargetNotFound
        )
    }
}

/// A failed or doubtful browser action, ready to be returned to the runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserFault {
    /// The code.
    pub code: FaultCode,
    /// What happened, in words safe to show a model: it never carries a secret value.
    pub message: String,
    /// Typed details (expected and actual revision, the dialog that is pending, …).
    pub details: Value,
}

impl BrowserFault {
    /// A fault with no details.
    #[must_use]
    pub fn new(code: FaultCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Value::Null,
        }
    }

    /// Attach typed details.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// The wire form of the fault: the typed error DOMAIN.md §15 describes, plus the two flags.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut error = json!({
            "code": self.code.as_str(),
            "message": self.message,
            "retryable": self.code.retryable(),
            "requires_new_observation": self.code.requires_new_observation(),
        });
        if !self.details.is_null() {
            error["details"] = self.details.clone();
        }
        error
    }
}

impl std::fmt::Display for BrowserFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for BrowserFault {}

impl From<super::BrowserError> for BrowserFault {
    fn from(error: super::BrowserError) -> Self {
        let code = match &error {
            super::BrowserError::InputFenced { .. }
            | super::BrowserError::SessionNotUsable { .. } => FaultCode::InputFenced,
            super::BrowserError::NoSuchElement { .. } => FaultCode::TargetNotFound,
            super::BrowserError::Cdp(_) | super::BrowserError::Page(_) => FaultCode::PageFailure,
        };
        Self::new(code, error.to_string())
    }
}

impl From<super::CdpError> for BrowserFault {
    fn from(error: super::CdpError) -> Self {
        Self::new(FaultCode::PageFailure, error.to_string())
    }
}
