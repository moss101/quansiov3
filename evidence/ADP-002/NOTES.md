# ADP-002 evidence notes

Native computer use is hardened in four places: the runtime's rules (`crates/machine/src/computer_use/`: `receipt`,
`sequence`, `permission`, `helper`, `config`, and the extended `bridge`), the Tool Registry schema
(`config/tools.yaml`, `crates/tools/src/receipt.rs`), the macOS bridge (`native/macos`: receipts, background
measurement, permission status, a foreground-control monitor), and the desktop guide (`apps/desktop/src/main/permissions.ts`,
`src/live/permissions.ts`, `src/renderer/ComputerPermissions.tsx`). Authority text is DOMAIN.md §8.6.1 (A-E), §7.5, §15.
`RECONCILIATION.md` records what existed, what was touched and what was not.

## What proves each acceptance statement

| Acceptance statement | Proof |
|---|---|
| An unverifiable action is recorded as unknown, never as verified | `crates/machine/tests/computer_receipts.rs` (16, real PostgreSQL for the dispatcher half): `effect_verified` is written as `null` and never omitted, an absent or non-boolean verdict is malformed (not unknown), every contradictory receipt is refused, a bridge with no verification gets `unverified`/`null` from the dispatcher's default `perform`, the ledger maps `unverified`/`not_applied`/`invariant_violated` to `OUTCOME_UNKNOWN` and only `applied`/`observed` to success, a background `applied` with a broken or missing measurement is re-derived to `false`/`null`, an incoherent receipt is a bridge fault (input: the ToolCall stays active; read: a failed read). `crates/tools/tests/computer_receipt.rs` (4): every `computer.*` output requires the one canonical receipt, a loosened schema is refused at load, and a result with an incoherent receipt fails output validation; the machine and the registry accept and refuse the same receipts (one parity test). `native/macos` `ReceiptTests`: the same fixtures encode and decode identically on both sides. |
| A revoked permission or takeover stops further actions in the same sequence | `computer_sequence.rs` (14, real PostgreSQL): for stop, takeover, revoked permission and each limit the count of times the model was asked for another action and the count of actions that reached the platform are asserted (2 and 2, never 3). The takeover race blocks action 2 *inside* the platform call, requests the takeover while it is in flight, shows `complete_takeover` refused until the action settles, and shows no input afterwards; a takeover landing between the control check and the action's own begin is still `COMPUTER_TAKEOVER_ACTIVE`; a sequence from an older generation is not even asked for actions. |
| A helper with the wrong identity is refused | `computer_helper.rs` (11) against real `/usr/bin/codesign` on real signed executables: a validly signed helper with another identifier, an unsigned one, one without the hardened runtime, one altered after signing, and an ad-hoc one against the production requirement (an Apple anchor and the configured team) are each refused with a typed error carrying `COMPUTER_HELPER_IDENTITY_REFUSED`. After install, a file that changes is refused at launch; re-signing the altered file makes its signature valid again and it is still refused, because the version is named by the fingerprint of what was verified. |
| Missing permission parks the run on an attention item, not on a model retry | `computer_sequence.rs`: a missing permission returns the attention item with no dispatch slot taken and nothing sent to the bridge (0 actions), a model that keeps proposing is asked once, three repeats get the same answer and still reach nothing, and the same action runs after the person grants it. The item is `blocked` / `ResumeRun` / a Privacy pane; a lapsed foreground-control grant asks the person to pause input with no pane and no permission. `apps/desktop/tests/permissions.test.ts` (29): the page parses exactly the items the runtime writes (shared fixtures), refuses an item that names any other resume command, opens only the pane of the permission the person clicked (a hostile payload table never reaches the opener), and enables Continue only when the snapshot says everything the run needs is granted. |

Takeover race, recovery, negative and real-boundary tests are named in the table above; the permission status/onboarding
test is `computer_permission.rs` (8) plus `permissions.test.ts`.

## Mutation checks

`mutation-checks.log` (script: `mutation-checks.py`): 50 mutants of the guards across Rust, Swift and the desktop. A mutant
is KILLED only if it compiled (`COMPILES`), the unmutated suite passed first (`CONTROL-PASSED`) and a named test then
failed. The first run left three SURVIVED (a refused receipt that dispatched nothing but claimed a verdict; a sequence
from an older generation, which the dispatcher's own check refused anyway; the registry's copy of the first rule): each
was a hole in the tests, not an equivalent mutant, and was closed by a test that counts what the model was asked or
names the missing case. The final run is in the log.

## Traps met, and what was decided

- **sqlx rolls an abandoned transaction back lazily.** A refused `complete_takeover` returned with its transaction open,
  and the row lock it held stalled the in-flight action that was about to finish: the race test waited for itself. Use
  the connection once (any query) after a refused store call before releasing the action.
- **A synchronous platform call must not hold a runtime worker.** A bridge call blocked a worker whose queue held the
  connection tasks of the database fencing it, so the takeover request waited forever. `ComputerDispatcher` now runs the
  call under `block_in_place` on a multi-thread runtime (and in place on a current-thread one).
- **Flipping the last byte of a signed Mach-O proves nothing.** It sits in the signature's padding and `codesign` still
  verifies. The tamper tests change a byte of the program's own text, which the page hashes cover.
- **`codesign -R` takes literal requirement text as a separate `=<text>` argument**; a failed requirement exits 3, an
  invalid signature 1, "not signed at all" prints on stderr.
- **A YAML `null` inside `type: [boolean, null]` is the null value, not the word.** The schema vocabulary needs the quoted
  `"null"`; the receipt is defined once (a YAML anchor) and the registry compares each computer Tool's copy to the
  canonical one in Rust.
- **The composition scan counts any mention as composition.** `ComputerSequence` naming `ComputerDispatcher` closed
  EXEC-010's known gap by accident; the obligation moved to `ComputerSequence` (owner ADP-002), `ActionFeed` is recorded
  as `NOT_COMPOSED` (see RECONCILIATION.md "Composition").
- **`native/windows` also implements `NativeComputerBridge`.** The trait grew `permission_status` and
  `foreground_control`; the Windows bridge answers them with what the platform actually has (no per-application permission
  gate, so granted; no input-lapse detection yet, so held) and says so in comments. It was outside the task's paths and
  would not compile otherwise.
- **Worktree.** `pnpm install --frozen-lockfile --offline` is needed for the desktop gates (no `node_modules` in a new
  worktree); `gen_contracts.py --all/--check` cannot run the Rust generator from under `.claude/worktrees`, so the
  catalog, OpenAPI, TypeScript and SDK steps were run individually (`--check --only catalog,openapi,ts,sdk` is CLEAN). The
  proto files are untouched.
- **`quansio-server --test api` needs MinIO.** `the_public_surface_works_as_the_application_role_under_rls` fails with a
  connection refused to `127.0.0.1:59010` when the dev stack is down; it is not a skip and not related to this change (the
  other 16 pass, including the error-code table).
- **Private database.** A throwaway PostgreSQL (`pgvector/pgvector:pg17` on port 55462) served every database-backed test;
  nothing touched the shared dev stack, and the container was removed afterwards.

## Real-boundary runs on this host

macOS Apple Silicon, Accessibility and Screen Recording granted to the terminal host, no Developer ID available:

- `QUANSIO_TEST_MACOS_COMPUTER_USE=1 swift test --package-path native/macos`: 49 tests. New and live: the permission probes
  against the system's own answers, the foreground-control monitor, a real one-point pointer move seen by the measurement
  (and put back), a real clipboard write receipted, measured and verified by read-back (and put back), and an unauthorized
  foreground refused before anything is written.
- `cargo test -p quansio-machine --test computer_helper`: real `codesign` and a real C compiler.

## Not exercised, and why

- A real click, key or typed text through the system input stream, and a real *physical-input* violation: posting drives the
  person's machine. The measurement logic for physical input is driven with a scripted probe and the live suite proves the
  pointer and frontmost measurements; the input receipts (`receiptedClick`, `receiptedTypeText`) compile and stamp the
  foreground-control monitor but are not run.
- The positive production helper identity (an Apple anchor and a real team id): the configuration is `unpublished`
  (`blocker` in `config/computer-use.yaml`) because no signing identity exists.

## Deferred, and why

- The runtime tool host that walks a sequence, settles each action on the Effect Ledger from `ledger_outcome`, parks the
  run (`SUSPENDED`, `COMPUTER_PERMISSION_REQUIRED`, a `blocked` Notification carrying the attention item) and maps the
  `COMPUTER_*` codes onto HTTP: there is no computer tool host in the server (QA-002 `KNOWN_GAPS`: `ComputerSequence`,
  `ActionFeed`). The shapes are fixed and tested on both sides.
- A helper executable and its control channel, and the desktop reading the helper's own permission status: `native/macos`
  is a library today. Electron probes Accessibility, Screen Recording and Microphone; Input Monitoring is shown as not
  checked (no computer action needs it).
- No `config/flags.yaml` entry: every behaviour here is a guard, and a flag would be an off-switch for a security control.
