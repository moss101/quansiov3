//! Typed Rust port to the narrow macOS/Windows native computer broker.

use serde_json::Value;
use sqlx::PgConnection;

use super::config::ComputerUseConfig;
use super::permission::{self, AttentionRequest, ForegroundControl, PermissionStatus};
use super::receipt::{
    ActionReceipt, BridgeReport, ComputerActionResult, ExecutionMethod, ReceiptMethod,
};
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

    /// The method the receipt of this action names when a bridge reports none.
    #[must_use]
    pub const fn default_method(&self) -> ReceiptMethod {
        match self {
            Self::Read { .. } => ReceiptMethod::AccessibilityTree,
            Self::Click { .. } | Self::Type { .. } | Self::SystemKey { .. } => {
                ReceiptMethod::HidEvent
            }
            Self::Clipboard { .. } => ReceiptMethod::Pasteboard,
        }
    }

    /// Bytes this action types or writes, which a sequence's limit counts.
    #[must_use]
    pub fn typed_bytes(&self) -> usize {
        match self {
            Self::Type { text, .. } => text.len(),
            Self::Clipboard {
                operation, text, ..
            } if operation == "write" => text.len(),
            _ => 0,
        }
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

    /// What the system says about the four OS permissions, right now.
    ///
    /// # Errors
    /// A status that cannot be read is a refusal, never a guess that everything is granted.
    fn permission_status(&self) -> Result<PermissionStatus, NativeBridgeError>;

    /// Whether the person's own input has taken the foreground back from the agent.
    ///
    /// # Errors
    /// An unreadable answer is a refusal to drive the foreground.
    fn foreground_control(&self) -> Result<ForegroundControl, NativeBridgeError>;

    /// Execute one action and report what it did (ADP-002, DOMAIN.md §8.6.1 A).
    ///
    /// The default wraps [`Self::execute`] in the weakest honest receipt: input was handed to the system
    /// and its effect was not observed (`effect_verified = None`), or, for a read, that data came back.
    /// A bridge that can verify or measure overrides this; one that cannot gets no credit for it.
    ///
    /// # Errors
    /// As [`Self::execute`].
    fn perform(&self, action: &NativeComputerAction) -> Result<BridgeReport, NativeBridgeError> {
        let output = self.execute(action)?;
        let method = action.default_method();
        let receipt = if action.is_consequential() {
            ActionReceipt::unverified(method, ExecutionMethod::Foreground)
        } else {
            ActionReceipt::observed(method, ExecutionMethod::Foreground)
        };
        Ok(BridgeReport { output, receipt })
    }
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
    /// An OS permission or the foreground-control grant is missing: the run parks on an attention item
    /// (DOMAIN.md §8.6.1 E). Nothing reached the bridge and no dispatch slot was taken.
    #[error("{}: the run waits for the person", .0.code())]
    PermissionRequired(AttentionRequest),
    /// The bridge could not say what the permissions are, so nothing was sent.
    #[error("permission status is unavailable: {0}")]
    PermissionStatusUnavailable(NativeBridgeError),
}

/// Run a platform call that may take a while (a screen capture, an accessibility walk, input that waits
/// for the system) without starving the async runtime.
///
/// On a multi-threaded runtime the worker is handed over before it blocks, so tasks queued behind it
/// (including the connection tasks of the database this call is fenced by) keep running. A
/// current-thread runtime has nothing to hand over to and runs the call in place.
fn blocking<T>(call: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(call)
        }
        _ => call(),
    }
}

/// Rust-owned native computer dispatcher.
pub struct ComputerDispatcher<B> {
    bridge: B,
    policy: ComputerUsePolicy,
    config: ComputerUseConfig,
}

impl<B: NativeComputerBridge> ComputerDispatcher<B> {
    /// Bind a platform bridge to the already-narrowed computer policy, under the committed
    /// `config/computer-use.yaml`.
    #[must_use]
    pub fn new(bridge: B, policy: ComputerUsePolicy) -> Self {
        Self::with_config(bridge, policy, ComputerUseConfig::builtin().clone())
    }

    /// As [`Self::new`] under an explicit configuration.
    #[must_use]
    pub const fn with_config(
        bridge: B,
        policy: ComputerUsePolicy,
        config: ComputerUseConfig,
    ) -> Self {
        Self {
            bridge,
            policy,
            config,
        }
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &ComputerUseConfig {
        &self.config
    }

    /// The bridge, for callers that report its other answers (the permission status the desktop shows).
    #[must_use]
    pub const fn bridge(&self) -> &B {
        &self.bridge
    }

    /// Authorize, check permissions, durably fence, invoke, and settle one native operation.
    ///
    /// The order is deliberate. Identity and tier come first (the app is what the decision is about),
    /// then the permissions the action needs: a missing one is the person's to fix, so the action is
    /// refused with an [`AttentionRequest`] before the control row is touched and before the bridge is
    /// asked to do anything. Only then does the durable slot open.
    ///
    /// The bridge's receipt is re-derived ([`ActionReceipt::normalized`]) before it is returned; a
    /// receipt that contradicts itself is a bridge fault, handled like a failed call.
    ///
    /// # Errors
    /// Refuses before dispatch for identity/tier/permission/control failures. A consequential bridge
    /// failure keeps the exact ToolCall active and returns [`ComputerDispatchError::OutcomeUnknown`].
    pub async fn execute(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        tool_call_id: &str,
        generation: i64,
        action: &NativeComputerAction,
    ) -> Result<ComputerActionResult, ComputerDispatchError> {
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
        let status = self
            .bridge
            .permission_status()
            .map_err(ComputerDispatchError::PermissionStatusUnavailable)?;
        let control = self
            .bridge
            .foreground_control()
            .map_err(ComputerDispatchError::PermissionStatusUnavailable)?;
        permission::gate(action, &status, control, &self.config.panes)
            .map_err(ComputerDispatchError::PermissionRequired)?;
        ComputerControlStore::begin_action(conn, tenant_id, target_id, tool_call_id, generation)
            .await?;
        let settled = blocking(|| self.bridge.perform(action))
            .map_err(|error| error.code)
            .and_then(|report| {
                report
                    .receipt
                    .normalized()
                    .map(|receipt| ComputerActionResult {
                        output: report.output,
                        receipt,
                    })
                    .map_err(|_| "receipt_incoherent".to_string())
            });
        match settled {
            Ok(result) => {
                ComputerControlStore::finish_action(
                    conn,
                    tenant_id,
                    target_id,
                    tool_call_id,
                    generation,
                )
                .await?;
                Ok(result)
            }
            Err(code) if action.is_consequential() => Err(ComputerDispatchError::OutcomeUnknown {
                tool_call_id: tool_call_id.to_string(),
                code,
            }),
            Err(code) => {
                ComputerControlStore::finish_action(
                    conn,
                    tenant_id,
                    target_id,
                    tool_call_id,
                    generation,
                )
                .await?;
                Err(ComputerDispatchError::ReadFailed(NativeBridgeError::new(
                    code,
                )))
            }
        }
    }
}
