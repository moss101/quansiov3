# ADP-004 reconciliation

Recorded before implementation (AGENTS.md task loop, RECONCILE) and updated at the end with what was and was not done.
Coverage: `PARTIAL`. APP-003 gave the desktop one sandboxed window, the preload allowlist, deep links, update hooks
and a durable session; APP-010 gave native notifications. There was no application menu, tray, shortcut,
keep-awake, quit confirmation, connectivity signal, secondary window, per-sender IPC check or update deferral.

## Canonical owner

`Product` (task `owner`); paths from the task: `apps/desktop/src/main/`, `apps/desktop/src/renderer/`,
`crates/server/src/api/`. Everything new lives there, plus the outside edits an authority document, a closed
vocabulary or an owner's count forces:

- `crates/server/src/api/maintenance.rs` (new) + one route in `api/mod.rs`: `GET /v1/maintenance`.
- The counts it reads are added to the modules that own the rows, not queried from the API:
  `runtime/state_machine/store.rs` (`count_active_turns`), `effects/ledger.rs` (`count_in_flight`) and
  `crates/machine/src/control/terminal.rs` (`TerminalStore::count_open`).
- `DOMAIN.md` §0 (three terms), §13.6 (new, normative A-G), §14 (one read projection).
- `config/desktop-shell.yaml` + `schemas/json/desktop-shell-config.schema.json` (D-018: every timing, bound and the
  default global shortcut is configuration; the bundle step copies the file beside `main.cjs`).
- Generated, not hand-edited: `schemas/catalog/*`, `schemas/openapi/public-api-v1.yaml`, `sdk/*/generated/*`.
- `apps/desktop/src/preload/bridge.ts` + `src/main/ipc.ts`: new `shell.*` channels (the allowlist is the one door).
- `tests/contract/test_contracts.py` validates the new config against its schema.

## Existing code and its coverage

| Code | Classification | What it did | What changes |
|---|---|---|---|
| `main/security.ts` (`RENDERER_WEB_PREFERENCES`, `navigationAllowed`, `windowOpenDecision`, CSP) | covered, kept | the one window's baseline and the trusted-URL check | reused for every window type; `navigationAllowed` becomes the sender check on every IPC call |
| `main/app.ts` (single `BrowserWindow`, handlers) | partial, extended | composes the shell against Electron | split: a `WindowManager` creates all window types; `app.ts` wires adapters (menu, tray, shortcuts, power, dialog) |
| `main/ipc.ts` (`registerBridgeHandlers`, `DesktopBridge`) | covered, extended | one handler per allowlisted channel, no sender check | handlers are wrapped by `authorizeIpc` (sender URL, managed window, window-type table, own-thread rule); ad-hoc `as…` guards are kept for old channels, every new channel validates through `main/schema.ts` |
| `client/stream.ts` (`StreamClient`, backoff 250 ms-8 s, cursor replay) | covered, reused | the one reconnect path | `DesktopBridge.reconnect()` is what connectivity signals call; no second path |
| `main/updates.ts` (`UpdateHooks`, unsigned = `unavailable`) | covered, extended | status, check, install | `install()` now goes through an `UpdateGate` that reads the maintenance status; unsigned behaviour untouched |
| `main/deeplinks.ts`, `automation/notifications.ts` | covered, kept | parse links; decide a native notice | their results are queued per window until it subscribes; a notice click opens the thread |
| `renderer/Palette.tsx`, `App.tsx`, `Settings.tsx` | covered, extended | palette, shell, settings | menu actions, connectivity reports, the shortcuts and keep-awake settings, pop-out entry points, the deep-link intent now navigates |
| `main/session-file.ts` | covered, pattern reused | atomic JSON under userData | `main/preferences.ts` follows it for the person's own shell choices (not authority, never read by the server) |

## Persistent state touched

Server: none written. The projection is derived on every read from `turns`, `runs`, `effect_records`,
`terminal_sessions` and `execution_targets` through their owners (no new table, no cache).
Desktop: one file, `shell-preferences.json` under the app's userData (shortcut bindings, keep-awake choice,
indicator position per display, install-when-idle choice), 0600, atomic write. It is the person's own client
preference, like `session.json`; the server never reads it and nothing in it is a credential.

## Contracts and events changed

`GET /v1/maintenance` (DOMAIN.md §13.6 A, §14). No command, no event, no error code. Window, shortcut and activity
rules are §13.6 B-G (client behaviour, normative for the desktop).

## External effects and capabilities

The shell performs no Effect Ledger effect: it opens windows, registers OS shortcuts the person configured, asks the
OS for a power-save blocker and a dialog, and calls the same public commands the main window already calls
(`PostMessage`, `CreateThread`) from the quick-entry window. No capability is widened: a window type only narrows
what the existing allowlist offers.

## Real boundaries

`real_boundary=false`. The shell is exercised against the real Electron binary on this Mac (window creation, IPC
trust refusal, quit and update gating) and, for the maintenance status, a real `quansio-server` on a private
PostgreSQL. OS surfaces that cannot be asserted headlessly are unit-tested behind an adapter and listed in NOTES as not
exercised live (tray on macOS, a global shortcut firing from another application, the power-source change, a real
monitor change, mouse click-through).

## Migrations and rollback

No migration. Rollback: revert the task branch; `shell-preferences.json` is ignored by the previous build.

## Plan (checklist, all done unless listed under Deferred in NOTES.md)

- [x] DOMAIN.md §0, §13.6, §14; `config/desktop-shell.yaml` + schema + contract test; regenerated catalogs/OpenAPI/SDK.
- [x] Server: `GET /v1/maintenance` over three owners' counts; real-PostgreSQL tests (what is counted, a flip back to idle,
      workspace narrowing, tenant isolation, auth and bounds).
- [x] Desktop main: `schema`, `shell-config`, `preferences`, `shortcut-grammar` + `shortcuts`, `keep-awake`, `connectivity`,
      `work-status`, `quit`, `update-gate` (+ `UpdateHooks` install-on-quit), `dialogs`, `activity`, `menu`, `tray`, `windows`,
      `ipc-trust`, `shell-host`; `app.ts` rewritten as the Electron adapter layer; `DesktopBridge` gained `maintenance`,
      `resumeStream`, `signedIn`, `streamOpen`, `observe`.
- [x] Preload: `SHELL_CHANNELS` (20 invoke) and five push channels; `PRELOAD_ALLOWLIST` is bridge + shell.
- [x] Renderer: window-kind boot (`index.tsx`), `Popout`, `QuickEntry`, `Indicator`, `DesktopSettings`, App wiring (commands,
      connectivity, activity, offline state), sidebar drag-out, palette entry.
- [x] Tests: unit (config/schema, shortcuts, gates, windows, activity, menu/tray, host, ipc trust, source scan, ui decoders,
      bridge), real-Electron + real-server smoke (`tests/e2e/shell.e2e.spec.ts`), mutation checks.
- [x] Evidence bundle, validator, progress, finalize script.

## Outcome against the reconciliation

Nothing in the existing authority path moved: the main window still reaches the server only through `DesktopBridge`, the one
reconnect is still `StreamClient` + the durable cursor, and `UpdateHooks` still reports `unavailable` for an unsigned build.
The one structural change is that `registerBridgeHandlers` (which installed handlers with no sender check) is gone and
`registerGuardedHandlers` is the only way a handler is installed.
