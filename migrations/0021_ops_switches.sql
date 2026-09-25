-- Operator kill switches (OPS-008, DOSSIER.md §21.5, DOMAIN.md §7.5). One row per engagement,
-- kept as history; at most one active row per (tenant, switch, target). Additive.
CREATE TABLE ops_switches (
    id             TEXT PRIMARY KEY CHECK (id LIKE 'ksw\_%'),
    tenant_id      TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    switch         TEXT NOT NULL CHECK (switch IN ('provider.disable', 'connector.revoke',
                   'worker.quarantine', 'target.drain', 'effect.freeze', 'policy.emergency_deny',
                   'stream.shed')),
    target         TEXT NOT NULL CHECK (length(target) BETWEEN 1 AND 256),
    reason         TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    engaged_by     JSONB NOT NULL,
    engaged_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    released_by    JSONB,
    released_at    TIMESTAMPTZ,
    release_reason TEXT,
    CHECK ((released_at IS NULL) = (released_by IS NULL))
);
CREATE UNIQUE INDEX ops_switches_active_idx
    ON ops_switches (tenant_id, switch, target) WHERE released_at IS NULL;
CREATE INDEX ops_switches_history_idx ON ops_switches (tenant_id, engaged_at DESC);

ALTER TABLE ops_switches ENABLE ROW LEVEL SECURITY;
ALTER TABLE ops_switches FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON ops_switches
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON ops_switches TO quansio_app;
