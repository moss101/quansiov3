# EXEC-010 reconciliation

## Canonical owner

`crates/machine/src/computer_use/` owns native-computer authorization and control state. The narrow
platform bridges remain in `native/macos/` and `native/windows/`. The existing Effect Ledger,
Capability Projection, Tool Registry, machine gateway, and runtime `ProtocolState` remain their sole
authorities; this task adds no parallel effect, policy, runtime, or machine subsystem.

## Existing coverage

Coverage is `PARTIAL`. Uncommitted work found at claim time already supplied a Rust tier model,
known-application refusal, an in-memory takeover fence and tests, plus a macOS AX/CGEvent draft and
read-oriented Swift tests. It is preserved as reconciliation input. Existing PASS work supplies the
ToolCall/effect path (`RUN-011`/`RUN-007`), capability and approval enforcement (`RUN-005`/`RUN-006`),
durable runtime takeover waits (`CORE-006`), ExecutionTarget/lease fencing (`EXEC-001`/`EXEC-002`),
and durable managed-browser takeover (`EXEC-009`).

The remaining gaps are material: process-local control does not survive restart; no native bridge is
wired behind a typed Rust port; Windows is only a crate marker; the Tool Registry exposes only
`computer.click` and `computer.type`; app identity is wildcarded for click; clipboard/system-key/read
tools are absent from the canonical catalog; generated command parameter schemas are placeholders;
and no native real-boundary evidence is recorded.

## Persistent state and recovery

Native control is per `ExecutionTarget`. A forward migration will add one tenant-scoped
`computer_controls` row per controlled target, recording holder, monotonic control generation,
pending takeover and the exact in-flight ToolCall. Row locks serialize action begin/finish,
takeover, and handback. A restart reloads that row: user ownership remains fenced and an abandoned
in-flight action remains blocked until its effect is reconciled. This is control/recovery state, not
memory. Browser takeover continues to use the existing `browser_sessions` row and runtime
`ProtocolState` wait.

## Contracts and events

`DOMAIN.md` §7.5 needs additive canonical names for `computer.read`, `computer.clipboard`, and
`computer.system_key`; §8 needs the minimum `ComputerControl` durable shape and transitions. Source
schemas/catalogs are regenerated after the domain update. `RequestTakeover`, `Handback`, and
`GrantComputerTier` receive concrete target/session, generation, app-resource and tier fields in the
source command schema when that source supports task-owned parameter materialization. Existing
`tool.*`, `effect.*`, `run.waiting/resumed`, and browser control events are reused; no new event family
is necessary.

## Effects, capabilities, and approvals

Accessibility-tree/screenshot/clipboard reads are `read.internal` tier 0. Click, type, clipboard
write, and system-key input are `computer.input.privileged` tier 3. Every action derives an exact
`app` resource from the resolved foreground identity and must be present in the current
CapabilityProjection; tier 3 follows the existing policy and exact ApprovalReceipt path. A missing,
empty, unknown, or ambiguous foreground identity fails closed before dispatch. `GrantComputerTier`
only narrows what the projection already permits and cannot itself execute input.

## Native boundaries

macOS uses the real `NSWorkspace`/AX/CGEvent/pasteboard/screen-capture APIs. Windows uses UI
Automation and fixed allowlisted input operations; it exposes no arbitrary command execution.
Boundary opt-ins are `QUANSIO_TEST_MACOS_COMPUTER_USE=1` and
`QUANSIO_TEST_WINDOWS_COMPUTER_USE=1`. Missing platform, Accessibility/UIA permission, interactive
desktop, or screen-recording permission must print an explicit `BLOCKED_EXTERNAL` marker. Unit
substitutes may test Rust policy/serialization but do not count as release evidence.

## Migration and rollback

The migration is additive. Rollback stops new native input, drains/reconciles any active ToolCall,
then removes the control rows/table; it never rewinds EffectRecords or browser/runtime state. No user
artifact or workspace data is rewritten.

## Implementation checklist

- [ ] Add the canonical `ComputerControl` shape/tool names and regenerate contracts/catalogs.
- [ ] Replace the process-local-only fence with a durable, tenant-scoped control store and recovery tests.
- [ ] Add a typed native bridge port and enforce exact app identity, computer tier and fence generation.
- [ ] Materialize read/click/type/clipboard/system-key declarations and ToolCall/effect-path tests.
- [ ] Complete macOS AX/screenshot/input behavior with safe, explicitly gated real-boundary tests.
- [ ] Complete the allowlisted Windows UIA/input broker and its platform-gated tests.
- [ ] Exercise negative, concurrent takeover, crash/restart and migration paths.
- [ ] Run full Rust/Swift/static/validator suites, record evidence, update progress and merge to `main`.
