-- ADP-007: routine revisions, idempotent creates, inbound hooks and their replay ledger (DOMAIN.md 13.8).
--
-- All of it belongs to the routine owner (`crates/server/src/control/routines`):
--   * `routines.revision` counts changes to a routine's definition or status (the optimistic-concurrency token);
--   * `routines.create_request_id` and `create_digest` make a client-named create idempotent per workspace;
--   * `routines.hook_endpoint_id` is the unguessable name of an inbound hook and `hook_secret_handle_id` the SecretHandle
--     (secret broker) that holds its signing secret; neither the secret nor the endpoint's meaning is stored here;
--   * `routine_fires.payload` keeps the accepted body of an inbound delivery with its fire, and the source `hook` is admitted;
--   * `routine_hook_deliveries` remembers every authenticated delivery by the sender's delivery key and by the digest of its
--     signature, so a replay fires nothing, whatever it says in headers, and across a restart.
-- A function resolves an endpoint to its tenant and routine without a tenant context, because an inbound request carries
-- none: it returns ids only. Additive and forward-safe.

ALTER TABLE routines
    ADD COLUMN revision              INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    ADD COLUMN create_request_id     TEXT,
    ADD COLUMN create_digest         TEXT,
    ADD COLUMN hook_endpoint_id      TEXT,
    ADD COLUMN hook_secret_handle_id TEXT;

CREATE UNIQUE INDEX routines_create_request_idx
    ON routines (tenant_id, workspace_id, create_request_id) WHERE create_request_id IS NOT NULL;
CREATE UNIQUE INDEX routines_hook_endpoint_idx
    ON routines (hook_endpoint_id) WHERE hook_endpoint_id IS NOT NULL;

ALTER TABLE routine_fires DROP CONSTRAINT routine_fires_source_check;
ALTER TABLE routine_fires ADD CONSTRAINT routine_fires_source_check
    CHECK (source IN ('schedule', 'event', 'manual', 'hook'));
ALTER TABLE routine_fires ADD COLUMN payload JSONB;

CREATE TABLE routine_hook_deliveries (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'rhd\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id     TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    routine_id       TEXT NOT NULL REFERENCES routines(id) ON DELETE CASCADE,
    delivery_key     TEXT NOT NULL,
    signature_digest TEXT NOT NULL,
    outcome          TEXT NOT NULL CHECK (outcome IN ('accepted', 'filtered')),
    fire_id          TEXT,
    received_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, routine_id, delivery_key),
    UNIQUE (tenant_id, routine_id, signature_digest)
);
CREATE INDEX routine_hook_deliveries_recent_idx
    ON routine_hook_deliveries (tenant_id, routine_id, received_at DESC);

ALTER TABLE routine_hook_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE routine_hook_deliveries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON routine_hook_deliveries
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON routine_hook_deliveries TO quansio_app;
CREATE TRIGGER routine_hook_deliveries_set_updated_at
    BEFORE UPDATE ON routine_hook_deliveries
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- The tenant and routine an inbound endpoint belongs to: ids and nothing else. The request then sets the tenant context and
-- reads the rest under the ordinary policies.
CREATE OR REPLACE FUNCTION routine_hook_lookup(p_endpoint TEXT)
    RETURNS TABLE (tenant_id TEXT, routine_id TEXT)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public
    AS $$ SELECT tenant_id, id FROM routines WHERE hook_endpoint_id = p_endpoint $$;
REVOKE ALL ON FUNCTION routine_hook_lookup(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION routine_hook_lookup(TEXT) TO quansio_app;
