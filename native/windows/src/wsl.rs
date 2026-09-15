//! Allowlisted WSL2 launcher (EXEC-004).
//!
//! The argv is built by the Rust machine authority. This module will spawn only `wsl.exe`
//! with an allowlisted verb. `--exec` and `cmd.exe` are refused here even if a caller
//! asks.

use quansio_machine::substrates::windows::{wsl_argv, WslBridge, WslLaunch, WSL_EXE, WSL_VERBS};

/// Native WSL bridge.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsWslBridge;

impl WslBridge for WindowsWslBridge {
    fn import(&mut self, launch: &WslLaunch) -> Result<(), String> {
        spawn(&wsl_argv("--import", launch, &[]).map_err(|error| error.to_string())?)
    }

    fn terminate(&mut self, distribution: &str) -> Result<(), String> {
        if distribution.is_empty() || distribution.starts_with('-') {
            return Err("BLOCKED_EXTERNAL: invalid WSL distribution".to_string());
        }
        spawn(&[
            WSL_EXE.to_string(),
            "--terminate".to_string(),
            distribution.to_string(),
        ])
    }

    fn shutdown(&mut self) -> Result<(), String> {
        spawn(&[WSL_EXE.to_string(), "--shutdown".to_string()])
    }

    fn export(&mut self, distribution: &str, dest: &std::path::Path) -> Result<(), String> {
        spawn(&[
            WSL_EXE.to_string(),
            "--export".to_string(),
            distribution.to_string(),
            dest.display().to_string(),
        ])
    }
}

fn spawn(argv: &[String]) -> Result<(), String> {
    let exe = argv.first().map(String::as_str).unwrap_or("");
    if exe != WSL_EXE {
        return Err(format!("windows capsule refused command {exe}"));
    }
    let verb = argv.get(1).map(String::as_str).unwrap_or("");
    if !WSL_VERBS.contains(&verb) || verb == "--exec" {
        return Err(format!("windows capsule refused command {verb}"));
    }
    #[cfg(not(windows))]
    {
        let _ = argv;
        Err(
            "BLOCKED_EXTERNAL: QUANSIO_TEST_WINDOWS_CAPSULE=1 requires Windows 11 with WSL2"
                .to_string(),
        )
    }
    #[cfg(windows)]
    {
        if std::env::var("QUANSIO_TEST_WINDOWS_CAPSULE").as_deref() != Ok("1") {
            return Err("BLOCKED_EXTERNAL: QUANSIO_TEST_WINDOWS_CAPSULE=1 is required".to_string());
        }
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err("wsl.exe refused the allowlisted verb".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_and_cmd_are_refused_before_spawn() {
        let error =
            spawn(&["cmd.exe".to_string(), "/c".to_string(), "calc".to_string()]).expect_err("cmd");
        assert!(error.contains("refused") || error.contains("BLOCKED_EXTERNAL"));
        let error = spawn(&[
            WSL_EXE.to_string(),
            "--exec".to_string(),
            "bash".to_string(),
        ])
        .expect_err("exec");
        assert!(error.contains("refused"), "{error}");
    }

    #[test]
    fn missing_windows_host_is_blocked_external() {
        #[cfg(not(windows))]
        {
            let error = WindowsWslBridge.shutdown().expect_err("not windows");
            assert!(error.contains("BLOCKED_EXTERNAL"), "{error}");
        }
    }
}
