//! EXEC-003 macOS capsule authority tests.
//!
//! The platform-neutral suite proves digest pinning, generation fencing, the closed qworkerd service
//! vocabulary and repeatable lifecycle sequencing. The native package owns the real
//! Virtualization.framework boundary test.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use quansio_core::Digest;
use quansio_machine::control::{
    ExecutionTarget, Substrate, TargetClass, TargetFence, TargetStatus,
};
use quansio_machine::gateway::{ActionEnvelope, EnvelopeKind};
use quansio_machine::substrates::macos_capsule::{
    guest_service, verify_artifact, CapsuleError, CapsuleLaunch, CapsuleState, GuestImage,
    GuestService, ImageArtifact, MacosCapsuleBridge, MacosCapsuleController, QWORKERD_VSOCK_PORT,
};

#[derive(Clone)]
struct RecordingBridge {
    state: CapsuleState,
    calls: Arc<Mutex<Vec<String>>>,
}

impl RecordingBridge {
    fn new(calls: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            state: CapsuleState::Unconfigured,
            calls,
        }
    }

    fn record(&self, call: &str) {
        self.calls
            .lock()
            .expect("calls lock")
            .push(call.to_string());
    }
}

impl MacosCapsuleBridge for RecordingBridge {
    fn state(&self) -> CapsuleState {
        self.state
    }

    fn configure(&mut self, _: &CapsuleLaunch) -> Result<(), String> {
        self.record("configure");
        self.state = CapsuleState::Stopped;
        Ok(())
    }

    fn start(&mut self) -> Result<(), String> {
        self.record("start");
        self.state = CapsuleState::Running;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        self.record("stop");
        self.state = CapsuleState::Stopped;
        Ok(())
    }

    fn reset_overlay(&mut self, _: &Path) -> Result<(), String> {
        self.record("reset_overlay");
        Ok(())
    }

    fn checkpoint(&mut self, _: &Path) -> Result<(), String> {
        self.record("checkpoint");
        Ok(())
    }

    fn restore(&mut self, _: &Path) -> Result<(), String> {
        self.record("restore");
        self.state = CapsuleState::Paused;
        Ok(())
    }
}

fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quansio-exec-003-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create scratch directory");
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write scratch artifact");
    path
}

fn artifact(name: &str, bytes: &[u8]) -> ImageArtifact {
    ImageArtifact {
        path: scratch(name, bytes),
        sha256: Digest::of(bytes).to_string(),
    }
}

fn target(generation: i64) -> ExecutionTarget {
    ExecutionTarget {
        id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        workspace_id: "wsp_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        class: TargetClass::PersistentWorkspaceComputer,
        substrate: Substrate::LocalCapsuleMacos,
        status: TargetStatus::Ready,
        generation,
        lease_id: Some("lse_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string()),
        image_digest: Some("sha256:release".to_string()),
        last_heartbeat_at: None,
        desired_state: Some("READY".to_string()),
        observed_state: None,
    }
}

fn launch() -> CapsuleLaunch {
    CapsuleLaunch {
        target_id: target(7).id,
        generation: 7,
        image: GuestImage {
            release_digest: "sha256:release".to_string(),
            architecture: "arm64".to_string(),
            kernel: artifact("kernel", b"verified linux kernel"),
            initrd: Some(artifact("initrd", b"verified initrd")),
            root_disk: artifact("root.raw", b"verified root disk with qworkerd"),
        },
        overlay_path: scratch("overlay.raw", b"ephemeral"),
        checkpoint_path: scratch("checkpoint.vzstate", b"host-bound"),
        cpu_count: 2,
        memory_bytes: 2 * 1024 * 1024 * 1024,
        qworkerd_port: QWORKERD_VSOCK_PORT,
    }
}

fn envelope(tool: &str) -> ActionEnvelope {
    ActionEnvelope {
        kind: EnvelopeKind::Dispatch,
        effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        dispatch_token: "dispatch-token".to_string(),
        target_id: target(7).id,
        lease_id: "lse_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        generation: 7,
        run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        tool: tool.to_string(),
        tool_version: 1,
        args_json: "{}".to_string(),
        args_digest: Digest::of_canonical_json("{}").to_string(),
        max_output_bytes: 1024,
        deadline_at: "2026-09-14T08:00:00Z".to_string(),
    }
}

#[test]
fn image_bytes_are_verified_and_mismatch_fails_closed() {
    let good = artifact("digest-good", b"known bytes");
    verify_artifact(&good).expect("matching digest");

    let mut wrong = good;
    wrong.sha256 = "0".repeat(64);
    assert!(matches!(
        verify_artifact(&wrong),
        Err(CapsuleError::ImageDigestMismatch { .. })
    ));
}

#[test]
fn lifecycle_is_repeatable_and_reset_discards_only_the_overlay() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut controller = MacosCapsuleController::new(RecordingBridge::new(calls.clone()));
    let target = target(7);
    let fence = TargetFence { generation: 7 };

    controller
        .prepare(&target, &fence, "arm64", launch())
        .expect("prepare verified image");
    assert_eq!(
        controller.start(&target, &fence).unwrap(),
        CapsuleState::Running
    );
    assert_eq!(
        controller.start(&target, &fence).unwrap(),
        CapsuleState::Running
    );
    controller.checkpoint(&target, &fence).expect("checkpoint");
    assert_eq!(
        controller.stop(&target, &fence).unwrap(),
        CapsuleState::Stopped
    );
    assert_eq!(
        controller.stop(&target, &fence).unwrap(),
        CapsuleState::Stopped
    );
    assert_eq!(
        controller.restore(&target, &fence).unwrap(),
        CapsuleState::Paused
    );
    assert_eq!(
        controller.reset(&target, &fence).unwrap(),
        CapsuleState::Running
    );

    assert_eq!(
        *calls.lock().unwrap(),
        [
            "configure",
            "start",
            "checkpoint",
            "stop",
            "restore",
            "stop",
            "reset_overlay",
            "configure",
            "start",
        ]
    );
}

#[test]
fn stale_generation_is_refused_before_native_execution() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut controller = MacosCapsuleController::new(RecordingBridge::new(calls.clone()));
    let error = controller
        .prepare(
            &target(8),
            &TargetFence { generation: 7 },
            "arm64",
            launch(),
        )
        .expect_err("stale fence must fail");
    assert!(matches!(error, CapsuleError::StaleGeneration { .. }));
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn guest_channel_has_a_closed_service_vocabulary() {
    assert_eq!(
        guest_service(&envelope("fs.read")).unwrap(),
        GuestService::File
    );
    assert_eq!(
        guest_service(&envelope("terminal.exec")).unwrap(),
        GuestService::Terminal
    );
    assert_eq!(
        guest_service(&envelope("process.spawn")).unwrap(),
        GuestService::Terminal
    );
    assert_eq!(
        guest_service(&envelope("browser.navigate")).unwrap(),
        GuestService::Browser
    );
    assert!(matches!(
        guest_service(&envelope("connector.github.write")),
        Err(CapsuleError::ServiceDenied(_))
    ));
    assert!(matches!(
        guest_service(&envelope("browser.session.import")),
        Err(CapsuleError::CookieImportRequiresApproval)
    ));
}
