-- WorkGraph aggregate revision head (CORE-004).
--
-- DOMAIN.md §1.2 defines `revision` as a per-graph-aggregate compare-and-set counter and
-- §4.5 has a `PlanProposal` carry a single `base_revision` for the whole workspace
-- WorkGraph. 0001 stores a revision per work node/edge, but a set of nodes has no single
-- row on which one atomic transaction can perform the batch-level compare-and-set, so the
-- graph stores add one head row per (tenant, workspace). It is purely additive: no 0001
-- table is altered, and every single-graph mutation advances it so a batch issued against
-- a stale head is rejected wholesale.
--
-- Tenant scope is structural like every other authoritative table: RLS is enabled and
-- forced, and with no `quansio.tenant_id` context the policy matches nothing.

CREATE TABLE graph_heads (
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    revision     BIGINT NOT NULL DEFAULT 1,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, workspace_id)
);

ALTER TABLE graph_heads ENABLE ROW LEVEL SECURITY;
ALTER TABLE graph_heads FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON graph_heads
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

GRANT SELECT, INSERT, UPDATE, DELETE ON graph_heads TO quansio_app;

CREATE TRIGGER graph_heads_set_updated_at
    BEFORE UPDATE ON graph_heads
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
