# APP-002 regression fix — the desktop did not return to sign-in when the session ended

Found by QA-008 on the real Electron app (2026-09-26): after **Sign out**, and after an administrator
revoked the person's sessions while the app was open, the shell stayed on screen. The renderer
listened for the session only inside the sign-in screens (`App` never left the shell once ready);
a command answered with `AUTH_INVALID_TOKEN` was surfaced raw ("Error invoking remote method … session
was revoked"); and the event stream stayed open (the server side is CORE-009's fix).

Fix (`apps/desktop`): `App` returns to sign-in on a `signed-out` session state; `ServerClient` gives
`AUTH_INVALID_TOKEN` the same single refresh as `AUTH_EXPIRED` (a revoked session fails it, and the
session manager signs out and clears custody); the bridge treats an `AUTH_*` stream refusal as a
reason to refresh — resuming from the cursor when the session renews, signing out when it does not.

Tests: `apps/desktop/tests/session.e2e.test.ts` (real Electron, real server: Sign out and revocation
both land on the sign-in screen within seconds), `apps/desktop/tests/identity.test.ts` (a revoked
session signs out after one refresh attempt). Both fail without the fix (`mutation-checks.log`).
