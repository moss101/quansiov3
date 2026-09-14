//! Typed Rust port to the narrow macOS/Windows native computer broker.

use serde_json::Value;
use sqlx::PgConnection;

use super::store::{ComputerControlError, ComputerControlStore};
use super::{AppIdentity, ComputerTier, ComputerUsePolicy, Refusal};

/// One validated operation understood by both native platform brokers.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeComputerAction {
    /// Read a bounded accessibility tree and optionally a bounded screenshot.
    Read {
        /// Exact foreground app identity from the ToolCall resource.
        application: String,
        /// Maximum returned AX nodes.
        max_nodes: u16,
        /// Maximum AX tree depth.
        max_depth: u8,
        /// Whether to include a screenshot reference.
        include_screenshot: bool,
    },
    /// Click absolute screen coordinates.
    Click {
        /// Exact foreground app identity.
        application: String,
        /// Horizontal screen coordinate.
        x: i32,
        /// Vertical screen coordinate.
        y: i32,
        /// `left`, `right`, or `middle`.
        button: String,
    },
    /// Type literal text.
    Type {
        /// Exact foreground app identity.
        application: String,
        /// Literal text; evidence redacts it.
        text: String,
    },
    /// Read or replace the shared clipboard.
    Clipboard {
        /// Exact foreground app identity.
        application: String,
        /// `read` or `write`.
        operation: String,
        /// Text for a write; empty for a read.
        text: String,
    },
    /// Send an allowlisted system-key chord.
    SystemKey {
        /// Exact foreground app identity.
        application: String,
        /// Canonical chord tokens.
        keys: Vec<String>,
    },
}

impl NativeComputerAction {
    /// Exact app resource the runtime authorized.
    #[must_use]
    pub fn application(&self) -> &str {
        match self {
            Self::Read { application, .. }
            | Self::Click { application, .. }
            | Self::Type { application, .. }
            | Self::Clipboard { application, .. }
            | Self::SystemKey { application, .. } => application,
        }
    }

    /// GrantComputerTier rung required by this operation.
    #[must_use]
    pub const fn computer_tier(&self) -> ComputerTier {
        match self {
            Self::Read { .. } => ComputerTier::Read,
            Self::Click { .. } => ComputerTier::Click,
            Self::Type { .. } => ComputerTier::Type,
            Self::Clipboard { .. } => ComputerTier::Clipboard,
            Self::SystemKey { .. } => ComputerTier::SystemKey,
        }
    }

    /// Whether dispatch may already have changed external machine state when the bridge disconnects.
    #[must_use]
    pub const fn is_consequential(&self) -> bool {
        !matches!(self, Self::Read { .. })
    }
}

/// Narrow platform implementation called only after Rust authorization and fencing.
///
/// Every `execute` implementation must re-resolve the foreground app immediately before input and
/// refuse when it differs from [`NativeComputerAction::application`]. This closes the identity-change
/// window between the Rust observation and the privileged platform call.
pub trait NativeComputerBridge: Send + Sync {
    /// Resolve the stable identity of the current foreground application.
    ///
    /// # Errors
    /// Returns a typed platform refusal when identity cannot be resolved exactly.
    fn foreground_app(&self) -> Result<AppIdentity, NativeBridgeError>;

    /// Execute one bounded, typed operation after rechecking exact foreground identity.
    ///
    /// # Errors
    /// Returns a platform refusal. For consequential input the caller records an unknown outcome and
    /// reconciles before any retry.
    fn execute(&self, action: &NativeComputerAction) -> Result<Value, NativeBridgeError>;
}

/// Native-platform refusal with no credential or input payload in its rendering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("native computer bridge refused: {code}")]
pub struct NativeBridgeError {
    /// Stable diagnostic code safe for logs and model context.
    pub code: String,
}

impl NativeBridgeError {
    /// Construct a redaction-safe refusal.
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

/// Failure from the complete Rust-owned native dispatch boundary.
#[derive(Debug, thiserror::Error)]
pub enum ComputerDispatchError {
    /// App identity or GrantComputerTier refused the proposal before dispatch.
    #[error(transparent)]
    Authorization(#[from] Refusal),
    /// Durable holder/generation/ToolCall fencing refused the proposal.
    #[error(transparent)]
    Control(#[from] ComputerControlError),
    /// The bridge returned no stable foreground identity.
    #[error("foreground application identity is empty; native input fails closed")]
    UnidentifiedApp,
    /// The exact app resource changed before dispatch.
    #[error(
        "foreground application changed from {expected} to {actual}; native input fails closed"
    )]
    ForegroundChanged {
        /// App resource authorized by the runtime.
        expected: String,
        /// App identity reported at the boundary.
        actual: String,
    },
    /// A read-only call failed and its slot was safely released.
    #[error(transparent)]
    ReadFailed(NativeBridgeError),
    /// Consequential input may have landed; the ToolCall remains active for effect reconciliation.
    #[error("native input outcome is unknown; reconcile ToolCall {tool_call_id} before retry")]
    OutcomeUnknown {
        /// Exact ToolCall left active in durable control state.
        tool_call_id: String,
        /// Redaction-safe bridge code.
        code: String,
    },
}

/// Rust-owned native computer dispatcher.
pub struct ComputerDispatcher<B> {
    bridge: B,
    policy: ComputerUsePolicy,
}

impl<B: NativeComputerBridge> ComputerDispatcher<B> {
    /// Bind a platform bridge to the already-narrowed computer policy.
    #[must_use]
    pub const fn new(bridge: B, policy: ComputerUsePolicy) -> Self {
        Self { bridge, policy }
    }

    /// Authorize, durably fence, invoke, and settle one native operation.
    ///
    /// # Errors
    /// Refuses before dispatch for identity/tier/control failures. A consequential bridge failure keeps
    /// the exact ToolCall active and returns [`ComputerDispatchError::OutcomeUnknown`].
    pub async fn execute(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        tool_call_id: &str,
        generation: i64,
        action: &NativeComputerAction,
    ) -> Result<Value, ComputerDispatchError> {
        let foreground = self
            .bridge
            .foreground_app()
            .map_err(ComputerDispatchError::ReadFailed)?;
        if foreground.bundle_id.is_empty() {
            return Err(ComputerDispatchError::UnidentifiedApp);
        }
        if foreground.bundle_id != action.application() {
            return Err(ComputerDispatchError::ForegroundChanged {
                expected: action.application().to_string(),
                actual: foreground.bundle_id,
            });
        }
        self.policy
            .authorize(action.computer_tier(), Some(&foreground))?;
        ComputerControlStore::begin_action(conn, tenant_id, target_id, tool_call_id, generation)
            .await?;
        match self.bridge.execute(action) {
            Ok(output) => {
                ComputerControlStore::finish_action(
                    conn,
                    tenant_id,
                    target_id,
                    tool_call_id,
                    generation,
                )
                .await?;
                Ok(output)
            }
            Err(error) if action.is_consequential() => Err(ComputerDispatchError::OutcomeUnknown {
                tool_call_id: tool_call_id.to_string(),
                code: error.code,
            }),
            Err(error) => {
                ComputerControlStore::finish_action(
                    conn,
                    tenant_id,
                    target_id,
                    tool_call_id,
                    generation,
                )
                .await?;
                Err(ComputerDispatchError::ReadFailed(error))
            }
        }
    }
}
