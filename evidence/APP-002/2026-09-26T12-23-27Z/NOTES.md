# APP-002 — a refused stream handshake was retried forever

Found by QA-008 (2026-09-26): when a person's session was revoked while their desktop's stream was
reconnecting, every reconnect was refused at the WebSocket handshake with HTTP 401 — before any
frame, so the in-band `AUTH_*` refusal (CORE-009) never arrived — and the client backed off and
retried with the same dead credential until it gave up, leaving the shell on "Reconnecting…".

Fix (`apps/desktop/src/client/stream.ts`): the `ws` client reports a refused handshake as an error
event ("Unexpected server response: <status>"); a 401 or 403 now closes the subscription and surfaces
a typed `AUTH_INVALID_TOKEN` / `SCOPE_FORBIDDEN` refusal, which the bridge answers with one refresh
(resume from the cursor) or a sign-out. Other failures (a 502, a dropped socket) still back off.

Tests: `apps/desktop/tests/shell-modules.test.ts` (401/403 stop retrying and surface the refusal;
502 retries), with the session-end journeys on the real app (`session.e2e.test.ts`). Mutation: with
the refusal handling disabled the unit test fails.
