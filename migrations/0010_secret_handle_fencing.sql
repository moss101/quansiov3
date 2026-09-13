-- Secret-handle fencing and lifetimes (EXEC-007).
--
-- `secret_handles` (0001) stores the envelope-encrypted material and a status, but nothing that lets a
-- materialization already handed to a worker be *fenced*. A handle's material is delivered at an
-- approved boundary and the worker keeps it; revoking the handle must stop the worker using the
-- materialization it cached, which needs something that changes when the handle does.
--
-- `generation` is that fence, the same primitive DOMAIN.md §1.2 uses everywhere else: rotation and
-- revocation bump it, and a materialization records the generation it was issued under, so a cached
-- one is refused by one comparison rather than by a search for outstanding copies (which is not
-- knowable). The generation starts at 1 so every existing handle is at a defined value.
--
-- `expires_at` makes a handle short-lived rather than only revocable: NULL means "no scheduled expiry",
-- which is what an existing row means, and the sweep in the broker moves a handle past this instant to
-- `expired` and bumps the generation with it.
--
-- Forward-only and additive: no 0001 column is altered, and no data migration is needed because both
-- columns have a defined meaning for a row that predates them.

ALTER TABLE secret_handles
    ADD COLUMN IF NOT EXISTS generation BIGINT NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;

ALTER TABLE secret_handles
    DROP CONSTRAINT IF EXISTS secret_handles_generation_check;

ALTER TABLE secret_handles
    ADD CONSTRAINT secret_handles_generation_check CHECK (generation >= 1);

-- A handle that expires before it was created is a typo, not a policy.
ALTER TABLE secret_handles
    DROP CONSTRAINT IF EXISTS secret_handles_expiry_check;

ALTER TABLE secret_handles
    ADD CONSTRAINT secret_handles_expiry_check
    CHECK (expires_at IS NULL OR expires_at > created_at);

-- The sweep and the bounded listing both ask for the live handles of one tenant.
CREATE INDEX IF NOT EXISTS secret_handles_live_idx
    ON secret_handles (tenant_id, expires_at)
    WHERE status = 'active';
