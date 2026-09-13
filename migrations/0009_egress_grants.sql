-- Egress grants: the destinations an execution target may reach (EXEC-008).
--
-- DOMAIN.md §7.1 makes `network.egress.new_destination` a tier-2 effect with the default
-- policy "ask (grant then allow)": the first request to a destination outside the target's
-- network policy is asked for, and once approved the destination is granted so later
-- requests are allowed without asking again. That grant has to be durable, scoped and
-- bounded, which is what this table is; §8.2 already gives `execution_targets` a
-- `network_policy_id`, so the policy is the canonical `policies` row rather than a new
-- authority.
--
-- The grant's identity is the effect that authorized it. A grant *is* the settled outcome
-- of a `network.egress.new_destination` effect, so keying it by `effect_id` means one grant
-- per authorizing effect and makes re-issuing the same effect idempotent by construction,
-- which is the Effect Ledger's rule rather than a rule restated here.
--
-- `policy_version` is the fence. `policies.version` is the policy's revision, so tightening
-- a network policy increments it and every grant issued under the previous revision is
-- fenced by one comparison -- no scan, no update-everything window, and no way for a grant
-- that the new policy would refuse to survive the change. Re-granting issues a new effect.
--
-- Tenant scope is structural like every other authoritative table: RLS is enabled and
-- forced, and with no `quansio.tenant_id` context the policy matches nothing.

CREATE TABLE egress_grants (
    effect_id         TEXT PRIMARY KEY REFERENCES effect_records(id) ON DELETE CASCADE,
    tenant_id         TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    target_id         TEXT NOT NULL REFERENCES execution_targets(id) ON DELETE CASCADE,
    -- The target generation the grant was issued under. A target that was replaced is a
    -- different machine, so a grant for the old one does not carry over (DOMAIN.md §1.2).
    target_generation BIGINT NOT NULL CHECK (target_generation >= 1),
    -- The capability the grant was issued for; NULL means it is not narrowed to one.
    capability_id     TEXT,
    policy_id         TEXT NOT NULL REFERENCES policies(id) ON DELETE CASCADE,
    policy_version    INTEGER NOT NULL CHECK (policy_version >= 1),
    -- Canonical lowercase host: no scheme, no port, no trailing dot. An uppercase or
    -- dotted host is refused rather than normalized, so the stored key is the lookup key.
    host              TEXT NOT NULL CHECK (host <> '' AND host = lower(host)),
    port              INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    issued_at         TIMESTAMPTZ NOT NULL,
    expires_at        TIMESTAMPTZ NOT NULL,
    revoked_at        TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (expires_at > issued_at)
);
CREATE INDEX egress_grants_lookup_idx
    ON egress_grants (tenant_id, target_id, host, port)
    WHERE revoked_at IS NULL;

ALTER TABLE egress_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE egress_grants FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON egress_grants
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

GRANT SELECT, INSERT, UPDATE, DELETE ON egress_grants TO quansio_app;

CREATE TRIGGER egress_grants_set_updated_at
    BEFORE UPDATE ON egress_grants
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
