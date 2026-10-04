-- Skill recording sessions (ADP-003, DOMAIN.md 11.7).
--
-- One row is one person's demonstration, from the explicit start to the explicit finish (or its
-- cancellation, abort or expiry). It is recorder-session state owned by control/skills/recording and
-- nothing more: the archive's bytes are an Artifact (artifacts/artifact_versions, owner-only grant), the
-- skill the person submits is a skill_versions row (DRAFT), and the compiled proposal under review is the
-- one JSONB column here, rebuildable by compiling the archive again.
--
-- The owner fence lives in the row: `owner_user_id`, the session family the recording started under and
-- the `epoch` the desktop advances whenever the signed-in account changes. A call from any other user,
-- family or epoch is RECORDING_OWNER_CHANGED and aborts a `recording` (the store, not this file, decides).

CREATE TABLE skill_recordings (
    id                  TEXT PRIMARY KEY CHECK (id LIKE 'rec\_%'),
    tenant_id           TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id        TEXT NOT NULL,
    owner_user_id       TEXT NOT NULL REFERENCES users(id),
    -- NULL only where scope is established by trusted headers (development harnesses): there is no
    -- session to bind. A session-authenticated start always records its family.
    session_family_id   TEXT,
    epoch               BIGINT NOT NULL CHECK (epoch >= 1),
    status              TEXT NOT NULL DEFAULT 'recording'
                        CHECK (status IN ('recording', 'finished', 'compiled', 'submitted', 'cancelled', 'aborted')),
    start_key           TEXT NOT NULL CHECK (length(start_key) BETWEEN 1 AND 128),
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at          TIMESTAMPTZ NOT NULL,
    finished_at         TIMESTAMPTZ,
    artifact_id         TEXT CHECK (artifact_id IS NULL OR artifact_id LIKE 'art\_%'),
    artifact_version_id TEXT CHECK (artifact_version_id IS NULL OR artifact_version_id LIKE 'artv\_%'),
    content_digest      TEXT CHECK (content_digest IS NULL OR content_digest ~ '^[0-9a-f]{64}$'),
    size_bytes          BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    -- {rule_set, counts{api_key, bearer_token, card_number, verification_code, password_field},
    --  screenshots_withheld}: counts only, never a value.
    redaction           JSONB CHECK (redaction IS NULL OR jsonb_typeof(redaction) = 'object'),
    stats               JSONB CHECK (stats IS NULL OR jsonb_typeof(stats) = 'object'),
    proposal            JSONB CHECK (proposal IS NULL OR jsonb_typeof(proposal) = 'object'),
    skill_version_id    TEXT CHECK (skill_version_id IS NULL OR skill_version_id LIKE 'sklv\_%'),
    abort_reason        TEXT CHECK (abort_reason IS NULL OR abort_reason IN ('owner_changed', 'expired')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    -- A repeated start key is the same recording (idempotent start), per owner.
    UNIQUE (tenant_id, owner_user_id, start_key),
    -- finished and later states carry the stored archive; recording/cancelled/aborted never do.
    CHECK ((status IN ('finished', 'compiled', 'submitted'))
           = (artifact_id IS NOT NULL AND artifact_version_id IS NOT NULL AND content_digest IS NOT NULL)),
    CHECK ((status = 'aborted') = (abort_reason IS NOT NULL)),
    CHECK ((status = 'submitted') = (skill_version_id IS NOT NULL)),
    CHECK (status NOT IN ('compiled', 'submitted') OR proposal IS NOT NULL),
    FOREIGN KEY (tenant_id, workspace_id) REFERENCES workspaces (tenant_id, id)
);

-- At most one recording in progress per person: a second start with another key is CONFLICT_STATE.
CREATE UNIQUE INDEX skill_recordings_one_active_per_owner
    ON skill_recordings (tenant_id, owner_user_id) WHERE status = 'recording';

CREATE INDEX skill_recordings_owner_idx
    ON skill_recordings (tenant_id, owner_user_id, created_at DESC);

ALTER TABLE skill_recordings ENABLE ROW LEVEL SECURITY;
ALTER TABLE skill_recordings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON skill_recordings
    USING (tenant_id = current_setting('quansio.tenant_id', true))
    WITH CHECK (tenant_id = current_setting('quansio.tenant_id', true));

GRANT SELECT, INSERT, UPDATE, DELETE ON skill_recordings TO quansio_app;

CREATE TRIGGER skill_recordings_set_updated_at
    BEFORE UPDATE ON skill_recordings
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
