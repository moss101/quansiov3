-- Durable timers for the canonical runtime scheduler (CORE-008).
--
-- A `wait.timer` must survive a process restart, so its due time lives in PostgreSQL,
-- not in the process that scheduled it (`DOSSIER.md` §5 timers/waits/routines,
-- `DOMAIN.md` §5.7 waits, §13.1 routine timing). The row is the durable unit of work:
-- `status` plus `claim_owner`/`claimed_until` is the lease the scheduler loop claims with
-- `FOR UPDATE SKIP LOCKED`, and `generation` is the fencing generation copied from the
-- Run, so a timer scheduled by an obsolete controller cannot wake new work.
--
-- The unique partial index allows a run/wait key to be rescheduled only after the
-- previous timer reached a terminal status (`fired`/`cancelled`); an active duplicate is
-- rejected in the database, not only in application code.
--
-- Tenant scope is structural like every other authoritative table: RLS is enabled and
-- forced, and with no `quansio.tenant_id` context the policy matches nothing.

CREATE TABLE durable_timers (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'tmr\_%'),
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id  TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    run_id        TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    step_id       TEXT REFERENCES steps(id) ON DELETE SET NULL,
    wait_key      TEXT NOT NULL,
    due_at        TIMESTAMPTZ NOT NULL,
    status        TEXT NOT NULL DEFAULT 'scheduled'
                  CHECK (status IN ('scheduled', 'claimed', 'fired', 'cancelled')),
    generation    BIGINT NOT NULL,
    claim_owner   TEXT,
    claimed_until TIMESTAMPTZ,
    fired_at      TIMESTAMPTZ,
    cancel_reason TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (claim_owner IS NULL OR status = 'claimed'),
    CHECK ((status = 'claimed') = (claimed_until IS NOT NULL))
);

CREATE UNIQUE INDEX durable_timers_active_wait_idx
    ON durable_timers (run_id, wait_key)
    WHERE status IN ('scheduled', 'claimed');

CREATE INDEX durable_timers_due_idx
    ON durable_timers (due_at)
    WHERE status IN ('scheduled', 'claimed');

ALTER TABLE durable_timers ENABLE ROW LEVEL SECURITY;
ALTER TABLE durable_timers FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON durable_timers
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

GRANT SELECT, INSERT, UPDATE, DELETE ON durable_timers TO quansio_app;

CREATE TRIGGER durable_timers_set_updated_at
    BEFORE UPDATE ON durable_timers
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
