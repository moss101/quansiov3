-- Per-tenant RuntimeEvent sequence counters (DOMAIN.md §1.2, §9.1).
--
-- The `sequence` of a RuntimeEvent is tenant-monotonic and assigned inside the commit
-- transaction. A single counter row per tenant is the serialization point: concurrent
-- writers take a row lock on it through `INSERT ... ON CONFLICT DO UPDATE`, so each
-- transaction observes the previous committed value and receives a distinct successor.
-- The increment participates in the caller's transaction, so a rollback releases the
-- value instead of burning it and the sequence stream stays gap-free.

CREATE TABLE tenant_event_sequences (
    tenant_id     TEXT PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    next_sequence BIGINT NOT NULL DEFAULT 1 CHECK (next_sequence >= 1),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE tenant_event_sequences ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenant_event_sequences FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tenant_event_sequences
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

GRANT SELECT, INSERT, UPDATE, DELETE ON tenant_event_sequences TO quansio_app;
