//! Typed OS permission status and the guide a missing one produces (ADP-002, DOMAIN.md §8.6.1 E).
//!
//! The bridge answers *what the system says*; this module decides what that means for an action. A
//! missing permission is not something a model can fix by trying again, so the answer is an
//! [`AttentionRequest`]: the run parks, a person is guided to the right Settings pane (by the desktop, on
//! their click), and the run resumes only when they resume it. The agent never operates System Settings,
//! and nothing here opens one.

use serde_json::{json, Value};

use super::bridge::NativeComputerAction;
use super::config::PermissionPanes;

/// An OS permission the native bridge may need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// Accessibility: read other applications' controls and post input to them.
    Accessibility,
    /// Screen Recording: read pixels and window titles.
    ScreenRecording,
    /// Input Monitoring: observe the person's keyboard and pointer.
    InputMonitoring,
    /// Microphone.
    Microphone,
}

impl Permission {
    /// Every permission, in the order the status reports them.
    pub const ALL: [Self; 4] = [
        Self::Accessibility,
        Self::ScreenRecording,
        Self::InputMonitoring,
        Self::Microphone,
    ];

    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::ScreenRecording => "screen_recording",
            Self::InputMonitoring => "input_monitoring",
            Self::Microphone => "microphone",
        }
    }

    /// Parse the canonical wire name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value)
    }
}

/// What the system says about one permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    /// The process may use it.
    Granted,
    /// The person refused it, or macOS reports no grant and cannot say whether it was asked.
    Denied,
    /// The person has not been asked.
    NotDetermined,
    /// A policy (for example a device-management profile) forbids it; the person cannot grant it.
    Restricted,
}

impl PermissionState {
    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotDetermined => "not_determined",
            Self::Restricted => "restricted",
        }
    }

    /// Parse the canonical wire name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "granted" => Some(Self::Granted),
            "denied" => Some(Self::Denied),
            "not_determined" => Some(Self::NotDetermined),
            "restricted" => Some(Self::Restricted),
            _ => None,
        }
    }
}

/// The bridge's answer for all four permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionStatus {
    /// Accessibility.
    pub accessibility: PermissionState,
    /// Screen Recording.
    pub screen_recording: PermissionState,
    /// Input Monitoring.
    pub input_monitoring: PermissionState,
    /// Microphone.
    pub microphone: PermissionState,
}

impl PermissionStatus {
    /// A status with every permission granted.
    #[must_use]
    pub const fn all_granted() -> Self {
        Self {
            accessibility: PermissionState::Granted,
            screen_recording: PermissionState::Granted,
            input_monitoring: PermissionState::Granted,
            microphone: PermissionState::Granted,
        }
    }

    /// The state of one permission.
    #[must_use]
    pub const fn get(&self, permission: Permission) -> PermissionState {
        match permission {
            Permission::Accessibility => self.accessibility,
            Permission::ScreenRecording => self.screen_recording,
            Permission::InputMonitoring => self.input_monitoring,
            Permission::Microphone => self.microphone,
        }
    }

    /// A copy with one permission changed.
    #[must_use]
    pub const fn with(mut self, permission: Permission, state: PermissionState) -> Self {
        match permission {
            Permission::Accessibility => self.accessibility = state,
            Permission::ScreenRecording => self.screen_recording = state,
            Permission::InputMonitoring => self.input_monitoring = state,
            Permission::Microphone => self.microphone = state,
        }
        self
    }

    /// The wire form: exactly the four permissions.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "accessibility": self.accessibility.as_str(),
            "screen_recording": self.screen_recording.as_str(),
            "input_monitoring": self.input_monitoring.as_str(),
            "microphone": self.microphone.as_str(),
        })
    }

    /// Parse the wire form strictly: all four keys, no others, each a known state.
    ///
    /// # Errors
    /// Returns a description of the first thing wrong. A status that is partly understood is not used:
    /// guessing a missing permission is granted would be the opposite of fail closed.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "a permission status must be an object".to_string())?;
        if let Some(extra) = object.keys().find(|key| Permission::parse(key).is_none()) {
            return Err(format!("unknown permission {extra:?}"));
        }
        let state = |permission: Permission| -> Result<PermissionState, String> {
            object
                .get(permission.as_str())
                .and_then(Value::as_str)
                .and_then(PermissionState::parse)
                .ok_or_else(|| format!("{} is missing or not a known state", permission.as_str()))
        };
        Ok(Self {
            accessibility: state(Permission::Accessibility)?,
            screen_recording: state(Permission::ScreenRecording)?,
            input_monitoring: state(Permission::InputMonitoring)?,
            microphone: state(Permission::Microphone)?,
        })
    }
}

/// Whether the person's own input has taken the foreground back from the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForegroundControl {
    /// The agent's input is the most recent input on the machine.
    Held,
    /// Input from the person arrived after the agent's last input; the grant to drive the foreground
    /// has lapsed until they pause.
    Lapsed,
}

impl ForegroundControl {
    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Lapsed => "lapsed",
        }
    }
}

/// The permissions an action needs before it may reach the bridge.
#[must_use]
pub fn required_permissions(action: &NativeComputerAction) -> Vec<Permission> {
    match action {
        NativeComputerAction::Read {
            include_screenshot, ..
        } => {
            let mut needed = vec![Permission::Accessibility];
            if *include_screenshot {
                needed.push(Permission::ScreenRecording);
            }
            needed
        }
        NativeComputerAction::Click { .. }
        | NativeComputerAction::Type { .. }
        | NativeComputerAction::SystemKey { .. } => vec![Permission::Accessibility],
        NativeComputerAction::Clipboard { .. } => Vec::new(),
    }
}

/// Whether an action drives the foreground (and so needs the person to be out of the way).
#[must_use]
pub const fn drives_foreground(action: &NativeComputerAction) -> bool {
    matches!(
        action,
        NativeComputerAction::Click { .. }
            | NativeComputerAction::Type { .. }
            | NativeComputerAction::SystemKey { .. }
    )
}

/// What the person is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuideAction {
    /// Open the named Privacy pane and allow Quansio.
    OpenSettings {
        /// The Privacy pane anchor (from configuration).
        pane: String,
    },
    /// The permission is restricted by policy; only an administrator can lift it.
    ContactAdministrator,
    /// Stop using the keyboard and pointer for a moment. Settings are not involved.
    PauseInput,
}

impl GuideAction {
    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::OpenSettings { .. } => "open_settings",
            Self::ContactAdministrator => "contact_administrator",
            Self::PauseInput => "pause_input",
        }
    }
}

/// Why the run is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionReason {
    /// A permission the action needs is not granted.
    PermissionMissing,
    /// A permission the action needs is restricted by policy.
    PermissionRestricted,
    /// The person's own input has taken the foreground back.
    ForegroundControlLapsed,
}

/// The attention item a missing permission parks the run on.
///
/// The runtime parks the Run `SUSPENDED` with [`Self::code`] and delivers a `blocked` Notification
/// ([`Self::NOTIFICATION_KIND`]); the run leaves that state only through the person's `ResumeRun`
/// ([`Self::RESUME_COMMAND`]). Nothing a model returns appears in this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionRequest {
    /// Why.
    pub reason: AttentionReason,
    /// The permission, when one is the cause.
    pub permission: Option<Permission>,
    /// What the system said about it.
    pub status: Option<PermissionState>,
    /// What the person is asked to do.
    pub guide: GuideAction,
}

impl AttentionRequest {
    /// The Notification kind (DOMAIN.md §13.3) the item is delivered as.
    pub const NOTIFICATION_KIND: &'static str = "blocked";
    /// The only command that leaves the parked state.
    pub const RESUME_COMMAND: &'static str = "ResumeRun";

    /// The typed error code (DOMAIN.md §15).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.reason {
            AttentionReason::ForegroundControlLapsed => "COMPUTER_FOREGROUND_CONTROL_LAPSED",
            AttentionReason::PermissionMissing | AttentionReason::PermissionRestricted => {
                "COMPUTER_PERMISSION_REQUIRED"
            }
        }
    }

    /// The wire form handed to the runtime and the desktop.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut guide = json!({ "action": self.guide.as_str() });
        if let GuideAction::OpenSettings { pane } = &self.guide {
            guide["pane"] = json!(pane);
        }
        json!({
            "code": self.code(),
            "notification_kind": Self::NOTIFICATION_KIND,
            "resume_command": Self::RESUME_COMMAND,
            "permission": self.permission.map(Permission::as_str),
            "status": self.status.map(PermissionState::as_str),
            "guide": guide,
        })
    }
}

/// Decide whether an action may reach the bridge given what the system says.
///
/// Permissions come first, in the status's own order, so the first missing one is the one the person is
/// guided to; a lapsed foreground-control grant is only considered when every permission is in place,
/// because opening Settings cannot fix it and pausing input cannot fix a permission.
///
/// # Errors
/// Returns the [`AttentionRequest`] the run parks on.
pub fn gate(
    action: &NativeComputerAction,
    status: &PermissionStatus,
    foreground: ForegroundControl,
    panes: &PermissionPanes,
) -> Result<(), AttentionRequest> {
    let mut needed = required_permissions(action);
    needed.sort();
    for permission in needed {
        let state = status.get(permission);
        match state {
            PermissionState::Granted => {}
            PermissionState::Restricted => {
                return Err(AttentionRequest {
                    reason: AttentionReason::PermissionRestricted,
                    permission: Some(permission),
                    status: Some(state),
                    guide: GuideAction::ContactAdministrator,
                });
            }
            PermissionState::Denied | PermissionState::NotDetermined => {
                return Err(AttentionRequest {
                    reason: AttentionReason::PermissionMissing,
                    permission: Some(permission),
                    status: Some(state),
                    guide: GuideAction::OpenSettings {
                        pane: panes.pane(permission).to_string(),
                    },
                });
            }
        }
    }
    if drives_foreground(action) && foreground == ForegroundControl::Lapsed {
        return Err(AttentionRequest {
            reason: AttentionReason::ForegroundControlLapsed,
            permission: None,
            status: None,
            guide: GuideAction::PauseInput,
        });
    }
    Ok(())
}
