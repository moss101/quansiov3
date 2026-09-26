# APP-012 — the web app follows the theme preference

The web app applies the person's theme preference (set in the desktop's Settings, stored by
`UpdateUserSettings {theme}`) for the workspace in view: `src/appearance.ts` reads it from `/v1/me`'s
effective settings and sets `data-theme` on the root; `app.css` keys its dark tokens on a chosen dark
theme or, for "system", the OS.

Tests: `vitest-web.log` (22/22 against the dev stack, incl. `appearance.test.ts`) and the web journeys in
Chrome (`web-journeys.log`, 9/9): a journey sets dark through the real server and sees the page dark on a
light system, then light on a dark system, then back to the system. With the web app not applying the
preference the journey fails; the control passes. (The journey lives in `tests/e2e/`, merged under QA-008.)
