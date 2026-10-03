//! Shared fixtures for the computer-use suites (ADP-002): a real PostgreSQL target with a control row, and
//! a scriptable platform bridge.
//!
//! The bridge here stands in for the *platform* so policy can be driven through every branch with real
//! durable control state behind it. It is never offered as evidence about macOS: that is the Swift suite
//! (`swift test --package-path native/macos`), and the helper suite runs real `codesign`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use quansio_machine::computer_use::bridge::{
    NativeBridgeError, NativeComputerAction, NativeComputerBridge,
};
use quansio_machine::computer_use::permission::{ForegroundControl, PermissionStatus};
use quansio_machine::computer_use::receipt::{
    ActionReceipt, BridgeReport, ExecutionMethod, ReceiptMethod,
};
use quansio_machine::computer_use::store::ComputerControlStore;
use quansio_machine::computer_use::{
    AppIdentity, ComputerGrant, ComputerTier, ComputerUsePolicy, KnownApps,
};
use quansio_machine::control::{MachineControl, NewTarget, Substrate, TargetClass};
use serde_json::{json, Value};
use sqlx::PgPool;

pub const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
pub const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
pub const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
pub const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
pub const CALL_A: &str = "tc_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
pub const CALL_B: &str = "tc_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";
pub const AT: &str = "2026-10-03T10:00:00Z";
pub const APP: &str = "com.apple.TextEdit";

/// An application the policy recognises.
pub fn known_app() -> AppIdentity {
    AppIdentity::new(APP, "TextEdit", 4242)
}

/// A policy that recognises TextEdit and grants up to `tier`.
pub fn policy(tier: ComputerTier) -> ComputerUsePolicy {
    ComputerUsePolicy::new(KnownApps::of([APP]), ComputerGrant::up_to(tier))
}

/// A click at (10, 20) in the known app.
pub fn click() -> NativeComputerAction {
    NativeComputerAction::Click {
        application: APP.to_string(),
        x: 10,
        y: 20,
        button: "left".to_string(),
    }
}

/// Typing `text` into the known app.
pub fn type_text(text: &str) -> NativeComputerAction {
    NativeComputerAction::Type {
        application: APP.to_string(),
        text: text.to_string(),
    }
}

/// A read of the known app.
pub fn read(include_screenshot: bool) -> NativeComputerAction {
    NativeComputerAction::Read {
        application: APP.to_string(),
        max_nodes: 16,
        max_depth: 3,
        include_screenshot,
    }
}

/// A clipboard write in the known app.
pub fn clipboard_write(text: &str) -> NativeComputerAction {
    NativeComputerAction::Clipboard {
        application: APP.to_string(),
        operation: "write".to_string(),
        text: text.to_string(),
    }
}

/// What the scripted bridge does on its next `perform`.
pub enum Step {
    /// Answer with this report.
    Report(BridgeReport),
    /// Fail with this code, as a bridge that disconnected after dispatch would.
    Fail(&'static str),
}

type Hook = Box<dyn Fn(usize) + Send + Sync>;

struct Inner {
    foreground: AppIdentity,
    permissions: Mutex<PermissionStatus>,
    foreground_control: Mutex<ForegroundControl>,
    script: Mutex<VecDeque<Step>>,
    performed: AtomicUsize,
    hook: Mutex<Option<Hook>>,
    status_calls: AtomicUsize,
    fail_status: Mutex<bool>,
}

/// A bridge whose answers a test scripts. Cloning shares the state, so the test keeps a handle after the
/// dispatcher takes ownership.
#[derive(Clone)]
pub struct ScriptedBridge(Arc<Inner>);

impl ScriptedBridge {
    /// A bridge with TextEdit in front, every permission granted, foreground control held and an empty
    /// script (every action is answered with the weakest honest receipt: input sent, effect unverified).
    pub fn new() -> Self {
        Self(Arc::new(Inner {
            foreground: known_app(),
            permissions: Mutex::new(PermissionStatus::all_granted()),
            foreground_control: Mutex::new(ForegroundControl::Held),
            script: Mutex::new(VecDeque::new()),
            performed: AtomicUsize::new(0),
            hook: Mutex::new(None),
            status_calls: AtomicUsize::new(0),
            fail_status: Mutex::new(false),
        }))
    }

    /// Queue the answer for a later `perform`.
    pub fn push(&self, step: Step) -> &Self {
        self.0.script.lock().expect("script").push_back(step);
        self
    }

    /// Replace the permission status the bridge reports.
    pub fn set_permissions(&self, status: PermissionStatus) {
        *self.0.permissions.lock().expect("permissions") = status;
    }

    /// Replace the foreground-control answer.
    pub fn set_foreground_control(&self, control: ForegroundControl) {
        *self.0.foreground_control.lock().expect("control") = control;
    }

    /// Make the permission probe itself fail.
    pub fn fail_status(&self, fail: bool) {
        *self.0.fail_status.lock().expect("fail") = fail;
    }

    /// Run `hook(call_index)` at the start of every `perform` (index counted from 1), before its answer.
    pub fn on_perform(&self, hook: impl Fn(usize) + Send + Sync + 'static) {
        *self.0.hook.lock().expect("hook") = Some(Box::new(hook));
    }

    /// How many actions reached the platform.
    pub fn performed(&self) -> usize {
        self.0.performed.load(Ordering::SeqCst)
    }

    /// How many times the permission status was read.
    pub fn status_calls(&self) -> usize {
        self.0.status_calls.load(Ordering::SeqCst)
    }
}

impl NativeComputerBridge for ScriptedBridge {
    fn foreground_app(&self) -> Result<AppIdentity, NativeBridgeError> {
        Ok(self.0.foreground.clone())
    }

    fn execute(&self, action: &NativeComputerAction) -> Result<Value, NativeBridgeError> {
        Ok(json!({ "application": action.application() }))
    }

    fn permission_status(&self) -> Result<PermissionStatus, NativeBridgeError> {
        self.0.status_calls.fetch_add(1, Ordering::SeqCst);
        if *self.0.fail_status.lock().expect("fail") {
            return Err(NativeBridgeError::new("status_unreadable"));
        }
        Ok(*self.0.permissions.lock().expect("permissions"))
    }

    fn foreground_control(&self) -> Result<ForegroundControl, NativeBridgeError> {
        Ok(*self.0.foreground_control.lock().expect("control"))
    }

    fn perform(&self, action: &NativeComputerAction) -> Result<BridgeReport, NativeBridgeError> {
        let index = self.0.performed.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(hook) = self.0.hook.lock().expect("hook").as_ref() {
            hook(index);
        }
        let step = self.0.script.lock().expect("script").pop_front();
        match step {
            Some(Step::Report(report)) => Ok(report),
            Some(Step::Fail(code)) => Err(NativeBridgeError::new(code)),
            None => {
                let method = action.default_method();
                let receipt = if action.is_consequential() {
                    ActionReceipt::unverified(method, ExecutionMethod::Foreground)
                } else {
                    ActionReceipt::observed(method, ExecutionMethod::Foreground)
                };
                Ok(BridgeReport {
                    output: json!({ "application": action.application() }),
                    receipt,
                })
            }
        }
    }
}

/// A report whose receipt `applied`.
pub fn applied_report() -> BridgeReport {
    BridgeReport {
        output: json!({ "clicked": true }),
        receipt: ActionReceipt::applied(ReceiptMethod::HidEvent, ExecutionMethod::Foreground),
    }
}

/// Create a scratch database with a tenant, a target and its control row.
pub async fn prepare(prefix: &str) -> Option<(String, PgPool)> {
    let name = super::scratch_name(prefix);
    let pool = super::fresh_database(&name).await?;
    super::seed_tenant(&pool, TENANT, USER, WORKSPACE).await;
    let mut conn = pool.acquire().await.expect("acquire");
    MachineControl::register(
        &mut conn,
        TENANT,
        &NewTarget {
            id: TARGET.to_string(),
            workspace_id: WORKSPACE.to_string(),
            class: TargetClass::PersistentWorkspaceComputer,
            substrate: Substrate::LocalCapsuleMacos,
            image_digest: None,
            desired_state: None,
        },
    )
    .await
    .expect("register target");
    ComputerControlStore::open(&mut conn, TENANT, TARGET, None)
        .await
        .expect("open control");
    drop(conn);
    Some((name, pool))
}
