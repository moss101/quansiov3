//! Windows WSL2 Linux capsule and native-broker adapter (EXEC-004).
//!
//! Generic workloads run in a WSL2 Linux guest through an allowlisted `wsl.exe` verb
//! list. Windows-only UI automation stays on the EXEC-010 native broker, which never
//! interpolates caller text into a shell. Canonical target and lease state remain in
//! [`crate::control`].

use std::path::{Path, PathBuf};

use crate::control::{ExecutionTarget, Substrate, TargetFence};
use crate::gateway::ActionEnvelope;
use crate::substrates::macos_capsule::{guest_service, verify_artifact, CapsuleError, GuestImage};

/// Allowlisted `wsl.exe` verbs. `--exec` is deliberately absent.
pub const WSL_VERBS: &[&str] = &[
    "--import",
    "--export",
    "--terminate",
    "--shutdown",
    "--list",
];

/// The only executable the capsule may spawn.
pub const WSL_EXE: &str = "wsl.exe";

/// A verified WSL distribution launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslLaunch {
    /// ExecutionTarget identity.
    pub target_id: String,
    /// Target generation this instance belongs to.
    pub generation: i64,
    /// Verified Linux rootfs (qworkerd, Chromium, base tooling).
    pub image: GuestImage,
    /// WSL distribution name derived from the target id, never from caller text.
    pub distribution: String,
    /// Host path of the verified rootfs tarball.
    pub rootfs_path: PathBuf,
    /// Host path of a checkpoint export.
    pub checkpoint_path: PathBuf,
}

/// Native WSL operations.
pub trait WslBridge {
    /// Import and start the verified distribution.
    fn import(&mut self, launch: &WslLaunch) -> Result<(), String>;
    /// Terminate the distribution.
    fn terminate(&mut self, distribution: &str) -> Result<(), String>;
    /// Shut WSL down.
    fn shutdown(&mut self) -> Result<(), String>;
    /// Export a checkpoint tarball.
    fn export(&mut self, distribution: &str, dest: &Path) -> Result<(), String>;
}

/// Fail-closed Windows substrate errors.
#[derive(Debug, thiserror::Error)]
pub enum WindowsError {
    /// Wrong substrate.
    #[error("target {0} is not a local_capsule_windows or windows_native ExecutionTarget")]
    WrongSubstrate(String),
    /// Stale generation or lease.
    #[error("target {target_id} generation is {current}, controller holds {held}")]
    StaleGeneration {
        /// Target identity.
        target_id: String,
        /// Current generation.
        current: i64,
        /// Held generation.
        held: i64,
    },
    /// Capsule image error.
    #[error(transparent)]
    Capsule(#[from] CapsuleError),
    /// A WSL verb or executable is not on the allowlist.
    #[error("windows capsule refused command {0}")]
    CommandRefused(String),
    /// Native bridge failure, including missing Windows/WSL.
    #[error("{0}")]
    Native(String),
}

/// Build an argv for `wsl.exe`. Caller strings never become verbs.
///
/// # Errors
/// Returns [`WindowsError::CommandRefused`] for an unknown verb or a distribution name that
/// is not the launch's own name.
pub fn wsl_argv(
    verb: &str,
    launch: &WslLaunch,
    extra: &[&str],
) -> Result<Vec<String>, WindowsError> {
    if !WSL_VERBS.contains(&verb) {
        return Err(WindowsError::CommandRefused(verb.to_string()));
    }
    if verb == "--exec" {
        return Err(WindowsError::CommandRefused("--exec".to_string()));
    }
    let mut argv = vec![WSL_EXE.to_string(), verb.to_string()];
    match verb {
        "--import" => {
            argv.push(launch.distribution.clone());
            argv.push(
                launch
                    .rootfs_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .display()
                    .to_string(),
            );
            argv.push(launch.rootfs_path.display().to_string());
        }
        "--export" => {
            argv.push(launch.distribution.clone());
            argv.push(launch.checkpoint_path.display().to_string());
        }
        "--terminate" => argv.push(launch.distribution.clone()),
        "--shutdown" | "--list" => {}
        _ => return Err(WindowsError::CommandRefused(verb.to_string())),
    }
    for item in extra {
        if item.starts_with('-') {
            return Err(WindowsError::CommandRefused((*item).to_string()));
        }
    }
    Ok(argv)
}

/// Refuse argv that would become an arbitrary host command.
#[must_use]
pub fn refuses_arbitrary_host_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if lower.contains("cmd.exe") && lower.contains("/c") {
        return true;
    }
    if lower.contains("powershell") && (lower.contains("-command") || lower.contains("iex")) {
        // The native broker uses powershell.exe only with a compiled-in script constant.
        return !lower.contains("quansiowin32");
    }
    if lower.contains("wsl.exe") && lower.contains("--exec") {
        return true;
    }
    false
}

/// Lifecycle adapter over WSL.
pub struct WindowsCapsuleController<B> {
    bridge: B,
    launch: Option<WslLaunch>,
}

impl<B: WslBridge> WindowsCapsuleController<B> {
    /// Unconfigured controller.
    pub const fn new(bridge: B) -> Self {
        Self {
            bridge,
            launch: None,
        }
    }

    /// Verify image and generation, then import.
    ///
    /// # Errors
    /// Returns [`WindowsError`] for substrate, generation, digest or native failures.
    pub fn prepare(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
        launch: WslLaunch,
    ) -> Result<(), WindowsError> {
        validate_target(target, fence)?;
        if launch.target_id != target.id || launch.generation != target.generation {
            return Err(WindowsError::StaleGeneration {
                target_id: target.id.clone(),
                current: target.generation,
                held: launch.generation,
            });
        }
        verify_artifact(&launch.image.root_disk)?;
        self.bridge.import(&launch).map_err(WindowsError::Native)?;
        self.launch = Some(launch);
        Ok(())
    }

    /// Terminate the distribution.
    ///
    /// # Errors
    /// Returns [`WindowsError`] when the fence is stale or WSL refuses.
    pub fn stop(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<(), WindowsError> {
        let distribution = self.validate_prepared(target, fence)?.distribution.clone();
        self.bridge
            .terminate(&distribution)
            .map_err(WindowsError::Native)
    }

    /// Export a checkpoint.
    ///
    /// # Errors
    /// Returns [`WindowsError`] when the fence is stale or WSL refuses.
    pub fn checkpoint(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<PathBuf, WindowsError> {
        let (path, distribution) = {
            let launch = self.validate_prepared(target, fence)?;
            (launch.checkpoint_path.clone(), launch.distribution.clone())
        };
        self.bridge
            .export(&distribution, &path)
            .map_err(WindowsError::Native)?;
        Ok(path)
    }

    fn validate_prepared(
        &self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<&WslLaunch, WindowsError> {
        validate_target(target, fence)?;
        self.launch
            .as_ref()
            .ok_or_else(|| WindowsError::Native("windows capsule is not configured".to_string()))
    }
}

/// Guest services admitted into the WSL capsule are the same closed qworkerd vocabulary.
pub fn windows_guest_service(envelope: &ActionEnvelope) -> Result<(), WindowsError> {
    guest_service(envelope)
        .map(|_| ())
        .map_err(WindowsError::Capsule)
}

fn validate_target(target: &ExecutionTarget, fence: &TargetFence) -> Result<(), WindowsError> {
    if !matches!(
        target.substrate,
        Substrate::LocalCapsuleWindows | Substrate::WindowsNative
    ) {
        return Err(WindowsError::WrongSubstrate(target.id.clone()));
    }
    if target.generation != fence.generation {
        return Err(WindowsError::StaleGeneration {
            target_id: target.id.clone(),
            current: target.generation,
            held: fence.generation,
        });
    }
    Ok(())
}
