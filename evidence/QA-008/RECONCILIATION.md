# QA-008 reconciliation — qualify desktop/web UX and accessibility

**Canonical owner.** `tests/e2e/` (critical journeys, keyboard/accessibility and degraded-state checks
against the real apps). Product defects the journeys expose are fixed in the app that owns them
(`apps/desktop`, `apps/web`) and noted on its task.

## Existing code and coverage — `PARTIAL`

`tests/e2e/journeys.spec.ts` (2026-09-15) drives the shipped journey *modules* in-process (onboarding
plan, composer params, approval binding, artifact diff, live-session reducer…) and asserts an
accessibility baseline file; its served-build tier (`QUANSIO_TEST_E2E=1`) never ran. The app suites
(`apps/desktop/tests/*.e2e.test.ts`, `apps/web/tests/web.e2e.test.ts`) drive the bridge and client
modules against the real server — not the rendered UI. No test had ever launched the real Electron
binary: pnpm never ran electron's install script, so `node_modules/electron/dist` did not exist.

## Findings so far

| Id | Finding | Severity | Status |
|---|---|---|---|
| U-1 | The real desktop binary rendered a blank window: `app:` was served with Node's `fetch`, which cannot read `file:` URLs. | P0 (every desktop journey) | fixed on `fix/APP-003-renderer-protocol` (merged 998db3a), regression test `apps/desktop/tests/launch.e2e.test.ts` |
| U-2 | Desktop navigation names surfaces by internal id (`live-browser-terminal`, `knowledge-controls`), and lists six unimplemented surfaces as inert text. | P2 | open |
| U-3 | Settings (a QA-008 journey) have no UI in either app: the desktop bridge exposes `settings.updateWorkspace` / `settings.updateUser`, but no surface calls them. | P1 (journey absent) | open |

## Real boundaries and support matrix (DOSSIER §21.1)

| Surface | This host | Status |
|---|---|---|
| macOS desktop (14+, Apple Silicon) | macOS 26.5.1, Apple Silicon, Electron 44.4.3 | runnable |
| Windows desktop (11 22H2+) | — | `BLOCKED_EXTERNAL`: needs a Windows 11 host (`QUANSIO_TEST_WINDOWS_DESKTOP=1`) |
| Web: Chrome (latest two) | Google Chrome 153 via `playwright-core` (`channel: chrome`) | runnable |
| Web: Edge, Firefox | not installed | `BLOCKED_EXTERNAL` (Playwright browser builds or the browsers, owner-approved download) |
| Web: Safari | installed, not drivable without enabling Remote Automation (a system setting) | `BLOCKED_EXTERNAL` |

Downloads approved by the owner on 2026-09-26: `playwright-core` (npm) and the Electron binary only.

## Persistent state, contracts, effects

None changed by the campaign itself: each run seeds a scratch database (`quansio-dev-seed`) and starts
the built server; the apps are driven as a person drives them.

## Plan

- [ ] Harness (`tests/e2e/harness.ts`): scratch server with the built web app, magic-link sign-in from
      the server log, restart for degraded states, real Electron launcher (mock keychain, throwaway
      profile), Chrome launcher, an accessibility audit over the browser's own accessibility tree
      (unnamed controls, document language, headings, keyboard reachability, visible focus).
- [ ] Desktop journeys (real Electron): onboarding, chat/objective, plan steering, approval, browser
      takeover, artifact review, collaboration, routines, settings; offline/reconnect.
- [ ] Web journeys (Chrome): sign-in, review, approvals, administration, viewer RBAC; keyboard and
      accessibility.
- [ ] Fix U-2 and U-3 (and whatever else the journeys expose) in their apps.
- [ ] CI, evidence, progress.
