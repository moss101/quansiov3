# APP-011 — Memory, knowledge, skills and capability settings UI

## What was built (task branch `task/APP-011-knowledge-ui`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
52 lines of predicates over an invented entry shape, with no projection, no command and
no UI. The stale branch of that era is parked as `stale/APP-011-knowledge-ui-2026-09-15`.
The ownership decision — why a person's "forget this" is a Rust data-lifecycle control
and not a second lifecycle owner — is recorded in `evidence/APP-011/RECONCILIATION.md`.

### Server

- `crates/server/src/control/data_lifecycle/intelligence.rs` — the entry-level forgetting
  path (the §21.3 deletion workflow OPS-002 owns). `DataLifecycle::{delete_knowledge,
  supersede_knowledge, delete_memory}` apply only the §11.4 tombstone edges (`→ deleted`
  from every non-deleted state; `→ superseded` from `verified|active`, naming an existing
  successor), purge the entry's `derived.embeddings` rows (source kinds `knowledge_entry`
  / `memory_entry`, exactly as the plane keys them) and stage `knowledge.deleted`,
  `knowledge.superseded`, `memory.deleted` — in one transaction, so tombstone, purge and
  event cannot disagree. The row stays; provenance and audit survive. An entry already in
  the state is the idempotent outcome (`already: true`; the purge still runs). A refused
  edge is `CONFLICT_STATE` naming both ends; an unknown entry `NOT_FOUND`; a malformed
  id or a self-successor `VALIDATION_SCHEMA`. The Python models remain the lifecycle
  authority: `python/tests/intelligence/test_data_lifecycle_edges.py` reads the Rust
  edge tables and source kinds and pins them to `knowledge/models.py::TRANSITIONS`,
  `memory/models.py::TRANSITIONS`, `SOURCE_KIND_KNOWLEDGE`, `SOURCE_KIND_MEMORY`, and
  asserts the control writes no status but the two tombstones.
- `crates/server/src/api/knowledge.rs` — `GET /v1/knowledge?workspace_id[&status]
  [&include_deleted]` (provenance, scope, kind, content, confidence, version, lifecycle
  state, successor, `retrievable` = ACTIVE), `GET /v1/memory?workspace_id[&scope]
  [&subject_ref][&include_deleted]` (scope, subject, content, provenance kind/ref,
  confidence, state, `retrievable` = ACTIVE and unexpired by the database clock; the
  statement names only `memory_entries` and a unit test pins that — memory is not
  recovery), `GET /v1/skills` (every version with its ladder position, whether it
  resolves — ACTIVE *and* named by the skill's pointer — and `promotable_to`, the edges
  the ladder allows). Commands: `DeleteKnowledge`, `SupersedeKnowledge`, `DeleteMemory`
  through the control above; `InstallSkill` (a skill with one DRAFT version; installing
  never activates), `PromoteSkillVersion` (one rung; the ladder refuses the rest with
  `RUNTIME_ILLEGAL_TRANSITION`; a second ACTIVE version is `CONFLICT_STATE`),
  `DisableSkill` (ACTIVE → DEPRECATED; no active version is `CONFLICT_STATE`). The API
  module still writes only `commands`.
- `GET /v1/runs/{id}` now carries `capability_projection` — the task-scoped resolution:
  inputs in the fixed layer order, grants, digest, expiry. A seeded projection records its
  inputs (the tenant/workspace policies as `tenant_policy`/`workspace_policy`, the ACTIVE
  skill versions as one `active_skills` input); skills are an input that contributes no
  grant, proven by the surface test (a manifest that needs `fs.write.workspace` leaves the
  run with `read.internal` only).
- `quansio-dev-seed --intelligence` seeds three knowledge entries (active, verified,
  candidate), one active memory entry and a derived embedding for each entry in force,
  shaped as the plane writes them, for harnesses.

### Desktop (`apps/desktop/src/knowledge/`)

- `entries.ts` — decoders with provenance, scope and lifecycle; retrievability is the
  server's word, never inferred from status; `decodeMemory` refuses a row that carries a
  field outside `MEMORY_FIELDS` or any recovery name (checkpoint, cursor, generation,
  lease, protocol_state, resume_token, attempt, effect_id…) and reports it; the controls
  (`deleteKnowledge`, `supersedeKnowledge`, `deleteMemory`) refuse an edge the ladder
  does not have before any command is sent, and the edge tables are pinned to the Rust
  source by the unit test.
- `skills.ts` — decoder, `promotion()` bound to the server's `promotable_to`,
  `activation()` that refuses anything but APPROVED → ACTIVE, `disable()`, `install()`
  (validated draft; never active); the ladder spelling is pinned to the Rust state
  machine.
- `capabilities.ts` — the run's resolution laid out over all ten layers in order, absent
  ones included; `skillInputs()` names the versions the resolution took; the layer order
  is pinned to `crates/capability/src/layer.rs`.
- Bridge: `knowledge.list/delete/supersede`, `memory.list/delete`,
  `skills.list/install/promote/disable`, `runs.capabilities`; mutations return refusals as
  data. Renderer `Knowledge.tsx`: knowledge with provenance and tombstones, semantic
  memory in its own section labelled as not recovery (refused rows counted), skills with
  only the next legal rung offered and "Activate" disabled unless approved, the install
  form, and the capability resolution table with grants; the shell's `knowledge-controls`
  surface.

## Acceptance

1. **Deleted memory/knowledge disappears from future retrieval** — `knowledge.e2e.test.ts`
   against the real server: after `DeleteMemory` the memory is absent from `GET /v1/memory`
   and `retrievable()` is empty; after `DeleteKnowledge` the entry is absent from
   `GET /v1/knowledge`, gone from `GET /v1/search` (knowledge channel), and its
   `derived.embeddings` row is removed (`derived_rows_removed: 1`); after
   `SupersedeKnowledge` the same holds for the superseded entry; each tombstone is still
   readable with `include_deleted=true`, with its provenance and `retrievable: false`, and
   its event (`memory.deleted`, `knowledge.deleted`, `knowledge.superseded`) reaches the
   workspace stream. `knowledge_surface.rs` proves the same at the API and additionally
   counts the derived rows and the staged events directly. The plane's own retrieval
   re-checks every hit against the fabric (INT-007/INT-006), so the status flip is what
   removes the entry there; the purge is what removes it from the semantic channel.
2. **Unapproved skill/capability cannot be activated through UI** — `knowledge.e2e.test.ts`:
   `activation()` refuses a draft (and, in `knowledge.test.ts`, candidate, evaluating,
   rejected, deprecated and active) before any command; sending `PromoteSkillVersion
   {to: active}` for the draft anyway is refused by the server with
   `RUNTIME_ILLEGAL_TRANSITION` and the skill keeps no active version; only after
   candidate → evaluating → approved does activation succeed; a deprecated version cannot
   be revived. Capabilities: the UI has no command that widens a projection — the only
   skill commands are the ladder's, and the run's resolution shows the active skill as an
   input that grants nothing (`grants == [read.internal]` with a manifest that needs
   `fs.write.workspace`).

## Tests (task list)

| test | where |
|---|---|
| delete propagation E2E | `apps/desktop/tests/knowledge.e2e.test.ts` "forgetting memory or knowledge removes it from every future retrieval…"; `crates/server/tests/knowledge_surface.rs::a_deleted_or_superseded_entry_leaves_every_retrieval_path_and_stays_as_a_tombstone` |
| skill activation policy test | `knowledge.e2e.test.ts` "a skill climbs the ladder one rung at a time; an unapproved version cannot be activated…"; `knowledge_surface.rs::a_skill_version_climbs_the_ladder_one_rung_at_a_time_and_grants_nothing`; `knowledge.test.ts` "skills" |
| memory is not recovery | `knowledge.test.ts` "refuses a row as memory…"; `api/knowledge.rs` unit test `the_memory_projection_reads_only_memory_entries`; `knowledge_surface.rs` asserts the exact key set of a memory row |
| edge tables pinned across owners | `python/tests/intelligence/test_data_lifecycle_edges.py`; `knowledge.test.ts` "pins the page's edge tables…", "pins the ladder spelling…", "pins the layer order…" |

## Notes

- Skill events (`skill.*`) are tenant-level — a skill row has no workspace — so the
  workspace stream does not carry them; the page re-reads the projection after every
  command, and the E2E reads rather than waits.
- `CreateMemory` remains unimplemented by design: memory is written through the
  policy-gated `memory.propose` tool (INT-007), not typed in by hand.
- `crates/server/src/control/data_lifecycle/mod.rs` still holds OPS-002's placeholder-era
  `DataSubject` value model beside the new module; OPS-002's own re-verification owns it.
