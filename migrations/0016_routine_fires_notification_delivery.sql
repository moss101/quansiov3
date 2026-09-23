-- APP-010: routines fire once per window, and notifications are delivered per channel.
--
-- `routine_fires` is the firing ledger (DOMAIN.md §13.1): one row per fire key
-- (`routine:<id>:<window>`, `routine:<id>:event:<event id>`, `routine:<id>:manual:<command
-- id>`), claimed in the transaction that advances the routine's `next_due_at`, leased while
-- it executes, and retried with backoff when execution fails. The unique key is what makes
-- a restart, a replayed event or a second scheduler instance claim the same fire once.
--
-- `notification_deliveries` holds one row per (notification, channel) the server delivers
-- beyond the in-app row itself — email today — with its attempts and backoff.
-- `notifications.source_event_id` names the RuntimeEvent a notification was made from, so
-- consuming the same event twice notifies once. Additive and forward-safe.

CREATE TABLE routine_fires (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'rfi\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id     TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    routine_id       TEXT NOT NULL REFERENCES routines(id) ON DELETE CASCADE,
    fire_key         TEXT NOT NULL,
    source           TEXT NOT NULL CHECK (source IN ('schedule', 'event', 'manual')),
    window_at        TIMESTAMPTZ,
    source_event_id  TEXT,
    status           TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'running', 'fired', 'failed')),
    attempts         INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until      TIMESTAMPTZ,
    last_error       TEXT,
    thread_id        TEXT,
    message_id       TEXT,
    objective_id     TEXT,
    run_id           TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, fire_key)
);
CREATE INDEX routine_fires_pending_idx ON routine_fires (tenant_id, next_attempt_at)
    WHERE status IN ('pending', 'running');
CREATE INDEX routine_fires_routine_idx ON routine_fires (tenant_id, routine_id, created_at DESC);

ALTER TABLE routine_fires ENABLE ROW LEVEL SECURITY;
ALTER TABLE routine_fires FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON routine_fires
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON routine_fires TO quansio_app;
CREATE TRIGGER routine_fires_set_updated_at
    BEFORE UPDATE ON routine_fires
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE notification_deliveries (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'ndl\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    notification_id  TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
    channel          TEXT NOT NULL CHECK (channel IN ('email', 'webhook')),
    status           TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'delivered', 'failed', 'unavailable')),
    attempts         INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error       TEXT,
    delivered_at     TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (notification_id, channel)
);
CREATE INDEX notification_deliveries_due_idx ON notification_deliveries (tenant_id, next_attempt_at)
    WHERE status = 'pending';

ALTER TABLE notification_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE notification_deliveries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON notification_deliveries
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON notification_deliveries TO quansio_app;
CREATE TRIGGER notification_deliveries_set_updated_at
    BEFORE UPDATE ON notification_deliveries
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

ALTER TABLE notifications ADD COLUMN IF NOT EXISTS source_event_id TEXT;
ALTER TABLE notifications ADD COLUMN IF NOT EXISTS title TEXT;
-- The channels the user's preference selected when the notification was made; 
-- `channels_delivered` records the ones that were delivered.
ALTER TABLE notifications ADD COLUMN IF NOT EXISTS channels JSONB NOT NULL DEFAULT '[]'::jsonb;
CREATE UNIQUE INDEX IF NOT EXISTS notifications_source_idx
    ON notifications (tenant_id, source_event_id, user_id, kind)
    WHERE source_event_id IS NOT NULL;

-- The background loop (scheduler, routine triggers, notifier, email) serves every tenant,
-- but `tenants` is readable only for the active tenant (0001). This function returns the
-- tenant ids and nothing else, so the loop can set each tenant's context in turn without
-- the application role being able to read any other tenant's rows.
CREATE OR REPLACE FUNCTION background_tenants() RETURNS SETOF TEXT
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public
    AS $$ SELECT id FROM tenants ORDER BY id $$;
REVOKE ALL ON FUNCTION background_tenants() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION background_tenants() TO quansio_app;
