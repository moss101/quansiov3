-- ADP-006: every version an output adapter produces records the checks it passed (DOMAIN.md 10.4 A).
--
-- `quality_record` is `{record_version, adapter, adapter_version, ok, inputs[], checks[], environment?, trust,
-- evidence_ids[]}`, written by the artifact authority in the same statement as the version, from checks the
-- authority ran itself (`crates/server/src/artifacts/outputs`). NULL for every version not made by an output tool:
-- uploads, `artifact.create` and `artifact.update` never carry one, which is what lets the deck gate tell a page
-- the authority rendered from a file somebody uploaded. Additive and forward-safe.

ALTER TABLE artifact_versions ADD COLUMN IF NOT EXISTS quality_record JSONB;
