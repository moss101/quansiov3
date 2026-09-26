# QA-008 evidence — desktop/web UX and accessibility on the real apps

**Tier.** `pnpm test:e2e` (`tests/e2e/`): the built desktop app in the real Electron 44.4.3 binary
(Playwright's Electron driver, mock keychain, throwaway profile) and the built web app in the
installed Google Chrome 153 (`playwright-core`), both against the real `quansio-server` on a scratch
database seeded by `quansio-dev-seed` (owner, approver, viewer; a write policy that asks; a live run on
an execution target). Sign-in is the real magic-link flow, the token read from the server's console.

## Journeys (23 tests, all passing)

Desktop (12): onboarding a new person (sign-in, where work runs, security defaults — written to the
workspace); chat with a teammate (reply and turn inspector); approval from the exact preview (the
bound digest equals the server's, untrusted-origin warning shown, the effect settles under a consumed
receipt); plan steering (draft an edit, propose it, the plan revision advances, the WorkGraph holds
the task); live browser takeover and hand-back; artifact review (versions and preview); a group thread
with a colleague; a routine; settings (the change and the restart it causes); an administrator's
revocation returns the person to sign-in; no accessibility blocker on any surface, every control
keyboard-reachable with visible focus; server down → "Reconnecting…", server back → "Connected.".

Web (7): sign-in from the emailed link and the work review; an approver's remote decision bound to its
preview (and audited under the approver); member administration (invite, then the audit entry); a
viewer offered no administration (and refused by the server); no accessibility blocker, keyboard
reachability and visible focus on every page, no horizontal scroll at 375 px; sign-out; and a self-test
that the audit catches an unnamed control, hidden focus and a positive tabindex.

Visible focus means an outline, or a box-shadow that appears or changes when the control takes focus.
The design gives buttons a raised shadow at rest; the first rerun after it counted that shadow as focus
and let the self-test's outline-less button pass, so the walk now compares against each control's
resting shadow. A surface is audited once its DOM has been quiet for 400 ms (`settle`), not after a
fixed pause: the settings page loads the workspace after it opens and replaces its form, and one run
under load judged the old form's controls unreachable. The tier then passed three runs in a row.

Module journeys (4): the in-process journey modules (`journeys.spec.ts`), with a stale roster case
updated to the current teammate shape. The self-certified accessibility baseline (a hand-written JSON
file) and the never-run gated Playwright tier were removed.

## Findings and where they were fixed

| Id | Finding | Severity | Fix |
|---|---|---|---|
| U-1 | The real desktop binary rendered a blank window (`app:` served with Node's `fetch`). | P0 | APP-003 `fix/APP-003-renderer-protocol` (998db3a) |
| U-3 | No settings surface in the desktop (APP-002's "show effective settings and restart/reindex effects"). | P1 | APP-002 `fix/APP-002-settings-surface` (f46f86e) |
| U-5a | An open event stream outlived a revoked, removed or expired session (server). | P1 (security) | CORE-009 `fix/CORE-009-stream-session-revalidation` (12ca2a4) |
| U-5b | Sign-out and revocation left the desktop shell on screen; a revoked token surfaced raw. | P1 | APP-002 `fix/APP-002-revoked-session` (0edc56e) |
| U-5c | A revoked session's reconnecting stream was retried forever (401 handshake treated as a network blip). | P1 | APP-002 `fix/APP-002-stream-handshake-refusal` (52c5d56) |
| U-2 | Navigation named surfaces by internal id; six §15 surfaces were inert text. | P2 | APP-003 `fix/APP-003-navigation-labels` (e9e0f9d) |
| U-8 | The desktop and web apps had almost no visual design (raised by the owner): browser-default controls, the event log filling every page, several navigation entries marked current at once. | P2 | APP-003/APP-012 `fix/APP-003-visual-design` (ef5a3b1) |
| U-9 | Creating an artifact while the database restarted answered `VALIDATION_BOUNDS` with the raw driver message ("expected to read 5 bytes"). | P3 | CORE-007 `fix/CORE-007-artifact-error-codes` (aa8d057) |
| U-4 | Onboarding shows internal identifiers (`local_capsule`, `retention_days: 365`). | P3 | open (recorded) |
| U-6 | The web email field is named by its example text (`you@example.com`), not a label. | P3 | open (recorded) |
| U-7 | Connector setup has no desktop page (administered in the web app); pack administration waits on CAP-005. | P3 | open (recorded; the navigation says where) |

## Mutation checks

Each breaks one thing a person relies on, the apps are rebuilt, and the journey that covers it is run —
after a control run of the same journey unmutated passed: desktop Approve does nothing; desktop Propose
does nothing; the desktop Send button loses its name; web Approve does nothing; desktop settings not
saved; the desktop focus outline removed (buttons keep only their resting shadow) — 6 of 6 killed (run at 3840728; later commits change only how a journey waits for a page). (A first pass without controls was discarded: filtered desktop journeys then
depended on an earlier test's sign-in and failed on their own.)

## Support matrix (DOSSIER.md §21.1)

| Surface | Result |
|---|---|
| macOS desktop (14+, Apple Silicon) | macOS 26.5.1, Apple Silicon — all desktop journeys pass |
| Windows 11 desktop | `BLOCKED_EXTERNAL` — needs a Windows 11 host (`QUANSIO_TEST_WINDOWS_DESKTOP=1`) |
| Web: Chrome | Chrome 153 — all web journeys pass |
| Web: Edge, Firefox | `BLOCKED_EXTERNAL` — not installed; the owner approved only `playwright-core` and the Electron binary |
| Web: Safari | `BLOCKED_EXTERNAL` — installed but not drivable without enabling Remote Automation (a system setting) |
