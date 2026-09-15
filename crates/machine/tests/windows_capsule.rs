//! EXEC-004 Windows WSL capsule and native-broker tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use quansio_core::Digest;
use quansio_machine::control::{
    ExecutionTarget, Substrate, TargetClass, TargetFence, TargetStatus,
};
use quansio_machine::gateway::{ActionEnvelope, EnvelopeKind};
use quansio_machine::substrates::macos_capsule::{GuestImage, ImageArtifact};
use quansio_machine::substrates::windows::{
    refuses_arbitrary_host_command, windows_guest_service, wsl_argv, WindowsCapsuleController,
    WindowsError, WslBridge, WslLaunch, WSL_EXE, WSL_VERBS,
};

struct RecordingWsl {
    calls: Arc<Mutex<Vec<String>>>,
}

impl WslBridge for RecordingWsl {
    fn import(&mut self, launch: &WslLaunch) -> Result<(), String> {
        self.calls
            .lock()
            .expect("lock")
            .push(format!("import:{}", launch.distribution));
        Ok(())
    }
    fn terminate(&mut self, distribution: &str) -> Result<(), String> {
        self.calls
            .lock()
            .expect("lock")
            .push(format!("terminate:{distribution}"));
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), String> {
        self.calls.lock().expect("lock").push("shutdown".into());
        Ok(())
    }
    fn export(&mut self, distribution: &str, dest: &Path) -> Result<(), String> {
        self.calls
            .lock()
            .expect("lock")
            .push(format!("export:{distribution}:{}", dest.display()));
        Ok(())
    }
}

fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quansio-exec-004-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write");
    path
}

fn launch(generation: i64) -> WslLaunch {
    let bytes = b"verified wsl rootfs";
    let root = scratch("rootfs.tar", bytes);
    WslLaunch {
        target_id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        generation,
        image: GuestImage {
            release_digest: "sha256:release".to_string(),
            architecture: "x86_64".to_string(),
            kernel: ImageArtifact {
                path: scratch("kernel", bytes),
                sha256: Digest::of(bytes).to_string(),
            },
            initrd: None,
            root_disk: ImageArtifact {
                path: root.clone(),
                sha256: Digest::of(bytes).to_string(),
            },
        },
        distribution: "quansio-tgt01".to_string(),
        rootfs_path: root,
        checkpoint_path: scratch("checkpoint.tar", b""),
    }
}

fn target(generation: i64) -> ExecutionTarget {
    ExecutionTarget {
        id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        class: TargetClass::PersistentWorkspaceComputer,
        substrate: Substrate::LocalCapsuleWindows,
        status: TargetStatus::Ready,
        generation,
        lease_id: Some("lse_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string()),
        image_digest: Some("sha256:release".to_string()),
        last_heartbeat_at: None,
        desired_state: Some("READY".to_string()),
        observed_state: None,
    }
}

#[test]
fn wsl_verbs_are_allowlisted_and_exec_is_absent() {
    assert!(WSL_VERBS.contains(&"--import"));
    assert!(!WSL_VERBS.contains(&"--exec"));
    let launch = launch(1);
    assert_eq!(wsl_argv("--import", &launch, &[]).unwrap()[0], WSL_EXE);
    assert!(matches!(
        wsl_argv("--exec", &launch, &["bash"]),
        Err(WindowsError::CommandRefused(_))
    ));
    assert!(refuses_arbitrary_host_command("cmd.exe /c calc"));
    assert!(refuses_arbitrary_host_command("wsl.exe --exec bash -c rm"));
}

#[test]
fn stale_generation_is_refused_before_wsl() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut controller = WindowsCapsuleController::new(RecordingWsl {
        calls: calls.clone(),
    });
    let error = controller
        .prepare(&target(8), &TargetFence { generation: 7 }, launch(8))
        .expect_err("stale");
    assert!(matches!(error, WindowsError::StaleGeneration { .. }));
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn prepare_stop_checkpoint_are_fenced_and_repeatable() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut controller = WindowsCapsuleController::new(RecordingWsl {
        calls: calls.clone(),
    });
    let target = target(3);
    let fence = TargetFence { generation: 3 };
    let prepared = launch(3);
    controller
        .prepare(&target, &fence, prepared.clone())
        .expect("prepare");
    controller.checkpoint(&target, &fence).expect("checkpoint");
    controller.stop(&target, &fence).expect("stop");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded[0], "import:quansio-tgt01");
    assert!(recorded[1].starts_with("export:quansio-tgt01:"));
    assert_eq!(recorded[2], "terminate:quansio-tgt01");
}

#[test]
fn wsl_guest_channel_is_the_typed_qworkerd_vocabulary() {
    let envelope = |tool: &str| ActionEnvelope {
        kind: EnvelopeKind::Dispatch,
        effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        dispatch_token: "t".to_string(),
        target_id: target(1).id,
        lease_id: "lse_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        generation: 1,
        run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
        tool: tool.to_string(),
        tool_version: 1,
        args_json: "{}".to_string(),
        args_digest: Digest::of_canonical_json("{}").to_string(),
        max_output_bytes: 1024,
        deadline_at: "2026-09-15T00:00:00Z".to_string(),
    };
    windows_guest_service(&envelope("fs.read")).expect("file");
    assert!(windows_guest_service(&envelope("connector.github.write")).is_err());
    assert!(windows_guest_service(&envelope("browser.session.import")).is_err());
}
