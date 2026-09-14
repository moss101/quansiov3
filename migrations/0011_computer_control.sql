-- Durable native-computer input ownership and fencing (EXEC-010, DOMAIN.md §8.6).
--
-- One row belongs to one ExecutionTarget. It is deliberately control state rather than a
-- second session/runtime: the target remains machine authority, the ToolCall/EffectRecord
-- remains effect authority, and this row only answers whether native input may cross the
-- privileged bridge at this instant. A row lock serializes begin/finish with takeover and
-- handback, while `generation` keeps a caller that survived a control change stale.
--
-- `active_tool_call_id` is retained across restart. Recovery must reconcile that ToolCall's
-- EffectRecord before clearing it; a restart never guesses that native input did not land.

CREATE TABLE computer_controls (
    target_id           TEXT PRIMARY KEY REFERENCES execution_targets(id) ON DELETE CASCADE,
    tenant_id           TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id              TEXT REFERENCES runs(id) ON DELETE SET NULL,
    holder              TEXT NOT NULL DEFAULT 'agent'
                        CHECK (holder IN ('agent', 'user', 'none')),
    generation          BIGINT NOT NULL DEFAULT 1 CHECK (generation >= 1),
    takeover_pending    BOOLEAN NOT NULL DEFAULT FALSE,
    active_tool_call_id TEXT,
    control_since       TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (active_tool_call_id IS NULL OR active_tool_call_id LIKE 'tc\_%'),
    CHECK (NOT takeover_pending OR holder = 'agent'),
    UNIQUE (tenant_id, target_id)
);

CREATE INDEX computer_controls_run_idx ON computer_controls (tenant_id, run_id)
    WHERE run_id IS NOT NULL;

ALTER TABLE computer_controls ENABLE ROW LEVEL SECURITY;
ALTER TABLE computer_controls FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON computer_controls
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

GRANT SELECT, INSERT, UPDATE, DELETE ON computer_controls TO quansio_app;

CREATE TRIGGER computer_controls_set_updated_at
    BEFORE UPDATE ON computer_controls
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
