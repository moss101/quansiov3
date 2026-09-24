# CAP-003 — artifact creation and editing workflows

Ownership, existing code and the plan: `evidence/CAP-003/RECONCILIATION.md`. The Python
workflow's rules were real but reached nothing: records lived in memory, the tools could not
carry a source, no server host executed them, and no completion check looked at a version's
format.

## What was built

- **Governed tools.** `config/tools.yaml`: `artifact.create {role, kind, title, content |
  content_base64, content_digest, media_type?}` and `artifact.update {artifact_id, content |
  content_base64, content_digest, media_type?}`, returning `{artifact_id, version_id, version,
  content_digest, replayed}`. `crates/server/src/artifacts/tool_host.rs`: `ArtifactToolHost`
  (digest-bound source, the authority's format + quality check, create/add version produced by
  the run and step, granted to the acting user) and `ServerToolHost` (artifact tools on the
  server, the rest to the wrapped host), installed by `ApiState::with_object_store`.
  `HostDispatch` gains `tenant_id` and `acting_user_id`.
- **Source formats in the authority.** `crates/server/src/artifacts/formats.rs` — the closed
  shapes of DOMAIN §10.1 (document, spreadsheet, presentation, code, data, media) with quality
  checks; every version the store adds records `format_check` (migration 0018). Uploads are
  stored either way; the tools refuse an invalid source.
- **Completion gate.** `artifact_valid` deterministic check (contract parser, proto
  `KIND_ARTIFACT_VALID`, checks): the role's current version must have `format_check.ok`.
- **Intelligence plane.** `python/intelligence/artifacts/proposals.py` (`create_call`,
  `update_call`: canonical bytes, digest, pre-check); `check_source`; code-path safety rule on
  both sides; the shared corpus `packs/skills/artifacts/fixtures/corpus.yaml` (5 valid kinds,
  11 malformed/mislabelled entries) with the skill instructions updated.
- **Desktop.** Each version shows its source check; a text source (document, CSV,
  slides/code/data JSON) is edited in place and saved as the next version through the existing
  upload channel (`version.created` selects it). CSV/TSV uploads are inferred as `spreadsheet`
  (were `data`, which would have recorded a failed check for every CSV).
- **DOMAIN:** §4.4 `artifact_valid`, §10.1 source formats and `format_check`, §7.3 unchanged
  here.

## Acceptance → tests

- *Artifacts are editable/versioned and not opaque chat blobs.*
  `crates/server/tests/artifact_workflows.rs::a_run_creates_and_edits_a_versioned_artifact_through_the_governed_tools`
  (a run's `artifact.create` then `artifact.update`: two immutable versions, v2's parent v1,
  bytes read back digest-verified from MinIO, readable by the acting user, origin `run`);
  `every_editable_kind_is_made_through_the_tool_from_its_fixture` (document, spreadsheet,
  slides, code, data fixtures); the chat-transcript fixture is refused (`artifact.opaque_blob`)
  on both sides; the desktop E2E edits a CSV into v2 with its parent and check.
- *Failed format validation blocks completion.* The same run's malformed edit and digest
  mismatch are refused with no version created, and the run completes only through
  `artifact_exists` + `artifact_valid`;
  `a_malformed_deliverable_blocks_completion_until_a_valid_version_is_added` (an uploaded
  ragged CSV fails verification with `artifact.format`; a valid version through the tool then
  lets the claim pass); Python `test_a_malformed_source_is_never_proposed`.
- Required tests: document/spreadsheet/slide fixtures (above + Python corpus), round-trip edit
  test (Rust run, Python `test_an_edit_round_trips_to_a_new_version_call`, desktop E2E),
  malformed artifact test (corpus on both sides, host refusal, blocked completion).
- The Rust and Python validators agree on every corpus entry
  (`the_source_corpus_verdicts_are_the_same_as_the_intelligence_planes`,
  `test_the_corpus_verdicts_are_the_authoritys`).

## Negative checks

`mutation-checks.log`: each guard removed in a scratch edit; the named test fails every time.

## Full CI

`bash scripts/ci/ci.sh` after merging main (APP-001's command-record fixes), with
`QUANSIO_TEST_SERVER_BIN`/`QUANSIO_TEST_SEED_BIN` pinned to copies of this branch's binaries:
11/11 gates PASS (`ci-summary.json`). Earlier runs: one failed on an eslint template-literal
nit in the new E2E step (fixed in 228edd4); one failed the desktop E2E because another
session's worktree builds with `CARGO_TARGET_DIR` set to this checkout's `target/` and had
replaced `target/debug/quansio-server` with its own branch mid-run (it still inferred CSV as
`data`) — hence the pinned copies. The same condition explains an earlier APP-015 SDK-compat
failure.

## Found on the way

- Upload kind inference mapped `text/csv` to `data`, whose source is JSON — every uploaded CSV
  would have recorded a failed check; CSV/TSV are now `spreadsheet` sources.
- The disk filled (171 MB free) during the mutation runs; `target/debug/incremental` was pruned.
