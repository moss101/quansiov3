-- Enterprise identity (OPS-001, DOMAIN.md §2): per-tenant OIDC single sign-on, the server-side
-- group -> role mapping, external identities, SCIM directory groups, and the SSO authorization
-- requests in flight. Additive: no existing column changes meaning.

-- One OIDC connection per tenant. The client secret is a custody handle, never material.
CREATE TABLE sso_connections (
    tenant_id            TEXT PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    protocol             TEXT NOT NULL DEFAULT 'oidc' CHECK (protocol IN ('oidc')),
    issuer               TEXT NOT NULL,
    client_id            TEXT NOT NULL,
    client_secret_handle TEXT,
    groups_claim         TEXT NOT NULL DEFAULT 'groups',
    default_workspace_id TEXT REFERENCES workspaces(id) ON DELETE SET NULL,
    enforced             BOOLEAN NOT NULL DEFAULT FALSE,
    status               TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- External group -> internal role. Only these rows give an SSO or SCIM user a role; `owner` is
-- never mapped.
CREATE TABLE sso_group_mappings (
    tenant_id      TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    external_group TEXT NOT NULL CHECK (length(external_group) BETWEEN 1 AND 512),
    -- '' is the tenant-level row; otherwise the ws_ the workspace role applies to.
    scope          TEXT NOT NULL DEFAULT '',
    tenant_role    TEXT CHECK (tenant_role IN ('admin', 'billing', 'member')),
    workspace_role TEXT CHECK (workspace_role IN ('admin', 'editor', 'approver', 'viewer')),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, external_group, scope),
    CHECK ((scope = '' AND tenant_role IS NOT NULL AND workspace_role IS NULL)
        OR (scope LIKE 'ws\_%' AND workspace_role IS NOT NULL AND tenant_role IS NULL))
);

-- A user as the tenant's IdP knows them: the SSO subject once linked, the SCIM externalId,
-- the groups last asserted, and whether this tenant provisioned the account.
CREATE TABLE external_identities (
    tenant_id   TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    issuer      TEXT,
    subject     TEXT,
    external_id TEXT,
    origin      TEXT NOT NULL CHECK (origin IN ('provisioned', 'linked')),
    sso_groups  JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id),
    CHECK ((issuer IS NULL) = (subject IS NULL))
);
CREATE UNIQUE INDEX external_identities_subject_idx
    ON external_identities (tenant_id, issuer, subject) WHERE subject IS NOT NULL;
CREATE UNIQUE INDEX external_identities_external_id_idx
    ON external_identities (tenant_id, external_id) WHERE external_id IS NOT NULL;

-- SCIM groups (DirectoryGroup, dgr_) and their members.
CREATE TABLE directory_groups (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'dgr\_%'),
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 512),
    external_id  TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, display_name)
);
CREATE TABLE directory_group_members (
    tenant_id  TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    group_id   TEXT NOT NULL REFERENCES directory_groups(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, user_id)
);
CREATE INDEX directory_group_members_user_idx ON directory_group_members (tenant_id, user_id);

-- SSO authorization requests in flight: state and nonce are stored hashed, the PKCE verifier
-- never leaves the server, and each row is consumed once.
CREATE TABLE sso_states (
    state_hash    TEXT PRIMARY KEY,
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    nonce_hash    TEXT NOT NULL,
    pkce_verifier TEXT NOT NULL,
    redirect_uri  TEXT NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    consumed_at   TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX sso_states_expiry_idx ON sso_states (expires_at);

-- A service principal key names its principal, so authentication reads one row before the
-- tenant is known.
CREATE INDEX service_principals_tenant_idx ON service_principals (tenant_id, created_at DESC);

-- Row-level security. The tenant-scoped tables follow tenant isolation. The two that
-- authentication must read before any tenant context exists (a principal key's row, an SSO
-- request) also admit the session authority's transaction-local `quansio.auth_flow` gate.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY['sso_connections', 'sso_group_mappings', 'external_identities',
                             'directory_groups', 'directory_group_members', 'sso_states'] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I '
            'USING (tenant_id = current_setting(''quansio.tenant_id'', true)) '
            'WITH CHECK (tenant_id = current_setting(''quansio.tenant_id'', true))', t);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO quansio_app', t);
    END LOOP;
    FOREACH t IN ARRAY ARRAY['sso_connections', 'sso_group_mappings', 'external_identities',
                             'directory_groups'] LOOP
        EXECUTE format(
            'CREATE TRIGGER %I_set_updated_at BEFORE UPDATE ON %I '
            'FOR EACH ROW EXECUTE FUNCTION set_updated_at()', t, t);
    END LOOP;
END $$;

CREATE POLICY auth_flow ON sso_states
    USING (current_setting('quansio.auth_flow', true) = 'on')
    WITH CHECK (current_setting('quansio.auth_flow', true) = 'on');
CREATE POLICY auth_flow ON service_principals
    USING (current_setting('quansio.auth_flow', true) = 'on')
    WITH CHECK (current_setting('quansio.auth_flow', true) = 'on');
