-- APP-002 layered settings. Security-relevant keys merge most-restrictively.
-- No new identity prefix: settings live on the existing user/tenant/workspace rows.

ALTER TABLE users
    ADD COLUMN IF NOT EXISTS settings JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS settings JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE workspaces
    ADD COLUMN IF NOT EXISTS settings JSONB NOT NULL DEFAULT '{}'::jsonb;
