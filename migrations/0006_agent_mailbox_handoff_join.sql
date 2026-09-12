-- RUN-002: AgentThread mailbox, recoverable handoff and once-only join lineage.
--
-- `0001_canonical_schema.sql` owns `agent_threads` and `agent_graph_edges`. Three
-- durable pieces DOMAIN.md §5.1/§9.2 requires are not modelled there and are added
-- additively here, without touching the existing rows:
--
--   * `agent_mailbox_items` — the durable per-AgentThread mailbox the `mailbox_cursor`
--     on `agent_threads` advances over. `(agent_thread_id, seq)` is the monotonic
--     order; `message_id` is unique per thread so redelivery is idempotent.
--   * `agent_handoffs` — the durable intent written before a thread starts handing its
--     run over, so a restarted runtime can finish or roll the handoff back instead of
--     guessing (DOSSIER.md §8).
--   * `agent_joins` — the once-only merge of a worker's outcome into its parent, keyed
--     by the child so a repeated join cannot duplicate lineage.
--
-- Tenant scope is structural, as in 0001: every table carries `tenant_id`, RLS is
-- enabled and forced, and a missing `quansio.tenant_id` context matches nothing.

CREATE TABLE agent_mailbox_items (
    agent_thread_id TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    seq             BIGINT NOT NULL CHECK (seq > 0),
    tenant_id       TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id    TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    message_id      TEXT NOT NULL,
    payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    delivered_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (agent_thread_id, seq),
    UNIQUE (agent_thread_id, message_id)
);
CREATE INDEX agent_mailbox_items_thread_idx ON agent_mailbox_items (agent_thread_id, seq);

CREATE TABLE agent_handoffs (
    id                       TEXT PRIMARY KEY CHECK (id LIKE 'ahf\_%'),
    tenant_id                TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id             TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    from_agent_thread_id     TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    to_agent_thread_id       TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    run_id                   TEXT REFERENCES runs(id) ON DELETE SET NULL,
    run_generation           BIGINT,
    work_node_id             TEXT REFERENCES work_nodes(id) ON DELETE SET NULL,
    mailbox_cursor           TEXT,
    evidence_ids             JSONB NOT NULL DEFAULT '[]'::jsonb,
    pending_child_thread_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    conversation_ref         TEXT,
    status                   TEXT NOT NULL DEFAULT 'PENDING'
                             CHECK (status IN ('PENDING', 'COMPLETED', 'ROLLED_BACK')),
    completed_at             TIMESTAMPTZ,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (from_agent_thread_id <> to_agent_thread_id)
);
-- At most one live handoff per source thread (per run, when one is named).
CREATE UNIQUE INDEX agent_handoffs_live_idx
    ON agent_handoffs (from_agent_thread_id, COALESCE(run_id, ''))
    WHERE status = 'PENDING';
CREATE INDEX agent_handoffs_workspace_idx ON agent_handoffs (workspace_id, status);

CREATE TABLE agent_joins (
    child_agent_thread_id  TEXT PRIMARY KEY REFERENCES agent_threads(id) ON DELETE CASCADE,
    tenant_id              TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id           TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    parent_agent_thread_id TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    edge_id                TEXT NOT NULL REFERENCES agent_graph_edges(id) ON DELETE CASCADE,
    child_status           TEXT NOT NULL,
    evidence_ids           JSONB NOT NULL DEFAULT '[]'::jsonb,
    artifact_ids           JSONB NOT NULL DEFAULT '[]'::jsonb,
    outcome                JSONB NOT NULL DEFAULT '{}'::jsonb,
    joined_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX agent_joins_parent_idx ON agent_joins (parent_agent_thread_id, joined_at);

DO $$
DECLARE
    t TEXT;
    run_002_tables TEXT[] := ARRAY['agent_mailbox_items', 'agent_handoffs', 'agent_joins'];
BEGIN
    FOREACH t IN ARRAY run_002_tables LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I USING (tenant_id = current_setting(''quansio.tenant_id'', true)) '
            'WITH CHECK (tenant_id = current_setting(''quansio.tenant_id'', true))', t);
    END LOOP;
END $$;

CREATE TRIGGER agent_handoffs_set_updated_at BEFORE UPDATE ON agent_handoffs
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
