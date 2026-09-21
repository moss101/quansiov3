-- APP-014 teammate definition (DOMAIN.md §3 Teammate). The build item names a model route
-- preference, a default execution target and an availability beside the persona, standing
-- instructions, default capability needs and memory scope CORE-001 already carries; each is
-- a preference the definition records and the run reads — never a grant. `version` is
-- bumped by every configuration change so a run's capability projection input names the
-- definition it was resolved from. Additive and forward-safe.

ALTER TABLE teammates
    ADD COLUMN IF NOT EXISTS template_id                 TEXT,
    ADD COLUMN IF NOT EXISTS model_route_preference      TEXT,
    ADD COLUMN IF NOT EXISTS default_execution_target_id TEXT,
    ADD COLUMN IF NOT EXISTS availability                TEXT NOT NULL DEFAULT 'available',
    ADD COLUMN IF NOT EXISTS version                     INTEGER NOT NULL DEFAULT 1;

ALTER TABLE teammates
    DROP CONSTRAINT IF EXISTS teammates_availability_check;

ALTER TABLE teammates
    ADD CONSTRAINT teammates_availability_check CHECK (availability IN ('available', 'away'));
