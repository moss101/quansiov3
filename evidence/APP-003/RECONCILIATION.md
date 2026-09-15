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
