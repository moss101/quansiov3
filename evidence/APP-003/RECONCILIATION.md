# APP-003 reconciliation

## Canonical owner

`apps/desktop/` is the desktop shell. It is projection + command surface, never authority.
The Rust server owns commands, events and run state. Existing `surfaces.ts` is the
DOSSIER §15 catalog and stays.

## Coverage

`GENUINE_GAP` for Electron main, preload and renderer. The package currently exports only
the surface list.

## Contracts

Renderer talks to the server through a typed client over HTTPS/WebSocket (`GET /v1/stream`,
`POST /v1/commands/:name`). Preload exposes only that client plus deep-link/update hooks.
No Node integration, no `fs`/`child_process` from renderer.

## Isolation

`nodeIntegration: false`, `contextIsolation: true`, `sandbox: true`, a locked CSP.
Permission requests default deny. Session reconnect restores `run_id` + stream cursor
from disk in the main process, never from renderer-held secrets.

## Tests

CSP/preload allowlist, permission deny, reconnect of run cursor. No Electron binary is
required: tests drive the shipped policy and session functions.

---

## 2026-09-19 re-reconciliation (agent:claude-opus-5)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
`apps/desktop` has no Electron dependency, `src/main/shell.ts` composes constants, the preload
is an allowlist with no `contextBridge`, the "client" builds URLs, and the renderer session
is an in-memory class. Nothing runs.

- **Canonical owner:** `apps/desktop/` (matches `paths`). Projection + command surface only
  (invariant 4); no runtime authority, no secrets in the renderer (invariant 8).
- **Existing code, coverage PARTIAL:** kept as the baseline — `RENDERER_WEB_PREFERENCES`,
  `RENDERER_CSP`, `DENIED_PERMISSIONS`, `PRELOAD_ALLOWLIST`, `createPreloadApi`,
  `DESKTOP_SURFACES`, `STRINGS`, and the per-surface stubs other APP tasks own.
- **Persistent state touched:** a main-process session file under Electron `userData`
  (active run id/generation, stream cursor, workspace, server origin). Not authority: the
  server owns runs; the file only lets the desktop resume the same subscription.
- **Contracts:** public API v1 as served by APP-001 — `POST /v1/commands/<Name>` with
  `{command_id, params}` → `{command_id, replayed, result}`; `GET /v1/runs/{id}`,
  `/v1/messages`, `/v1/effects`, `/v1/health`; `GET /v1/stream` WebSocket with
  `{subscribe: [{channel}], cursor?, consumer?}` and frames `{kind: "event", cursor, event}` /
  `{kind: "live", channel, frame}` (DOMAIN.md §9.3); errors `{code, message,
  correlation_id, retryable}` (§15). Scope headers `x-quansio-tenant`, `x-quansio-user`,
  `x-quansio-consumer`. No new contract; nothing added to DOMAIN.md.
- **External effects:** none from the desktop itself; every mutation is a public command.
- **Capabilities/approvals:** none in this task (APP-006 owns the approvals surface).
- **Migrations:** none.
- **Real boundaries:** none declared (`real_boundary: false`). The reconnect E2E runs against
  the real `quansio-server` binary on a scratch PostgreSQL database from
  `QUANSIO_TEST_POSTGRES_URL`; without it the E2E skips with a `BLOCKED_EXTERNAL` marker.
- **Rollback/recovery:** the session file is advisory; deleting it loses only the cursor
  (the next subscribe replays from the server's current position).

## Plan (checklist)

1. Electron main: window with the security baseline applied through real handlers (CSP
   header injection, permission requests, navigation and window-open guards), IPC handlers
   for exactly the preload allowlist, deep links (`quansio://`), update hooks (stable/beta
   channels, DOSSIER.md §18), a durable session file.
2. Secure preload: `contextBridge.exposeInMainWorld("quansio", …)` over `ipcRenderer.invoke`
   restricted to the allowlist; no Node globals reach the page.
3. Typed clients: `ServerClient` (commands, reads, health, error taxonomy), `StreamClient`
   (subscribe, cursor bookkeeping, reconnect with bounded backoff, event vs live frames),
   `LocalRuntimeClient` (the desktop-hosted execution target registers through the same API).
4. React renderer: surfaces, connection state, composer, stream log; strings from `i18n`.
5. Tests: CSP/preload (baseline + handlers), permission boundary (preload exposure, IPC
   refusal of non-allowlisted channels), reconnect E2E against the real server: post → event
   → persist → drop socket → reconnect from the persisted cursor → active run intact.
6. Bundle (esbuild) + typecheck + vitest + eslint; evidence; progress.
