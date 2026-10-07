# ADP-004 evidence notes

The desktop shell is built in four places: the server's maintenance status (`crates/server/src/api/maintenance.rs`, with
the counts in the three modules that own the rows), the shell's main-process modules (`apps/desktop/src/main/`: `windows`,
`ipc-trust`, `shortcuts`, `keep-awake`, `connectivity`, `quit`, `update-gate`, `activity`, `menu`, `tray`, `shell-host`,
with `app.ts` as the Electron adapter layer), the renderer's four window kinds and settings
(`apps/desktop/src/renderer/`), and the policy values (`config/desktop-shell.yaml`). Authority text is DOMAIN.md §13.6
(A-G), §0 (three terms) and §14 (`GET /v1/maintenance`). `RECONCILIATION.md` records what existed and what was touched.

Status is **PASS** (`real_boundary: false`; both dependencies, APP-003 and APP-010, are PASS).

## What proves each acceptance statement

| Acceptance statement | Proof |
|---|---|
| No window can invoke IPC from an untrusted URL | `tests/ipc-trust.test.ts`: through `registerGuardedHandlers` (the function `app.ts` installs), every allowlisted channel from twelve kinds of untrusted frame (remote, local web server, `file:`, `data:`, `javascript:`, `about:blank`, another `app:` host, a lookalike host, the wrong scheme, a gone frame, a subframe of the trusted page, an unparseable URL), and from every window type, is a logged no-op: no handler runs and nothing is answered. A trusted URL in a window the shell did not create is the same. `tests/ipc-sources.test.ts` scans the source: no `ipcMain.on/once`, no `sendSync`, no raw `ipcRenderer.send`, one `ipcMain.handle` path, no credential store named in the shell's modules, no CORS stripping or `webSecurity: false`. Real Electron (`tests/e2e/shell.e2e.spec.ts`): unmanaged windows with the real preload on a `data:` page, a local HTTP page and `app://desktop/index.html` answer `undefined` to six calls each (including `PostMessage`, `updateWorkspace`, `setShortcut`), the log names each refusal, the server's thread count does not move, and the signed-in main window still gets real answers. Per-window authority: the exhaustive channel × window-type matrix, a quick-entry window refused settings/commands/approvals, an indicator refused threads, a pop-out refused another thread's messages, posts, questions, runs (real Electron and unit). Every window type is built on the sandboxed baseline (`specMeetsBaseline` for each type and platform; `getLastWebPreferences` of each live window). |
| Quit and update never interrupt an active run without confirmation | `tests/shell-gates.test.ts`: `QuitGate` asks when work is running, asks when the status is unknown or the read throws (never idle), quits silently only on an idle reading, shares one question between overlapping requests; `UpdateGate` asks before restarting over work (default button "Later", "Restart anyway" only by choice), never installs on an unreadable status, installs when idle only on an idle reading, polls with a self-rescheduling timer, revokes the quit permission when an install does not start; a failed install turns install-on-quit off (`UpdateHooks`); the dialogs say the work is *paused* and continues from the server, and the default and cancel button interrupt nothing. `tests/shell-host.test.ts`: the same through the channels. Real Electron: with a Turn seen on the stream, `app.quit()` raises the question ("1 turn is running.", "Keep working" leaves the app and its windows running); with the server stopped it asks ("could not check"); with the server back and nothing running it exits without a dialog. `update.install` over a running Turn asks, "Later" calls the installer zero times, "Restart anyway" once, idle needs no question. The server half: `crates/server/tests/maintenance.rs` (4, real PostgreSQL). |
| Shortcut registration failure rolls back | `tests/shortcuts.test.ts`: a system that declines a global binding leaves the previous binding registered and stored (nothing written to disk), a system that throws does the same, one that also cannot restore the old binding says so and keeps the stored binding, the typed results (`invalid`, `conflict` naming what holds it, `registration_failed`); only stored bindings (and the configured toggle default) are ever registered, a cleared binding is never re-registered as the default. Real Electron: `globalShortcut` registers a binding, another holder takes a key, the request for it answers `registration_failed` and the first binding is still registered with the real OS API. |

Also covered: recording pauses every global shortcut and the menu (the menu is one inert entry with no role: a role carries its
own default accelerator, found by the smoke) and ends on blur/navigation/crash/close; the 10 s offline grace absorbs a flap and
the signals call the bridge's one reconnect (unit, and `DesktopBridge.resumeStream` resumes from the durable cursor in
`shell-modules.test.ts`); keep-awake only on AC power while a Turn runs (the full truth table plus the host); the pending queue
(cap 64, oldest dropped, per window, cleared on destroy, pulled in order); the activity order and Turn-state rule; the pop-out's
validated launch data, drop-point placement, one window per thread; quick entry starting a thread through the real
`PostMessage` (real Electron: a new thread appears on the server); the indicator listing and dismissing (real Electron).

## Counts

`desktop-tests.log`: 46 files, 379 passed, 4 skipped (ADP-003's recorder binary is not built here; MinIO-gated cases print
`BLOCKED_EXTERNAL`); `tsc` and `eslint` clean over `apps/desktop` and `tests/e2e`. `shell-e2e.log`: the real-Electron smoke,
8 passed. `workspace-tests.log`: `cargo test --workspace --no-fail-fast` 1071 passed, 13 failed: the same 13 calls to the
artifact/evidence object store at `127.0.0.1:59010` that ADP-002 and ADP-003 recorded (MinIO is not running); this task adds the
4 `maintenance` tests. `repo-tests.log`: 241 passed (`tests/architecture`, `tests/contract`, `tests/ci`; the Rust contract
generator cannot run from a worktree under `.claude/worktrees`, so `test_generated_bindings_are_current` is run in the finalize
dry-run clone). `lint.log`: `cargo fmt --check` and `clippy -D warnings` clean. `static-gates.log`: validator, arch, workspace,
inventory, authority, legacy map, supply chain, contract regeneration (catalog, openapi, ts, sdk) all clean.
`tests/work.e2e.test.ts` is excluded from `desktop-tests.log` as in ADP-003's run: it asserts exact workspace revision numbers
and fails differently on different runs of the same code (observed 2 of 4 runs here, with this task's code not on its path).

## Mutation checks

`mutation-checks.log` (script `mutation-checks.py`, run from a committed tree): 58 mutants of the guards across the server's
counts, keep-awake, connectivity, quit, update, shortcut, IPC-trust, window, activity, menu, tray, bridge, config, grammar and
schema code. Every mutant COMPILES (`tsc` for the TypeScript ones, `cargo test --no-run` for the Rust ones), the unmutated suite
passes first (CONTROL-PASSED for both toolchains), and a named test then fails: 58 killed, 0 survived, 0 no-verdict. The first
full run found one survivor (the status's `idle` flag ignoring in-flight actions: no test had actions in flight with no Turn);
`maintenance.rs` now has that case, and five mutants that did not compile were rewritten to compile; the log is the final run.
`workspace-tests.log` predates that one added assertion (same four tests).

## Not exercised live (stated, not claimed)

Unit-tested behind an adapter, and in the real-Electron run only as far as Electron's own API answers:

- **The tray.** macOS shows none, so the Windows/Linux tray (`new Tray`, the Open and Quit entries, the generated PNG icon as an
  image Electron accepts) was not created on a real Windows or Linux desktop. What is tested: which platforms want one, the
  entries, the icon as a valid PNG, and the close behaviour with and without a tray (`shell-menu.test.ts`).
- **A global shortcut fired from another application.** The OS registration and its refusal are real; the key press that triggers
  the handler is not (needs Accessibility to synthesize and a foreground app). The handler is called directly (`onGlobalShortcut`).
- **Power source changes and resume from sleep.** `powerMonitor` events are not raised in a headless run; `onPowerSource` and
  `onResume` are called directly. The blocker itself (`powerSaveBlocker.start/stop`) is the real API but its effect on the machine
  was not observed.
- **A second or removed display.** Display geometry is a fake with two displays (`FakeDisplays`); the clamp, per-display
  persistence and re-placement are unit-tested; no monitor was attached or removed.
- **Mouse click-through.** `setIgnoreMouseEvents(true, {forward: true})` is applied at creation and toggled by the page's pointer
  events (the call is asserted on the window handle); Electron has no getter and no pointer was moved over the real window.
- **Visibility on all Spaces and over full-screen apps, hidden from Mission Control.** `isVisibleOnAllWorkspaces()` is asserted
  true on the real indicator window; Mission Control and full-screen behaviour were not looked at.
- **Recording ending on a real blur.** The `blur`/navigation/crash events are delivered to the registry in unit tests; the
  smoke did not move OS focus.
- **Windows and Linux.** Everything platform-conditional (accelerator spelling, the menu without an app menu, minimize-when-no-tray)
  is unit-tested for `win32` and `linux`; only macOS ran.
- **Drag-out to open a pop-out at the drop point.** The channel and the placement are tested; the HTML5 drag was not performed.
- **A signed update.** `autoUpdater.quitAndInstall` is replaced in the smoke; whether the platform installer works is REL-002/003.

## Deferred

- **Schema validation of the older channels.** Every shell channel validates with `schema.ts`; the channels APP-003..ADP-003
  built keep their `as…` guards (their tests are unchanged). Moving them is a mechanical change with no new behaviour.
- **Start-up hydration of the activity list.** The list is built from the stream, and the durable cursor replays what was missed
  while the app was closed. An approval or question that was already open and was raised before the stored cursor does not
  appear in the list until something about it happens again. `ActivityTracker.hydrateApprovals` exists and is unit-tested, but
  nothing calls it: reading the server's pending approvals at start-up is not wired.
- **`open-run` navigation** finds the thread from messages the sidebar has read; for a run it has not seen it opens the
  conversation page.
- **A pop-out for a thread opened from a notification** (the click opens the thread in the main window).

## Traps met

- The harness refuses compound shell commands that `cd ..`, use `git -C <computed>` or assemble python in a heredoc: write scripts
  to the scratchpad and run them.
- `pnpm install --offline` fails in a worktree (a tarball is missing from the store); symlink the primary's `node_modules`
  (root, `apps/desktop`, `apps/web`, `sdk/typescript`). The links show as untracked: `git add` explicit paths, never `apps/desktop`.
- A menu `role` item carries a default accelerator (Command+C); "no accelerators while recording" needs a menu with no roles.
- `tests/session.e2e.test.ts` expects one `role="status"` in the Settings region: new Settings content uses `aria-live`.
- `@typescript-eslint/no-unnecessary-condition` narrows a `let` flag after an `await` (`if (!cancelled)` is "always true"): read it
  through an accessor.
- The e2e harness needs the web bundle (`apps/web/dist/web`) even for desktop-only tests: the server refuses to start without it.
- A real-Electron test needs `QUANSIO_DESKTOP_SIGNED=1` to wire the updater and `QUANSIO_DESKTOP_TEST_HOOKS=1` to reach the
  shell from `app.evaluate` (the hooks object is exposed on the main process's `globalThis` only under that variable).
- A private PostgreSQL for the run: `docker run --tmpfs ... pgvector/pgvector:pg17 -c max_locks_per_transaction=1024` with
  `infra/compose/initdb` mounted.
