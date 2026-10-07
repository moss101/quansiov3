-- ADP-008: who proposed a tool call, content-hash pack revisions and redacted install reports (DOMAIN.md 7.5, 11.6).
--
--   * `tool_calls.caller` records whether a model proposed the call or the host made it. A call to a tool that is not
--     model-visible is rejected and recorded as the model's; a host call that parks for approval resumes as a host call.
--   * `capability_pack_versions` gains the pack's content-hash `revision` (sha256 of its canonical manifest and files),
--     the content itself (`manifest`, `files`) so a retained previous revision can be re-activated, and when it was
--     activated and retired. The revision, not the semver, is a version's identity, so 0001's unique (pack_id, semver) goes.
--     A revision that is no longer retained keeps its row (history) with its content cleared.
--   * `pack_install_reports` holds one redacted, bounded report per install attempt, keyed by the command id that asked.
--     Reports expire (`expires_at`); expired ones are purged in the same transaction as the next install or read, the
--     repository's lazy sweep (no timer, no daemon).
-- All of it belongs to the pack owner (`crates/server/src/control/packs`) except the `tool_calls` column, which the tool
-- dispatch owner writes. Additive and forward-safe.

ALTER TABLE tool_calls
    ADD COLUMN caller TEXT NOT NULL DEFAULT 'model' CHECK (caller IN ('model', 'host'));

ALTER TABLE capability_pack_versions DROP CONSTRAINT capability_pack_versions_pack_id_semver_key;
ALTER TABLE capability_pack_versions
    ADD COLUMN revision     TEXT CHECK (revision ~ '^sha256:[0-9a-f]{64}$'),
    ADD COLUMN manifest     JSONB,
    ADD COLUMN files        JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN activated_at TIMESTAMPTZ,
    ADD COLUMN retired_at   TIMESTAMPTZ;
CREATE UNIQUE INDEX capability_pack_versions_revision_idx
    ON capability_pack_versions (pack_id, revision) WHERE revision IS NOT NULL;

CREATE TABLE pack_install_reports (
    tenant_id            TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    request_id           TEXT NOT NULL,
    pack_name            TEXT NOT NULL,
    pack_id              TEXT,
    revision             TEXT CHECK (revision IS NULL OR revision ~ '^sha256:[0-9a-f]{64}$'),
    previous_revision    TEXT,
    state                TEXT NOT NULL CHECK (state IN ('running', 'succeeded', 'failed')),
    phase                TEXT NOT NULL CHECK (phase IN ('fetch', 'verify', 'compile', 'activate', 'done')),
    outcome              TEXT CHECK (outcome IN ('activated', 'retained_previous', 'failed')),
    diagnostics          JSONB NOT NULL DEFAULT '[]'::jsonb,
    diagnostics_omitted  INTEGER NOT NULL DEFAULT 0 CHECK (diagnostics_omitted >= 0),
    started_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at          TIMESTAMPTZ,
    expires_at           TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id, request_id),
    -- A report that is still running has no outcome; a finished one always has.
    CHECK ((state = 'running') = (outcome IS NULL AND finished_at IS NULL))
);
CREATE INDEX pack_install_reports_recent_idx ON pack_install_reports (tenant_id, started_at DESC);
CREATE INDEX pack_install_reports_expiry_idx ON pack_install_reports (tenant_id, expires_at);

ALTER TABLE pack_install_reports ENABLE ROW LEVEL SECURITY;
ALTER TABLE pack_install_reports FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON pack_install_reports
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON pack_install_reports TO quansio_app;
