# APP-012 — a command palette for the web app

The owner asked for the desktop's Cmd/Ctrl+K palette on the web app too. `apps/web/src/app/Palette.tsx`
(the web app's own component; nothing is imported across owners) opens from Cmd/Ctrl+K anywhere once
signed in, or from the search button in the sidebar's brand row (the top bar on phones; outside the
navigation landmark, so the page list is unchanged). It offers what the shell offers: the pages the
session's roles allow (`offered(roles)`), the person's workspaces (switching the one in view) and
signing out. Arrow keys move, Return opens, Escape or the scrim closes.

Tests: `tests/palette.test.ts` (matching) in the web package suite (`vitest-web.log`, 20/20 against
the dev stack), and the web journeys in Chrome against the real server (`web-journeys.log`, 8/8):
a new journey jumps to Audit & privacy from the shortcut and closes the palette with Escape, and the
viewer journey checks the palette offers a viewer no administration page. Mutation checks: making the
palette list every page fails the viewer journey; making the shortcut do nothing fails the palette
journey; the controls pass. (The journeys live in `tests/e2e/`, merged under QA-008.)

Screenshots (Chrome, real server): `w-palette-all.png`, `w-palette.png` (1280×800), `w-audit.png`
(after Return), `w-phone-palette.png` (375 wide, from the top bar's search button).
