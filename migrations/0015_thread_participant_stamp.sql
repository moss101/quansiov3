-- APP-009: thread membership changes are updates (a participant leaves, rejoins or changes
-- role). 0001 attached the shared `set_updated_at` trigger to `thread_participants` but
-- gave the table no `updated_at` column, so every UPDATE failed inside the trigger. The
-- column is added with the default every other stamped table has. Additive and forward-safe:
-- existing rows take `now()`.

ALTER TABLE thread_participants
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE INDEX IF NOT EXISTS thread_participants_active_idx
    ON thread_participants (tenant_id, thread_id)
    WHERE left_at IS NULL;
