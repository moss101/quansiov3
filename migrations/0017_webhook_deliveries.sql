-- APP-015: outbound webhook delivery (DOMAIN.md §13.5).
--
-- `webhook_subscriptions` (0001, row-level security and the updated_at trigger already
-- applied there) gains who created it and the event sequence it starts
-- after, so a new subscription receives events committed after it and never the backlog.
-- `webhook_deliveries` is the delivery ledger: one row per (subscription, event), enqueued
-- in the transaction that advances the fan-out cursor, so a replayed batch, a restart or two
-- instances scanning at once enqueue an event once. A delivery is leased while an attempt is
-- in flight and retried with backoff under the same event id (consumers dedupe on it) until
-- it is delivered or dead-lettered. Additive and forward-safe.

ALTER TABLE webhook_subscriptions ADD COLUMN IF NOT EXISTS created_by TEXT;
ALTER TABLE webhook_subscriptions ADD COLUMN IF NOT EXISTS start_sequence BIGINT NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS webhook_subscriptions_tenant_idx
    ON webhook_subscriptions (tenant_id, created_at);

CREATE TABLE webhook_deliveries (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'whd\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    subscription_id  TEXT NOT NULL REFERENCES webhook_subscriptions(id) ON DELETE CASCADE,
    workspace_id     TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    event_id         TEXT NOT NULL,
    event_type       TEXT NOT NULL,
    event_sequence   BIGINT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'delivering', 'delivered', 'dead_lettered')),
    attempts         INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until      TIMESTAMPTZ,
    last_status      TEXT,
    last_attempt_at  TIMESTAMPTZ,
    delivered_at     TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (subscription_id, event_id)
);
CREATE INDEX webhook_deliveries_due_idx ON webhook_deliveries (tenant_id, next_attempt_at)
    WHERE status IN ('pending', 'delivering');
CREATE INDEX webhook_deliveries_subscription_idx
    ON webhook_deliveries (tenant_id, subscription_id, event_sequence DESC);

ALTER TABLE webhook_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE webhook_deliveries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON webhook_deliveries
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON webhook_deliveries TO quansio_app;
CREATE TRIGGER webhook_deliveries_set_updated_at
    BEFORE UPDATE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
