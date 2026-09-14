//! macOS local Linux capsule adapter (EXEC-003).
//!
//! The trusted Rust machine authority verifies the desktop-update image, the target generation and
//! the closed qworkerd service vocabulary before calling the narrow Swift
//! Virtualization.framework bridge. The guest has one private virtio-socket channel and no direct
//! network or host-filesystem device; egress remains a host-side broker decision.

mod catalog;

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::control::{ExecutionTarget, Substrate, TargetFence};
use crate::gateway::ActionEnvelope;

pub use catalog::{GuestImageCatalog, GUEST_IMAGES_YAML};

/// Fixed private virtio-socket port on which the guest image launches qworkerd.
pub const QWORKERD_VSOCK_PORT: u32 = 40_581;

/// A pinned artifact in the desktop update channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageArtifact {
    /// Local path materialized by the desktop updater.
    pub path: PathBuf,
    /// Lowercase SHA-256 supplied by the signed update catalog.
    pub sha256: String,
}

/// The complete immutable Linux image bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestImage {
    /// Update-channel release identity stored on the ExecutionTarget.
    pub release_digest: String,
    /// Host architecture the kernel was built for.
    pub architecture: String,
    /// Linux kernel.
    pub kernel: ImageArtifact,
    /// Optional initial ramdisk.
    pub initrd: Option<ImageArtifact>,
    /// Immutable raw root disk containing qworkerd, Chromium and base tooling.
    pub root_disk: ImageArtifact,
}

/// A configuration handed to the native bridge only after verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleLaunch {
    /// Canonical ExecutionTarget identity.
    pub target_id: String,
    /// Target generation this VM instance belongs to.
    pub generation: i64,
    /// Verified image bundle.
    pub image: GuestImage,
    /// Ephemeral writable data disk. Reset discards only this path.
    pub overlay_path: PathBuf,
    /// Host-bound machine-state file used for a full checkpoint.
    pub checkpoint_path: PathBuf,
    /// CPU limit.
    pub cpu_count: usize,
    /// Memory limit in bytes.
    pub memory_bytes: u64,
    /// The only private guest control port.
    pub qworkerd_port: u32,
}

/// Native VM execution state as reported by Virtualization.framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsuleState {
    /// No VM has been configured.
    Unconfigured,
    /// A validated VM exists and is stopped.
    Stopped,
    /// The guest is running.
    Running,
    /// The guest is paused for a checkpoint.
    Paused,
}

/// The deliberately small native bridge surface.
pub trait MacosCapsuleBridge {
    /// Current observed VM state.
    fn state(&self) -> CapsuleState;
    /// Build and validate the native VM configuration.
    fn configure(&mut self, launch: &CapsuleLaunch) -> Result<(), String>;
    /// Start the configured VM.
    fn start(&mut self) -> Result<(), String>;
    /// Stop the VM; repeated calls while stopped succeed.
    fn stop(&mut self) -> Result<(), String>;
    /// Discard and recreate only the ephemeral writable disk.
    fn reset_overlay(&mut self, overlay_path: &Path) -> Result<(), String>;
    /// Pause and save host-bound VM state.
    fn checkpoint(&mut self, checkpoint_path: &Path) -> Result<(), String>;
    /// Restore a stopped VM from host-bound state.
    fn restore(&mut self, checkpoint_path: &Path) -> Result<(), String>;
}

/// One of the three qworkerd service families admitted into the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestService {
    /// Workspace file service.
    File,
    /// Terminal and process service.
    Terminal,
    /// Browser service.
    Browser,
}

/// Fail-closed capsule errors.
#[derive(Debug, thiserror::Error)]
pub enum CapsuleError {
    /// The selected target is not a macOS local capsule.
    #[error("target {0} is not a local_capsule_macos ExecutionTarget")]
    WrongSubstrate(String),
    /// The controller is stale.
    #[error("target {target_id} generation is {current}, controller holds {held}")]
    StaleGeneration {
        /// Target identity.
        target_id: String,
        /// Current target generation.
        current: i64,
        /// Generation supplied by the controller.
        held: i64,
    },
    /// The target does not pin the selected update image.
    #[error("target image digest does not match the selected desktop-update image")]
    ImageSelectionMismatch,
    /// A pinned artifact is absent or unreadable.
    #[error("cannot read image artifact {path}: {source}")]
    ImageRead {
        /// Artifact path.
        path: PathBuf,
        /// Filesystem failure.
        source: io::Error,
    },
    /// A pinned artifact's bytes do not match the catalog.
    #[error("image digest mismatch for {path}: expected {expected}, got {actual}")]
    ImageDigestMismatch {
        /// Artifact path.
        path: PathBuf,
        /// Pinned digest.
        expected: String,
        /// Computed digest.
        actual: String,
    },
    /// The image is for another host architecture.
    #[error("guest image architecture {image} does not match host {host}")]
    ArchitectureMismatch {
        /// Image architecture.
        image: String,
        /// Host architecture.
        host: String,
    },
    /// An envelope names a service the capsule must never expose.
    #[error("tool {0} is not a file, terminal, process or browser qworkerd service")]
    ServiceDenied(String),
    /// Saved-login import must take the tier-4 approval path, never the guest channel.
    #[error("browser.session.import requires an explicit tier-4 approval")]
    CookieImportRequiresApproval,
    /// The desktop-update catalog is not a published, digest-pinned release.
    #[error("macos local capsule image is unavailable ({status}): {reason}")]
    ImageUnavailable {
        /// Catalog status (`BLOCKED_EXTERNAL`, `unpublished`, …).
        status: String,
        /// Human-readable blocker from the catalog.
        reason: String,
    },
    /// The embedded guest-image catalog is not the shape the controller requires.
    #[error("guest-image catalog is invalid: {0}")]
    CatalogInvalid(String),
    /// The native bridge refused the operation.
    #[error("macOS capsule bridge: {0}")]
    Native(String),
}

/// Verify one artifact without loading an entire VM disk into memory.
pub fn verify_artifact(artifact: &ImageArtifact) -> Result<(), CapsuleError> {
    let mut file = File::open(&artifact.path).map_err(|source| CapsuleError::ImageRead {
        path: artifact.path.clone(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| CapsuleError::ImageRead {
                path: artifact.path.clone(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || actual != artifact.sha256
    {
        return Err(CapsuleError::ImageDigestMismatch {
            path: artifact.path.clone(),
            expected: artifact.sha256.clone(),
            actual,
        });
    }
    Ok(())
}

/// Resolve the only service family the guest may execute.
pub fn guest_service(envelope: &ActionEnvelope) -> Result<GuestService, CapsuleError> {
    if envelope.tool == "browser.session.import" {
        return Err(CapsuleError::CookieImportRequiresApproval);
    }
    if envelope.tool.starts_with("fs.") {
        return Ok(GuestService::File);
    }
    if envelope.tool.starts_with("terminal.") || envelope.tool.starts_with("process.") {
        return Ok(GuestService::Terminal);
    }
    if envelope.tool.starts_with("browser.") {
        return Ok(GuestService::Browser);
    }
    Err(CapsuleError::ServiceDenied(envelope.tool.clone()))
}

/// Lifecycle adapter over the native bridge. Canonical target state remains in [`crate::control`].
pub struct MacosCapsuleController<B> {
    bridge: B,
    launch: Option<CapsuleLaunch>,
}

impl<B: MacosCapsuleBridge> MacosCapsuleController<B> {
    /// Construct an unconfigured controller.
    pub const fn new(bridge: B) -> Self {
        Self {
            bridge,
            launch: None,
        }
    }

    /// Observe native state without treating it as canonical state.
    pub fn observed_state(&self) -> CapsuleState {
        self.bridge.state()
    }

    /// Verify and configure a capsule for the exact target generation.
    pub fn prepare(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
        host_architecture: &str,
        launch: CapsuleLaunch,
    ) -> Result<(), CapsuleError> {
        validate_target(target, fence)?;
        if launch.target_id != target.id || launch.generation != target.generation {
            return Err(CapsuleError::StaleGeneration {
                target_id: target.id.clone(),
                current: target.generation,
                held: launch.generation,
            });
        }
        if target.image_digest.as_deref() != Some(launch.image.release_digest.as_str()) {
            return Err(CapsuleError::ImageSelectionMismatch);
        }
        if launch.image.architecture != host_architecture {
            return Err(CapsuleError::ArchitectureMismatch {
                image: launch.image.architecture.clone(),
                host: host_architecture.to_string(),
            });
        }
        if launch.qworkerd_port != QWORKERD_VSOCK_PORT {
            return Err(CapsuleError::ServiceDenied(format!(
                "private port {}",
                launch.qworkerd_port
            )));
        }
        verify_artifact(&launch.image.kernel)?;
        if let Some(initrd) = &launch.image.initrd {
            verify_artifact(initrd)?;
        }
        verify_artifact(&launch.image.root_disk)?;
        self.bridge
            .configure(&launch)
            .map_err(CapsuleError::Native)?;
        self.launch = Some(launch);
        Ok(())
    }

    /// Start once; a repeated start of the same running instance is idempotent.
    pub fn start(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<CapsuleState, CapsuleError> {
        self.validate_prepared(target, fence)?;
        if self.bridge.state() != CapsuleState::Running {
            self.bridge.start().map_err(CapsuleError::Native)?;
        }
        Ok(self.bridge.state())
    }

    /// Stop once; a repeated stop is idempotent.
    pub fn stop(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<CapsuleState, CapsuleError> {
        self.validate_prepared(target, fence)?;
        if self.bridge.state() != CapsuleState::Stopped {
            self.bridge.stop().map_err(CapsuleError::Native)?;
        }
        Ok(self.bridge.state())
    }

    /// Stop, discard the writable disk, reconfigure and restart the same verified image.
    pub fn reset(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<CapsuleState, CapsuleError> {
        self.validate_prepared(target, fence)?;
        if self.bridge.state() != CapsuleState::Stopped {
            self.bridge.stop().map_err(CapsuleError::Native)?;
        }
        let launch = self.launch.as_ref().expect("validated above");
        self.bridge
            .reset_overlay(&launch.overlay_path)
            .map_err(CapsuleError::Native)?;
        self.bridge
            .configure(launch)
            .map_err(CapsuleError::Native)?;
        self.bridge.start().map_err(CapsuleError::Native)?;
        Ok(self.bridge.state())
    }

    /// Save a repeatable full checkpoint through the native bridge.
    pub fn checkpoint(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<PathBuf, CapsuleError> {
        self.validate_prepared(target, fence)?;
        let path = self
            .launch
            .as_ref()
            .expect("validated above")
            .checkpoint_path
            .clone();
        self.bridge
            .checkpoint(&path)
            .map_err(CapsuleError::Native)?;
        Ok(path)
    }

    /// Restore a checkpoint after rechecking the target fence.
    pub fn restore(
        &mut self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<CapsuleState, CapsuleError> {
        self.validate_prepared(target, fence)?;
        if self.bridge.state() != CapsuleState::Stopped {
            self.bridge.stop().map_err(CapsuleError::Native)?;
        }
        let path = self
            .launch
            .as_ref()
            .expect("validated above")
            .checkpoint_path
            .clone();
        self.bridge.restore(&path).map_err(CapsuleError::Native)?;
        Ok(self.bridge.state())
    }

    fn validate_prepared(
        &self,
        target: &ExecutionTarget,
        fence: &TargetFence,
    ) -> Result<(), CapsuleError> {
        validate_target(target, fence)?;
        let Some(launch) = &self.launch else {
            return Err(CapsuleError::Native(
                "capsule is not configured".to_string(),
            ));
        };
        if launch.target_id != target.id || launch.generation != target.generation {
            return Err(CapsuleError::StaleGeneration {
                target_id: target.id.clone(),
                current: target.generation,
                held: launch.generation,
            });
        }
        Ok(())
    }
}

fn validate_target(target: &ExecutionTarget, fence: &TargetFence) -> Result<(), CapsuleError> {
    if target.substrate != Substrate::LocalCapsuleMacos {
        return Err(CapsuleError::WrongSubstrate(target.id.clone()));
    }
    if target.generation != fence.generation {
        return Err(CapsuleError::StaleGeneration {
            target_id: target.id.clone(),
            current: target.generation,
            held: fence.generation,
        });
    }
    Ok(())
}
