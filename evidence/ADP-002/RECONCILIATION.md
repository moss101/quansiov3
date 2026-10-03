# ADP-002 reconciliation

Recorded before implementation (AGENTS.md task loop, RECONCILE) and updated at the end with what was and was not
done. Coverage: `GENUINE_GAP`. EXEC-010 built the native bridge's policy (tiers, exact application identity, the
takeover fence); none of the following existed: a result that says what an input action did, a way to end a sequence
of actions on a terminal condition, a measurement of a "background" claim, a pinned identity for the helper that
holds the OS permissions, or a typed permission status with a guide that parks a run instead of inviting a retry.

## Canonical owner

`Execution` (task `owner`); paths from the task: `crates/machine/src/computer_use/`, `native/macos/`,
`apps/desktop/src/main/`. Everything new lives there, plus the outside edits an authority document forces:

- `config/computer-use.yaml` and `schemas/json/computer-use-config.schema.json` — D-018: the helper pin, the sequence
  limits and the Settings panes are configuration, validated like the other `config/` files.
- `config/tools.yaml` and `crates/tools` — the task asks for the receipt "in the typed tool result in the Tool
  Registry schema"; `crates/tools/src/receipt.rs` is the canonical schema, the registry check and the result check.
- `crates/server/src/api/error.rs` — `ApiErrorCode` is a closed enum checked against the generated
  `schemas/catalog/errors.yaml`; DOMAIN.md §15 gains the `Computer` family, so the enum gains it too (no handler uses
  it yet: see "Not in this task").
- `apps/desktop/src/{live,renderer,preload,i18n}` and `apps/desktop/src/index.ts` — the guide surface and its two
  channels (`computer.permissions`, `computer.openSettings`) cannot live in `src/main/` alone.
- `native/windows/src/computer_use.rs` — `WindowsComputerBridge` implements `NativeComputerBridge`, whose two new
  required methods (`permission_status`, `foreground_control`) it must answer; it reports what Windows actually has
  (no per-application permission gate; no input-lapse detection yet) and says so in comments.
- `tests/architecture/{composition_scan.py,test_qa002_composition.py}` and `tests/contract/test_contracts.py` — see
  "Composition"; the error catalog now has 61 codes and `computer-use.yaml` is validated against its schema.
- Generated: `schemas/catalog/{errors,glossary}.yaml`, `schemas/openapi/public-api-v1.yaml`, `sdk/*/generated/*`.

## Existing code and its coverage

| Code | Classification | What it did | What changes |
|---|---|---|---|
| `computer_use/mod.rs` (`ComputerTier`, `ComputerGrant`, `KnownApps`, `ComputerUsePolicy`) | covered, kept | tiers are grants; an unknown foreground app is refused at every tier | untouched |
| `computer_use/store.rs` (`ComputerControlStore`) | covered, kept | durable holder/generation/active-ToolCall fence | untouched; the sequence runner reads it |
| `computer_use/bridge.rs` (`NativeComputerBridge`, `ComputerDispatcher`) | covered, extended | authorize, fence, call the platform, settle | the trait gains `permission_status`, `foreground_control` and `perform` (a default that wraps `execute` in the weakest honest receipt); `execute` returns a `ComputerActionResult` (output + re-derived receipt); a permission gate runs before the slot opens; a platform call no longer blocks a runtime worker |
| `native/macos/.../ComputerUse.swift` | covered, kept | AX tree, input posting, clipboard, screenshots | untouched; new files add receipts, measurement, permission status |
| `config/tools.yaml` `computer.*` | covered, extended | output schemas without a receipt | each output requires the canonical `receipt` (one YAML anchor) |
| Effect Ledger | covered, not touched | `OUTCOME_UNKNOWN` is reconciled, never retried | nothing: the receipt maps onto its existing statuses |

## Persistent state touched

None. No table, column, migration, queue or store (AGENTS.md invariant 12; D-019). The helper's install directory
(`versions/<sha256>/`, `active`, `previous`) is files under the application data directory, not a service or a
database; the receipt and the attention item are values, and the run's park uses the existing `SUSPENDED` state and
`blocked` Notification kind (DOMAIN.md §5.2, §13.3).

## Contracts and events changed

DOMAIN.md: §0 glossary (ActionReceipt, Terminal condition, PermissionStatus, Pinned helper), §7.5 (computer Tool
results carry a receipt), new §8.6.1 A–E, §15 (the `Computer` family and a flags table). No event type and no command
changes. Generated catalogs, OpenAPI and the SDK operation tables are regenerated, not hand-edited.

## External effects and their classes

Unchanged taxonomy. `computer.input.privileged` (tier 3, manual reconciliation) is what every input action already
was. The receipt settles a dispatched action whose effect is not established as `OUTCOME_UNKNOWN`, which is that
class's existing "manual" strategy; nothing is retried.

## Capabilities and approvals

Capability only narrows: the gate adds refusals (a missing permission, a lapsed foreground grant, a spent limit, a
takeover). No grant is widened and no approval is created or consumed here.

## Migrations

None.

## Real boundaries and the environment they need

- `swift test --package-path native/macos` on this Mac. The live computer-use tests need
  `QUANSIO_TEST_MACOS_COMPUTER_USE=1` (else `BLOCKED_EXTERNAL`), no other variable. They write the clipboard and put
  it back and move the pointer one point and put it back; they post no key or click. Accessibility and Screen
  Recording were granted to the terminal host in this run; a tree walk and a screenshot are skipped with a marker
  when they are not.
- Real `/usr/bin/codesign` and the system C compiler for the helper suite (`BLOCKED_EXTERNAL` marker without them).
  Helpers are signed ad hoc: no Developer ID exists for the pin (`helper.status: unpublished`), so the positive
  production-requirement run is the blocker recorded on the task.
- Real PostgreSQL for the sequence/receipt suites (`QUANSIO_TEST_POSTGRES_URL`, private database).
- No credential is read by any test.

## Rollback and recovery

Rollback is forward-fix; no migration to undo. A restart reloads the durable control row (an action that failed after
dispatch stays active; nothing is retried); the helper's `active`/`previous` pointers survive a crash and a failed
activation restores the previous version; orphaned helpers are terminated on start.

## Plan (checklist)

- [x] DOMAIN.md §8.6.1, §15, glossary, §7.5
- [x] `config/computer-use.yaml` + schema + loader with fail-closed validation
- [x] receipt types, normalization, ledger mapping; tool schema + registry check + result check
- [x] sequence runner, terminal conditions, takeover race
- [x] permission status, gate, attention item, guide
- [x] helper pin: verify, install, rollback, launch, orphan sweep (real codesign)
- [x] Swift: receipts, background measurement, permission status, foreground-control monitor
- [x] desktop: status, Settings click, resume, polling
- [x] negative tests, recovery tests, mutation checks, evidence

## Composition

`ComputerSequence` and `ActionFeed` are not constructed by any production code: the runtime tool host that would walk
a model's computer actions does not exist yet (EXEC-010's `ComputerDispatcher` was already uncomposed; QA-002's
`KNOWN_GAPS`). The scan counts any mention outside a type's own file as composition, so `ComputerSequence` naming the
dispatcher would have closed that gap by accident. The obligation was therefore moved to `ComputerSequence` (owner
ADP-002) and `ActionFeed` is recorded as `NOT_COMPOSED`; the real work remaining is the same one gap.

## What is deliberately not in this task

- The runtime half of the park (a `SUSPENDED` transition with `COMPUTER_PERMISSION_REQUIRED` and the `blocked`
  Notification carrying the attention item) and the HTTP mapping of the `COMPUTER_*` codes: there is no tool host to
  call them (see Composition). The machine returns the exact item and the desktop parses exactly it (shared fixtures).
- A helper executable and its control channel: `native/macos` is a library and no daemon launches or talks to a
  helper yet. The pin, install and launch are real against signed test executables.
- Reading the helper's own permission status in the desktop (Electron probes three of the four; Input Monitoring is
  shown as "not checked here"; no computer action needs it).
