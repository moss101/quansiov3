-- Hardened managed browser (ADP-001, DOMAIN.md §8.4).
--
-- A browser session already survives its worker (0001). What the hardened browser adds is state the
-- *server* must hold for the user and for recovery, and that no worker-local file may be the only copy
-- of: the tabs' document revisions, a JavaScript dialog waiting for an answer, a certificate error
-- waiting for a user, the downloads of a tab and the browser actions whose outcome is in doubt.
--
-- This is one more column on the existing owner table, not a new store. `tabs` (already JSONB) carries
-- each tab's `document_revision`; `guard_state` carries the rest as the worker last reported it, and the
-- user's certificate and permission decisions. It is rebuildable from the worker's next report except for
-- those decisions, which is why they are written by machine control and not by the worker.
ALTER TABLE browser_sessions
    ADD COLUMN guard_state JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE browser_sessions
    ADD CONSTRAINT browser_sessions_guard_state_object
    CHECK (jsonb_typeof(guard_state) = 'object');
