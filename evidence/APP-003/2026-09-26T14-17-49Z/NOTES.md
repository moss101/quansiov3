# APP-003 / APP-012 — a visual design for the desktop and web apps

Raised by the owner during QA-008 (2026-09-26): "css and design is missing". The desktop rendered as
browser-default HTML (`before-d-0.png`): unstyled buttons and fields, headings at default sizes, the
event log filling the bottom of every page; its stylesheet only laid out grids with named colours. The
web app had a bare admin skeleton (`before-w-approvals.png`).

One design system now serves both apps (`apps/desktop/src/renderer/index.css`, `apps/web/src/app/app.css`):
tokens for colour (light and dark, via `prefers-color-scheme`), type, spacing, radius and elevation;
buttons by role (primary for what a form is for and for Approve, danger for Deny/Cancel/Remove, quiet for
toolbar actions, link rows for lists); fields with a focus ring; panels, list rows, status pills (the
status is always written; colour only reinforces it), chat bubbles, the approval preview (a tier-3
warning edge, the arguments block, the bound digest, the untrusted-origin warning), tables and empty
states. The desktop shell is now a sidebar (brand, surfaces, active run), a top bar (connection, settings,
sign out) and the page, with the event stream as a collapsible activity panel. Behaviour fixes that came
with it: only a page's own navigation entry is marked current (embedded surfaces used to light up
together); sign-in providers are written Google/Microsoft/GitHub; the skill form shows its field names.

Keyboard focus stays visible everywhere (`:focus-visible` outlines; fields add a ring), which QA-008's
keyboard walk checks on every surface. Screenshots: `d-*.png` (desktop, 1280×800, light and dark),
`w-*.png` (web in Chrome). Tests: the desktop account/launch/shell suites and the web layout test
(`vitest-design.log`); the full desktop and web suites pass.
