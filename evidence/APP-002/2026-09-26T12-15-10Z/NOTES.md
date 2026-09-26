# APP-002 gap closed — the desktop had no settings surface

Found by QA-008 (2026-09-26): the settings journey could not be driven because no desktop surface
called `settings.updateWorkspace` / `settings.updateUser`, so APP-002's build item "show effective
policy/settings and restart/reindex effects" was met by the server and the bridge but not by the app.

`apps/desktop/src/renderer/Settings.tsx` (opened from the header): the workspace layer — name, where
work runs, the four security controls, retention — for workspace administrators (disabled, with a
note, for everyone else); the person's own layer (theme); the effective settings the server returns
after its merge (security controls only get stricter, so asking for telemetry leaves it off); and the
restart/reindex effects the server reports for a change (moving the execution target re-provisions
the workspace computer).

Tests (real Electron, real server): `apps/desktop/tests/session.e2e.test.ts` — the owner changes where
work runs and retention and sees the restart and the effective values, sets a dark theme, asks for
telemetry and sees it stay off; a viewer sees the workspace controls disabled and their own enabled.
The full desktop suite passes.
