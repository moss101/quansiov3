# EXEC-004 reconciliation

## Canonical owner and existing coverage

`native/windows/` owns the allowlisted UI Automation/input broker (already landed by EXEC-010)
and the WSL2 launch surface. `crates/machine/src/substrates/windows/` owns the Rust adapter
that maps WSL lifecycle and the native broker onto ExecutionTarget, Lease and generation.
Coverage is `GENUINE_GAP` for the WSL capsule; the native broker's allowlist already exists
and must be reused, not forked.

## Persistent state, contracts, events

No new authoritative table. `execution_targets` already stores `local_capsule_windows` and
`windows_native` substrates. Checkpoints stay in `checkpoints`. The adapter drives EXEC-001
APIs and emits existing `target.*` / `lease.*` / `checkpoint.*` events.

## Effects, capabilities, isolation

Guest workloads run in WSL2 Linux and reach qworkerd only through the typed EXEC-002 envelope.
The native broker never interpolates caller text into PowerShell or `cmd.exe`. WSL is invoked
only as `wsl.exe` with an allowlisted verb. Host filesystem bind-mounts and arbitrary
`--exec` are refused. Cookie import is not a WSL path.

## Image, migration, recovery

The same unpublished V8.1 Linux guest catalog as EXEC-003 is the WSL rootfs source. No digest
or URL is invented. Reset discards the writable overlay/distribution; restore is a new
generation. Stale leases and generations are refused before native execution.

## Real boundary

Requires a Windows 11 host with WSL2, `wsl.exe`, and `QUANSIO_TEST_WINDOWS_CAPSULE=1`.
Missing host, WSL, or signed rootfs is `BLOCKED_EXTERNAL`. No fake WSL counts.

## Implementation checklist

- [x] Reuse the EXEC-010 allowlisted native broker; prove no arbitrary host command escape.
- [x] Add the Rust WSL capsule adapter over ExecutionTarget/Lease/generation.
- [x] Allowlist `wsl.exe` verbs; refuse `--exec` and caller-interpolated commands.
- [x] Lease/generation fence before native execution.
- [x] Real Windows smoke test or explicit `BLOCKED_EXTERNAL`.
