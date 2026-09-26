# APP-012 — the web app on the refreshed design system

After the desktop design refresh (APP-003, merge 44f4e47) the owner asked for the same on the web
review and administration app, which still wore the earlier indigo design (`before-w-users.png`).

The web app now uses the desktop's tokens and patterns (`apps/web/src/app/app.css`, `App.tsx`,
`pages.tsx`, `icons.tsx`, `faces.tsx`): ink-on-paper palette with a full dark theme; a sidebar
with the brand mark, pill workspace switcher, icon rows and an account footer (initials, name,
roles, sign-out); an icon rail with tooltips on tablets; a top bar and slide-in drawer on phones;
hairline card grids with status pills; tables in their own scrolling panels; initials faces for
people; the approval preview as a modal bound to its digest; a calm refusal card; the desktop's
sign-in look. The layout modes and breakpoints (`src/layout.ts`) are unchanged.

Found in Chrome and fixed: a CSS tooltip's generated text joined the rail buttons' accessible names
("Users Users"), so nav buttons carry their label explicitly; a visually hidden table header
escaped its scrolling panel and gave the phone Users page about 290px of sideways scroll, so the panel
is its positioning context.

Tests: the web package suite against the dev stack (`vitest-web.log`, 19/19: layout, RBAC,
review, client, faces, and the real-server RBAC/approval E2E) and the QA-008 web journeys in
Chrome against the real server (`web-journeys.log`, 7/7, including accessibility and keyboard on
every page and no horizontal scroll at 375px).

Screenshots (Chrome, real server): `w-signin.png`, `w-approval-dialog.png` (1280×800),
`w-users-dark.png`, `w-tablet.png` (900 wide, rail), `w-phone.png` and `w-phone-menu.png` (375 wide).
