-- CAP-003: every artifact version records the check of its source format (DOMAIN.md §10.1).
--
-- `format_check` is `{ok, rule, detail}` from the authority's validator
-- (`crates/server/src/artifacts/formats.rs`), written when the version is added. The
-- `artifact_valid` completion check reads it, so a malformed or empty source blocks
-- `run.verification_passed`. NULL for versions stored before the check existed: the check
-- treats them as unvalidated. Additive and forward-safe.

ALTER TABLE artifact_versions ADD COLUMN IF NOT EXISTS format_check JSONB;
