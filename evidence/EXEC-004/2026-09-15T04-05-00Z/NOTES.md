# EXEC-004 evidence notes

WSL capsule adapter and allowlisted native broker are implemented. This runner is macOS, so the live Windows 11 / WSL2 / UIA boundary is `BLOCKED_EXTERNAL`.

## acceptance

**Windows path passes shared worker/tool conformance suite.** The WSL guest admits only the typed qworkerd file/terminal/browser vocabulary; connector writes and cookie import are refused. Live conformance on a real WSL distribution cannot run here.

**Native broker exposes no arbitrary host command escape.** `wsl.exe` verbs are allowlisted and `--exec` is absent. The UIA broker uses compiled-in PowerShell with base64 environment values; caller text cannot become a command. `cmd.exe /c` is refused before spawn.

## real boundary

`QUANSIO_TEST_WINDOWS_CAPSULE=1` and `QUANSIO_TEST_WINDOWS_COMPUTER_USE=1` on Windows 11 with WSL2. Unset here; tests print `BLOCKED_EXTERNAL`.
