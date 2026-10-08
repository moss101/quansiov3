//! [`ComputerToolHost`]: `computer.*` calls through the sequence runner (ADP-002).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_machine::computer_use::bridge::{
    ComputerDispatchError, ComputerDispatcher, NativeBridgeError, NativeComputerAction,
    NativeComputerBridge,
};
use quansio_machine::computer_use::config::ComputerUseConfig;
use quansio_machine::computer_use::permission::{
    AttentionRequest, ForegroundControl, GuideAction, PermissionStatus,
};
use quansio_machine::computer_use::receipt::{BridgeReport, LedgerOutcome};
use quansio_machine::computer_use::sequence::{
    ComputerSequence, ListFeed, SequenceEnd, SequenceReport, StopHandle, TerminalCondition,
};
use quansio_machine::computer_use::store::{ComputerControlError, ComputerControlStore};
use quansio_machine::computer_use::{AppIdentity, ComputerGrant, ComputerUsePolicy, KnownApps};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::runtime::state_machine::{RunStatus, RuntimeIdentity, RuntimeStore, Suspension};
use crate::runtime::turn_loop::{HostDispatch, HostFailure, HostOutcome, ToolHostPort};

/// The task that owns the native computer rules this host applies.
const OWNER: &str = "ADP-002";

/// The code an unavailable bridge refuses with.
pub const BRIDGE_UNAVAILABLE: &str = "COMPUTER_BRIDGE_UNAVAILABLE";

/// The bridge of a build that has no way to reach a machine's input: it answers nothing and sends nothing.
///
/// This is the fail-closed default, not a simulation. Every call is refused before the control row is touched.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableComputerBridge;

impl NativeComputerBridge for UnavailableComputerBridge {
    fn foreground_app(&self) -> Result<AppIdentity, NativeBridgeError> {
        Err(NativeBridgeError::new(BRIDGE_UNAVAILABLE))
    }

    fn execute(&self, _action: &NativeComputerAction) -> Result<Value, NativeBridgeError> {
        Err(NativeBridgeError::new(BRIDGE_UNAVAILABLE))
    }

    fn permission_status(&self) -> Result<PermissionStatus, NativeBridgeError> {
        Err(NativeBridgeError::new(BRIDGE_UNAVAILABLE))
    }

    fn foreground_control(&self) -> Result<ForegroundControl, NativeBridgeError> {
        Err(NativeBridgeError::new(BRIDGE_UNAVAILABLE))
    }
}

/// A shared bridge behind the dispatcher's generic parameter.
struct SharedBridge(Arc<dyn NativeComputerBridge>);

impl NativeComputerBridge for SharedBridge {
    fn foreground_app(&self) -> Result<AppIdentity, NativeBridgeError> {
        self.0.foreground_app()
    }

    fn execute(&self, action: &NativeComputerAction) -> Result<Value, NativeBridgeError> {
        self.0.execute(action)
    }

    fn permission_status(&self) -> Result<PermissionStatus, NativeBridgeError> {
        self.0.permission_status()
    }

    fn foreground_control(&self) -> Result<ForegroundControl, NativeBridgeError> {
        self.0.foreground_control()
    }

    fn perform(&self, action: &NativeComputerAction) -> Result<BridgeReport, NativeBridgeError> {
        self.0.perform(action)
    }
}

/// The stop flags of the sequences in flight, by run.
///
/// A person's stop (`CancelRun`, `PauseRun`) raises the flag of the sequence the run is walking; the sequence
/// checks it before it even asks for another action. A flag cannot be lowered, so each call registers its own and
/// removes it when it ends.
#[derive(Debug, Clone, Default)]
pub struct StopRegistry {
    flags: Arc<Mutex<HashMap<String, (u64, StopHandle)>>>,
    next: Arc<AtomicU64>,
}

impl StopRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh flag for `run_id`; the token releases exactly this registration.
    fn register(&self, run_id: &str) -> (u64, StopHandle) {
        let token = self.next.fetch_add(1, Ordering::SeqCst);
        let handle = StopHandle::new();
        self.flags
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run_id.to_string(), (token, handle.clone()));
        (token, handle)
    }

    /// Drop the registration `token` made, and no newer one.
    fn release(&self, run_id: &str, token: u64) {
        let mut flags = self.flags.lock().unwrap_or_else(PoisonError::into_inner);
        if flags
            .get(run_id)
            .is_some_and(|(current, _)| *current == token)
        {
            flags.remove(run_id);
        }
    }

    /// Stop the sequence `run_id` is walking, if any. Returns whether one was in flight.
    pub fn stop(&self, run_id: &str) -> bool {
        let flags = self.flags.lock().unwrap_or_else(PoisonError::into_inner);
        flags.get(run_id).is_some_and(|(_, handle)| {
            handle.stop();
            true
        })
    }
}

/// Executes `computer.*` tools.
#[derive(Clone)]
pub struct ComputerToolHost {
    pool: PgPool,
    bridge: Arc<dyn NativeComputerBridge>,
    known: KnownApps,
    grant: ComputerGrant,
    config: ComputerUseConfig,
    stops: StopRegistry,
}

impl std::fmt::Debug for ComputerToolHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputerToolHost").finish_non_exhaustive()
    }
}

fn failure(code: &str, rule: &str, message: impl Into<String>, retryable: bool) -> HostOutcome {
    HostOutcome::Failure {
        retryable,
        error: json!({
            "code": code,
            "rule": rule,
            "message": message.into(),
            "retryable": retryable,
        }),
        evidence_ids: Vec::new(),
    }
}

fn unavailable(detail: impl Into<String>) -> HostFailure {
    HostFailure::Unavailable {
        owner: OWNER,
        detail: detail.into(),
    }
}

/// The action a `computer.*` call asks for, from arguments the registry has already validated.
fn parse_action(tool: &str, args: &Value) -> Result<NativeComputerAction, String> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("{key} is required"))
    };
    let number = |key: &str| {
        args.get(key)
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("{key} is required"))
    };
    let application = text("application")?;
    Ok(match tool {
        "computer.read" => NativeComputerAction::Read {
            application,
            max_nodes: u16::try_from(number("max_nodes")?)
                .map_err(|_| "max_nodes is out of range".to_string())?,
            max_depth: u8::try_from(number("max_depth")?)
                .map_err(|_| "max_depth is out of range".to_string())?,
            include_screenshot: args
                .get("include_screenshot")
                .and_then(Value::as_bool)
                .ok_or("include_screenshot is required")?,
        },
        "computer.click" => NativeComputerAction::Click {
            application,
            x: i32::try_from(number("x")?).map_err(|_| "x is out of range".to_string())?,
            y: i32::try_from(number("y")?).map_err(|_| "y is out of range".to_string())?,
            button: text("button")?,
        },
        "computer.type" => NativeComputerAction::Type {
            application,
            text: text("text")?,
        },
        "computer.clipboard" => NativeComputerAction::Clipboard {
            application,
            operation: text("operation")?,
            text: text("text")?,
        },
        "computer.system_key" => NativeComputerAction::SystemKey {
            application,
            keys: args
                .get("keys")
                .and_then(Value::as_array)
                .ok_or("keys is required")?
                .iter()
                .map(|key| key.as_str().map(str::to_string).ok_or("a key is a string"))
                .collect::<Result<Vec<_>, _>>()?,
        },
        other => return Err(format!("{other} is not a native computer tool")),
    })
}

/// What the Tool error of a terminal condition says, with the message the typed shape promises.
fn terminal_error(condition: &TerminalCondition) -> Value {
    let mut error = condition.to_json();
    error["message"] = json!(match condition {
        TerminalCondition::TakeoverActive =>
            "a person holds the computer, or asked to; nothing was sent",
        TerminalCondition::PermissionRevoked { .. } =>
            "an operating-system permission this call had was withdrawn",
        TerminalCondition::LimitExceeded { .. } => "the call passed a limit of a computer sequence",
        TerminalCondition::StoppedByUser => "the person stopped the run; nothing was sent",
    });
    error
}

/// The Tool error a parked call is recorded with.
fn attention_error(attention: &AttentionRequest) -> Value {
    let lapsed = matches!(attention.guide, GuideAction::PauseInput);
    json!({
        "code": attention.code(),
        "message": "the run waits for the person; nothing was sent",
        "retryable": true,
        "requires_new_observation": lapsed,
        "requires": if lapsed { "input_paused" } else { "permission_granted" },
        "details": { "attention": attention.to_json() },
    })
}

/// Ask for the run to be suspended on `attention`: nothing was sent.
fn suspend(attention: &AttentionRequest) -> HostOutcome {
    HostOutcome::Suspend(Box::new(Suspension {
        reason: attention.code().to_string(),
        attention: attention.to_json(),
        error: attention_error(attention),
    }))
}

/// What the dispatcher is told about a finished sequence.
fn settle(report: &SequenceReport) -> Result<HostOutcome, HostFailure> {
    match &report.end {
        SequenceEnd::Terminal(condition) => Ok(HostOutcome::Failure {
            retryable: condition.retryable(),
            error: terminal_error(condition),
            evidence_ids: Vec::new(),
        }),
        SequenceEnd::Parked(attention) => Ok(suspend(attention)),
        SequenceEnd::Completed | SequenceEnd::Halted { .. } => {
            match report.ledger_outcome() {
                LedgerOutcome::SettledSuccess => {
                    let result = report
                        .settled
                        .first()
                        .ok_or_else(|| unavailable("a completed sequence settled no action"))?
                        .result
                        .tool_result()
                        .map_err(|error| unavailable(error.to_string()))?;
                    Ok(HostOutcome::Success {
                        output: result,
                        evidence_ids: Vec::new(),
                        remote_ref: None,
                    })
                }
                LedgerOutcome::SettledFailed => Ok(failure(
                    "CONFLICT_STATE",
                    "computer.refused",
                    "the bridge refused the action; nothing was sent",
                    false,
                )),
                // Input reached the machine and its effect is not established: reconcile, never retry blind.
                LedgerOutcome::OutcomeUnknown => Ok(HostOutcome::Unknown {
                    detail: json!({
                        "code": "COMPUTER_OUTCOME_UNKNOWN",
                        "requires": "reconciliation",
                        "receipts": report
                            .settled
                            .iter()
                            .map(|settled| settled.result.receipt.to_json())
                            .collect::<Vec<_>>(),
                    })
                    .to_string(),
                }),
            }
        }
    }
}

/// What the dispatcher is told about a call whose sequence failed before it could settle.
fn dispatch_failure(error: ComputerDispatchError) -> Result<HostOutcome, HostFailure> {
    match error {
        ComputerDispatchError::ReadFailed(bridge) | ComputerDispatchError::PermissionStatusUnavailable(bridge)
            if bridge.code == BRIDGE_UNAVAILABLE =>
        {
            Err(unavailable(
                "this build has no native computer bridge: the helper that holds the operating-system permissions is not installed",
            ))
        }
        ComputerDispatchError::PermissionStatusUnavailable(bridge) => Err(unavailable(format!(
            "the bridge could not say what the permissions are ({}); nothing was sent",
            bridge.code
        ))),
        ComputerDispatchError::ReadFailed(bridge) => Ok(failure(
            "ADAPTER_UNAVAILABLE",
            "computer.bridge",
            format!("the bridge failed ({})", bridge.code),
            false,
        )),
        ComputerDispatchError::Authorization(refusal) => Err(HostFailure::Refused {
            detail: refusal.to_string(),
        }),
        ComputerDispatchError::UnidentifiedApp => Ok(failure(
            "CONFLICT_STATE",
            "computer.foreground",
            "the foreground application cannot be identified; nothing was sent",
            true,
        )),
        ComputerDispatchError::ForegroundChanged { expected, actual } => Ok(failure(
            "CONFLICT_STATE",
            "computer.foreground",
            format!("the foreground application changed from {expected} to {actual}; observe again"),
            true,
        )),
        // The bridge failed after consequential input may have landed: the slot stays active and the effect is
        // reconciled, never retried.
        ComputerDispatchError::OutcomeUnknown { code, .. } => Ok(HostOutcome::Unknown {
            detail: json!({
                "code": "COMPUTER_OUTCOME_UNKNOWN",
                "requires": "reconciliation",
                "bridge": code,
            })
            .to_string(),
        }),
        ComputerDispatchError::Control(ComputerControlError::Database(error)) => {
            Err(unavailable(format!("computer control is unavailable: {error}")))
        }
        ComputerDispatchError::Control(other) => Ok(failure(
            "CONFLICT_STATE",
            "computer.control",
            other.to_string(),
            false,
        )),
        // The sequence turns this into a park before it can reach here.
        ComputerDispatchError::PermissionRequired(attention) => Ok(suspend(&attention)),
    }
}

impl ComputerToolHost {
    /// A host over the database whose bridge is [`UnavailableComputerBridge`], recognising no application.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            bridge: Arc::new(UnavailableComputerBridge),
            known: KnownApps::none(),
            grant: ComputerGrant::read_only(),
            config: ComputerUseConfig::builtin().clone(),
            stops: StopRegistry::new(),
        }
    }

    /// Reach a machine through `bridge`, recognising `known` applications up to `grant`.
    #[must_use]
    pub fn with_bridge(
        mut self,
        bridge: Arc<dyn NativeComputerBridge>,
        known: KnownApps,
        grant: ComputerGrant,
    ) -> Self {
        self.bridge = bridge;
        self.known = known;
        self.grant = grant;
        self
    }

    /// Run under another configuration (limits, panes).
    #[must_use]
    pub fn with_config(mut self, config: ComputerUseConfig) -> Self {
        self.config = config;
        self
    }

    /// The stop flags of the sequences in flight.
    #[must_use]
    pub fn stops(&self) -> &StopRegistry {
        &self.stops
    }

    async fn run(&self, dispatch: &HostDispatch) -> Result<HostOutcome, HostFailure> {
        let action = match parse_action(&dispatch.tool, &dispatch.args) {
            Ok(action) => action,
            Err(message) => {
                return Ok(failure(
                    "VALIDATION_SCHEMA",
                    "computer.arguments",
                    message,
                    false,
                ))
            }
        };
        let run_id = CanonicalId::parse_typed(&dispatch.run_id, Prefix::Run).map_err(|error| {
            HostFailure::Refused {
                detail: format!("{} is not a run id: {error}", dispatch.run_id),
            }
        })?;
        let identity = RuntimeIdentity::system(
            dispatch.tenant_id.clone(),
            "computer-host",
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        let runtime = RuntimeStore::new(self.pool.clone(), identity)
            .map_err(|error| unavailable(error.to_string()))?;
        let run = runtime
            .load_run(&run_id)
            .await
            .map_err(|error| unavailable(error.to_string()))?;
        let Some(target_id) = run.execution_target_id.clone() else {
            return Err(HostFailure::Refused {
                detail: "the run has no execution target, so there is no computer to act on"
                    .to_string(),
            });
        };
        // A run that is no longer running (a person paused or cancelled it) sends nothing.
        if run.status != RunStatus::Running {
            return Ok(HostOutcome::Failure {
                retryable: false,
                error: terminal_error(&TerminalCondition::StoppedByUser),
                evidence_ids: Vec::new(),
            });
        }
        let mut conn =
            self.pool.acquire().await.map_err(|error| {
                unavailable(format!("computer control is unavailable: {error}"))
            })?;
        let control = match ComputerControlStore::load(&mut conn, &dispatch.tenant_id, &target_id)
            .await
        {
            Ok(control) => control,
            Err(ComputerControlError::NotFound(_)) => {
                return Err(HostFailure::Refused {
                    detail: format!("{target_id} is not a native computer: it has no control row"),
                })
            }
            Err(error) => return Err(unavailable(error.to_string())),
        };
        let policy = ComputerUsePolicy::new(self.known.clone(), self.grant);
        let dispatcher = ComputerDispatcher::with_config(
            SharedBridge(Arc::clone(&self.bridge)),
            policy,
            self.config.clone(),
        );
        let (token, stop) = self.stops.register(&dispatch.run_id);
        // One call is a sequence of one action: the runner owns the checks that come before an action, so the
        // dispatcher is never reached around them.
        let ran = ComputerSequence::new(&dispatcher, stop.clone())
            .run(
                &mut conn,
                &dispatch.tenant_id,
                &target_id,
                &dispatch.tool_call_id,
                control.generation,
                ListFeed::new([action]),
            )
            .await;
        self.stops.release(&dispatch.run_id, token);
        match ran {
            Ok(report) => settle(&report),
            Err(error) => dispatch_failure(error),
        }
    }
}

#[async_trait]
impl ToolHostPort for ComputerToolHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        if dispatch.tool.starts_with("computer.") {
            self.run(&dispatch).await
        } else {
            Err(HostFailure::Refused {
                detail: format!("{} is not a computer tool", dispatch.tool),
            })
        }
    }
}

/// Sends `computer.*` calls to the computer host and every other call to the host it wraps.
#[derive(Clone)]
pub struct ComputerRoutedHost {
    computer: Arc<ComputerToolHost>,
    rest: Arc<dyn ToolHostPort>,
}

impl std::fmt::Debug for ComputerRoutedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputerRoutedHost").finish_non_exhaustive()
    }
}

impl ComputerRoutedHost {
    /// Route `computer.*` to `computer`, the rest to `rest`.
    #[must_use]
    pub fn new(computer: Arc<ComputerToolHost>, rest: Arc<dyn ToolHostPort>) -> Self {
        Self { computer, rest }
    }
}

#[async_trait]
impl ToolHostPort for ComputerRoutedHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        if dispatch.tool.starts_with("computer.") {
            self.computer.execute(dispatch).await
        } else {
            self.rest.execute(dispatch).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_reaches_only_the_sequence_the_run_is_walking_and_a_release_drops_only_its_own_flag() {
        let registry = StopRegistry::new();
        assert!(!registry.stop("run_a"), "nothing in flight to stop");
        let (first, first_flag) = registry.register("run_a");
        let (_other, other_flag) = registry.register("run_b");
        assert!(registry.stop("run_a"));
        assert!(first_flag.is_stopped());
        assert!(
            !other_flag.is_stopped(),
            "another run's sequence is not stopped"
        );
        // A newer call for the same run replaces the registration; the old call's release does not drop it.
        let (second, second_flag) = registry.register("run_a");
        registry.release("run_a", first);
        assert!(registry.stop("run_a"));
        assert!(second_flag.is_stopped());
        registry.release("run_a", second);
        assert!(!registry.stop("run_a"), "released");
    }

    #[test]
    fn the_arguments_of_each_tool_become_the_action_the_machine_policy_judges() {
        let read = parse_action(
            "computer.read",
            &json!({ "application": "a.b", "max_nodes": 8, "max_depth": 2, "include_screenshot": true }),
        )
        .expect("read");
        assert!(matches!(
            read,
            NativeComputerAction::Read {
                max_nodes: 8,
                max_depth: 2,
                include_screenshot: true,
                ..
            }
        ));
        let key = parse_action(
            "computer.system_key",
            &json!({ "application": "a.b", "keys": ["cmd", "c"] }),
        )
        .expect("key");
        assert!(matches!(key, NativeComputerAction::SystemKey { ref keys, .. } if keys.len() == 2));
        assert!(parse_action(
            "computer.click",
            &json!({ "application": "a.b", "x": 1, "y": 2 })
        )
        .is_err());
        assert!(parse_action("computer.read", &json!({ "application": "a.b", "max_nodes": 70000, "max_depth": 2, "include_screenshot": false })).is_err());
        assert!(parse_action("browser.click", &json!({ "application": "a.b" })).is_err());
    }
}
