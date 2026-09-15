# CAP-003 reconciliation

- Canonical owner: `python/intelligence/artifacts/` (propose versioned sources) and
  `packs/skills/artifacts/` (skill/tool procedure). Rust `crates/server/artifacts`
  remains the metadata/object-store authority (CORE-007). Desktop preview is APP-008.
- Existing code: `python/intelligence/artifacts/__init__.py` was a stub. APP-008
  desktop workspace is projection-only. CORE-007 already stores ArtifactVersion.
- Coverage: GENUINE_GAP for the intelligence workflow + skill pack.
- Persistent state: none in this plane. Completion is a proposal; runtime commits.
- Contracts: DOMAIN.md §10.1 Artifact/ArtifactVersion, §7.5 `artifact.create` /
  `artifact.update`. No new fields.
- External effects: none (`real_boundary=false`).
- Capabilities: skill `tool_needs` are `artifact.create`/`artifact.update`; resolution
  cannot grant them.
- Migrations: none.
- Real boundaries: none. PASS is forbidden while APP-008 is BLOCKED_EXTERNAL
  (INT-002 chain).
- Rollback: versions are append-only; a failed completion leaves the last valid
  version current.

## Plan

1. Versioned source model (kind, digest, seq, parent_version_id).
2. Format validators + canonical round-trip for document/spreadsheet/slides/code.
3. Workflow create/edit/preview/complete; format failure blocks completion.
4. Skill pack + fixtures.
5. Tests named in the task: fixtures, round-trip edit, malformed artifact.
