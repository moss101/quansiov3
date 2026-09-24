# CAP-003 reconciliation — artifact creation and editing workflows

**Canonical owners.** The task's `paths` are the intelligence side
(`python/intelligence/artifacts/`) and the skill pack (`packs/skills/artifacts/`). The
authority for artifacts is CORE-007's `crates/server/src/artifacts/` (metadata + bytes), and
`runtime/turn_loop/dispatch.rs` already names CORE-007 as the owner of the server-hosted
`artifact.create`/`artifact.update` tools (`ARTIFACT_HOST_OWNER`). The governed-tool half of
this task therefore lands there, additively; the completion gate lands in RUN-008's
`runtime/verification/` as a new deterministic check kind.

**Existing code.**
- `python/intelligence/artifacts/` (formats, models, workflow; ~1000 lines): closed source
  shapes per editable kind (document → `# Title` markdown, spreadsheet → CSV, presentation →
  slides JSON, code → files JSON, data → rows JSON), opaque-chat-blob refusal, canonical
  serialize, quality checks, an in-memory `ArtifactRecord` with versions. Classification:
  PARTIAL — the rules are real, but nothing reaches the artifact authority: it builds
  records in memory, and no governed tool carries them.
- `packs/skills/artifacts/` (skill.yaml, instructions, fixtures, evals): kept; extended.
- `config/tools.yaml` declares `artifact.create`/`artifact.update` (`content.upload`, host
  `server`) with arguments `{role, content_digest, media_type}` — no content, kind or title,
  so a call cannot carry a source file.
- The deployable's tool host is the conformance stub (`api/seams.rs`); no server-hosted tool
  executes. `HostDispatch` carries no tenant, so a server host cannot scope its writes.
- `runtime/verification`: `artifact_exists` only counts versions; nothing checks a version's
  format, so a malformed file satisfies a contract.

**Plan.**
- [ ] DOMAIN: §10.1 source formats and a per-version `format_check`; §4.4 `artifact_valid`
      check; §7.5 note on `artifact.create`/`update` arguments. `config/tools.yaml`: the two
      tools carry `kind`, `title`, `content` (UTF-8) or `content_base64`, bound to
      `content_digest` (additive, `additionalProperties: false` kept).
- [ ] Migration 0018: `artifact_versions.format_check JSONB` (`{ok, rule, detail, checked}`).
- [ ] `crates/server/src/artifacts/formats.rs`: the same closed shapes and quality checks in
      Rust (the authority re-validates; Python pre-validates); every version the store adds
      records its `format_check`.
- [ ] `crates/server/src/artifacts/tool_host.rs`: `ArtifactToolHost` (verify digest, refuse a
      malformed or low-quality source with a typed, non-retryable tool failure, create/add a
      version produced by the run, grant the acting user) and `ServerToolHost` routing
      artifact tools to it; `HostDispatch` gains `tenant_id` and `acting_user_id`;
      `ApiState::with_object_store` installs the routing host.
- [ ] `artifact_valid` deterministic check: the role's current version must have
      `format_check.ok`; a malformed upload therefore blocks `run.verification_passed`.
- [ ] Python: `intelligence/artifacts/proposals.py` builds the governed tool calls (canonical
      bytes, digest, kind, title) from structured sources; skill pack names the tools and the
      `artifact_valid` contract; a shared fixture corpus (valid + malformed) that the Rust and
      Python validators must agree on.
- [ ] Desktop: edit a text-kind artifact and save it as a new version; the version list shows
      its format check.
- [ ] Tests: fixtures (document/spreadsheet/slides/code/data) through the governed tool path
      into real PostgreSQL + MinIO; round-trip edit (v1 → v2 with parent, bytes verify);
      malformed source refused by the tool and a malformed uploaded version failing
      `artifact_valid`; Python/Rust agreement on the corpus.

**Persistent state.** `artifact_versions.format_check` (additive). No new store.

**External effects.** `content.upload` (tier 1, allow): bytes into the workspace object
store, idempotent by content digest (the store already replays identical bytes).

**Capabilities/approvals.** The tools' grant templates (`content.upload` on `artifact:*`);
the skill pack proposes and cannot grant them.

**Real boundaries.** PostgreSQL and the dev MinIO (`QUANSIO_TEST_MINIO_*`), as CORE-007's
tests use; none external.

**Rollback/recovery.** A replayed tool call with the same digest returns the same version
(content-addressed); a crash after the object write and before the metadata leaves an
unreferenced object, which the store already tolerates.
