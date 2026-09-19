-- APP-002 baseline authentication (DOMAIN.md §2): passwordless email link, OAuth sign-in,
-- and sessions made of a short-lived ES256 access token plus an opaque rotating refresh
-- token. Tokens are stored hashed; a session belongs to a rotation family so a reused
-- refresh token revokes every descendant at once. Additive and forward-safe.

-- Magic-link challenges are pre-authentication: no tenant scope yet.
CREATE TABLE auth_challenges (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'ach\_%'),
    email        TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    token_hash   TEXT NOT NULL UNIQUE,
    expires_at   TIMESTAMPTZ NOT NULL,
    consumed_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX auth_challenges_expiry_idx ON auth_challenges (expires_at);

-- OAuth authorization requests in flight: the PKCE verifier never leaves the server.
CREATE TABLE oauth_states (
    state         TEXT PRIMARY KEY,
    provider      TEXT NOT NULL CHECK (provider IN ('google', 'microsoft', 'github')),
    pkce_verifier TEXT NOT NULL,
    redirect_uri  TEXT NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    consumed_at   TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    id                 TEXT PRIMARY KEY CHECK (id LIKE 'ses\_%'),
    user_id            TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tenant_id          TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    family_id          TEXT NOT NULL,
    client             TEXT NOT NULL DEFAULT 'desktop'
                       CHECK (client IN ('desktop', 'web', 'cli')),
    auth_method        TEXT NOT NULL,
    refresh_hash       TEXT NOT NULL UNIQUE,
    refresh_expires_at TIMESTAMPTZ NOT NULL,
    rotated_from       TEXT REFERENCES sessions(id),
    rotated_at         TIMESTAMPTZ,
    revoked_at         TIMESTAMPTZ,
    revoked_reason     TEXT,
    last_used_at       TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX sessions_user_idx ON sessions (user_id, created_at DESC);
CREATE INDEX sessions_family_idx ON sessions (family_id);

-- Row-level security, forced like every other table. Challenges and OAuth requests exist
-- before any tenant is known, and a refresh or bearer lookup happens before the tenant
-- context is set, so the session authority opens a transaction-local gate
-- (`quansio.auth_flow = 'on'`) for its own statements; nothing else sets it. Sessions are
-- additionally readable under the ordinary tenant isolation once the scope is resolved.
ALTER TABLE auth_challenges ENABLE ROW LEVEL SECURITY;
ALTER TABLE auth_challenges FORCE ROW LEVEL SECURITY;
CREATE POLICY auth_flow ON auth_challenges
    USING (current_setting('quansio.auth_flow', true) = 'on')
    WITH CHECK (current_setting('quansio.auth_flow', true) = 'on');

ALTER TABLE oauth_states ENABLE ROW LEVEL SECURITY;
ALTER TABLE oauth_states FORCE ROW LEVEL SECURITY;
CREATE POLICY auth_flow ON oauth_states
    USING (current_setting('quansio.auth_flow', true) = 'on')
    WITH CHECK (current_setting('quansio.auth_flow', true) = 'on');

ALTER TABLE sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE sessions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON sessions
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));
CREATE POLICY auth_flow ON sessions
    USING (current_setting('quansio.auth_flow', true) = 'on')
    WITH CHECK (current_setting('quansio.auth_flow', true) = 'on');

GRANT SELECT, INSERT, UPDATE, DELETE ON auth_challenges, oauth_states, sessions TO quansio_app;
