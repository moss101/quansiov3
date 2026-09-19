# APP-003 — Electron desktop shell and secure preload bridge

## What was built (task branch `task/APP-003-electron-shell`, merged 5bbaea6 and 305b3f8)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md).
`apps/desktop` now is the shell:

- **Electron main** (`src/main/app.ts`, the only Electron importer besides the preload):
  one window; the renderer security baseline applied through real handlers — CSP injected
  on every response, device permissions denied, navigation pinned to the bundled `app://`
  document, `window.open` denied with https handed to the OS; IPC handlers for exactly the
  preload allowlist; `quansio://` deep links parsed into typed intents; update hooks on the
  stable/beta channels (reported `unavailable` until the build is signed, REL-002/003); a
  durable session file under `userData` with the active run and last stream cursor. Main
  holds the session scope and is the only process that opens a socket.
- **Preload** (`src/preload/index.ts`): `contextBridge` exposes `createPreloadApi` over
  `ipcRenderer.invoke`/`on`, restricted to the allowlist and push channels; no Node global
  reaches the page.
- **Typed clients**: `ServerClient` (commands, projections, health, §15 errors),
  `StreamClient` (§9.3 subscribe, cursor bookkeeping, bounded reconnect, event vs live
  frames), `LocalRuntimeClient` (the §14 execution commands the desktop-hosted substrate
  registers through, like any other target).
- **React renderer**: surfaces, connection state, active run, composer, stream log fed only
  by bridge pushes; every visible string is an `i18n` key. esbuild bundles main/preload as
  CommonJS and the renderer as ESM served from `app://`.

Electron's binary download stays an unapproved build script in `pnpm-workspace.yaml`, so
CI installs the package (types) without fetching the runtime; the tests fake the module.

## Acceptance

1. **Renderer security baseline passes** — `tests/security.test.ts` asserts the baseline on
   the installed handlers (CSP replacement, permission denial, navigation and window-open
   guards) and that nothing the renderer can load imports a Node API;
   `tests/preload.test.ts` loads the real preload script with Electron faked and checks the
   exact surface of `window.quansio` (permission boundary).
2. **Desktop reconnects without losing active run state** — `tests/reconnect.e2e.test.ts`
   against the real `quansio-server` binary on a scratch database seeded by
   `quansio-dev-seed`: post → durable event with cursor → run and cursor persisted →
   the bridge is torn down and rebuilt from the file alone → run and cursor restored →
   reconnect from the cursor → a later post's events arrive with later cursors → memory,
   file and stream client agree. `reconnect-e2e-real-server.log`.

## What the E2E found

- **Cursor ↔ channel set.** The server binds a cursor to the channel set it was issued
  under (`cursor stream mismatch`). The desktop's one durable subscription is therefore the
  workspace channel, which carries every run's durable events; a non-retryable refusal
  makes the client forget its cursor and resume from the server's position rather than
  repeat the refusal (unit-tested).
- **Server defect (APP-001, fixed at 0943011).** The stream handler kept the live-frame
  sender alive, so the replay session waited on it forever and `tail_events` never ran:
  no client ever received an event committed after it subscribed. The walking skeleton
  posted before subscribing and never saw it. See `evidence/APP-001/2026-09-19T18-41-53Z/`.
- **Client harness vs authority.** The first E2E imported `pg` and inserted a tenant from
  the desktop package; the architecture gate refused it (`ci-run1-client-seed-refused.log`).
  `quansio-dev-seed` (Rust, crates/server) now owns scratch-database creation and seeding
  for every harness.

## Gates on 305b3f8

`pnpm build` (tsc + esbuild bundle), `pnpm typecheck`, desktop vitest 41/41 (16 files),
workspace `pnpm test`, `pnpm lint` (eslint strictTypeChecked, clean), the repo e2e journeys
spec (its reconnect journey now uses the durable session file), `scripts/validate_v81.py`
PASS, `bash scripts/ci/ci.sh` PASS.
