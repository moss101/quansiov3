# APP-003 — navigation named surfaces by internal id

Found by QA-008 on the real app (2026-09-26): the Surfaces navigation read `live-browser-terminal`,
`knowledge-controls`, `automations`…, and six DOSSIER §15 surfaces (`plan-and-agents`, `run-status`,
`evidence-timeline`, `connectors`, `capability-admin`, `model-usage`) were inert text although five
of them are shown inside another page. Fix: every surface has an externalized label; the embedded ones
open their host page (objectives, chat, approvals, knowledge); connector setup is labelled as
administered in the web app (the desktop has no connector page; the web admin has one);
`aria-current="page"` marks the open page. Test: `apps/desktop/tests/session.e2e.test.ts` — 15
entries, none named by an id, 14 buttons each opening a page.
