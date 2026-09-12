-- Derived client read models (CORE-009, DOSSIER.md §5 "Product state display: client
-- projections only", §8, DOMAIN.md §9.2).
--
-- Both tables are projections of the authoritative RuntimeEvent stream: they are
-- rebuildable from zero, are never consulted for recovery and are never a second event
-- store. Every row is a pure function of the events consumed, so the state is
-- byte-identical after incremental application and after a rebuild. That is also why
-- there is no wall-clock `created_at`/`updated_at`: those would differ between the two
-- paths. `first_occurred_at`/`last_occurred_at` come from the event envelope.
--
-- `last_sequence` and `event_count` make re-application idempotent: an upsert only
-- advances a row when the incoming `sequence` is strictly greater than the stored one,
-- so replaying an already-applied event neither double-counts nor regresses state.
--
-- Tenant scope is structural: RLS is enabled and forced, and with no
-- `quansio.tenant_id` context the policy matches nothing.

CREATE TABLE run_status_projection (
    tenant_id          TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id             TEXT NOT NULL CHECK (run_id LIKE 'run\_%'),
    workspace_id       TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    work_node_id       TEXT,
    agent_thread_id    TEXT,
    status             TEXT NOT NULL CHECK (status IN (
                           'CREATED', 'QUEUED', 'RUNNING', 'WAITING',
                           'WAITING_APPROVAL', 'WAITING_QUESTION', 'WAITING_EVENT',
                           'WAITING_TIMER', 'WAITING_CHILD', 'WAITING_TAKEOVER',
                           'VERIFYING', 'SUCCEEDED', 'FAILED', 'CANCELLED',
                           'BLOCKED_UNRECOVERABLE', 'SUSPENDED')),
    last_event_type    TEXT NOT NULL,
    current_turn_id    TEXT,
    terminal_reason    TEXT,
    started_at         TIMESTAMPTZ,
    ended_at           TIMESTAMPTZ,
    first_occurred_at  TIMESTAMPTZ NOT NULL,
    last_occurred_at   TIMESTAMPTZ NOT NULL,
    event_count        BIGINT NOT NULL DEFAULT 1 CHECK (event_count >= 1),
    last_sequence      BIGINT NOT NULL CHECK (last_sequence >= 1),
    PRIMARY KEY (tenant_id, run_id)
);
CREATE INDEX run_status_projection_workspace_idx
    ON run_status_projection (tenant_id, workspace_id, run_id);

CREATE TABLE work_node_status_projection (
    tenant_id            TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    work_node_id         TEXT NOT NULL CHECK (work_node_id LIKE 'wn\_%'),
    workspace_id         TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    kind                 TEXT,
    title                TEXT,
    status               TEXT NOT NULL CHECK (status IN (
                             'draft', 'ready', 'blocked', 'in_progress', 'waiting',
                             'verifying', 'done', 'failed', 'cancelled')),
    owner_agent_thread_id TEXT,
    revision             BIGINT,
    last_event_type      TEXT NOT NULL,
    first_occurred_at    TIMESTAMPTZ NOT NULL,
    last_occurred_at     TIMESTAMPTZ NOT NULL,
    event_count          BIGINT NOT NULL DEFAULT 1 CHECK (event_count >= 1),
    last_sequence        BIGINT NOT NULL CHECK (last_sequence >= 1),
    PRIMARY KEY (tenant_id, work_node_id)
);
CREATE INDEX work_node_status_projection_workspace_idx
    ON work_node_status_projection (tenant_id, workspace_id, work_node_id);

ALTER TABLE run_status_projection ENABLE ROW LEVEL SECURITY;
ALTER TABLE run_status_projection FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON run_status_projection
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

ALTER TABLE work_node_status_projection ENABLE ROW LEVEL SECURITY;
ALTER TABLE work_node_status_projection FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON work_node_status_projection
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

GRANT SELECT, INSERT, UPDATE, DELETE ON run_status_projection TO quansio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON work_node_status_projection TO quansio_app;
