# APP-011 reconciliation (2026-09-20, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `apps/desktop/src/knowledge/controls.ts` (52 lines
of local predicates over an invented `KnowledgeEntry {id, status, kind}`; no projection, no
command, no UI; docs/review/2026-09-18-m5-placeholder-audit.md). A stale
`task/APP-011-knowledge-ui` branch from that era is parked as
`stale/APP-011-knowledge-ui-2026-09-15`.

- **Canonical owners.** The surface is `apps/desktop/src/knowledge/` (the task's `paths`).
  Behind it: the Knowledge Fabric (`python/intelligence/knowledge/`, INT-006) and semantic
  memory (`python/intelligence/memory/`, INT-007) own the rows and the lifecycle *model*
  (`knowledge/models.py::TRANSITIONS`, `memory/models.py::TRANSITIONS`); the derived
  semantic index is `derived.embeddings` (INT-011, keyed by `source_kind`/`source_ref`);
  skills are the Rust control plane (`crates/server/src/control/skills`, INT-009:
  `SkillStore::{create_skill, create_version, promote, active_versions}` over the §11.5
  ladder, one ACTIVE version per skill); a run's authority is its capability projection
  (`capability_projections`, CORE-004: `inputs[]` in the fixed layer order, `grants[]`).
- **Existing code, coverage PARTIAL, kept:** every model, store, ladder and forgetting path
  above; the Rust search projection (APP-005) already serves only `active|verified`
  knowledge; Python retrieval (`memory/retrieval.py`, `knowledge/indexing.py`) re-checks
  every hit against the fabric, so a status change is enough for it, and clears derived rows
  through `SourceDeletionPort`.
- **Gaps:** `GET /v1/knowledge`, `/v1/memory`, `/v1/skills` (catalogued) unserved;
  `DeleteKnowledge`, `SupersedeKnowledge`, `DeleteMemory`, `InstallSkill`,
  `PromoteSkillVersion`, `DisableSkill` (catalogued) unimplemented; a run's projection is
  seeded with no `inputs`, so nothing shows *why* a run may do what it may; the desktop has
  no knowledge module, bridge channels or page.
- **Ownership decision (recorded, not assumed).** A user-initiated delete of a knowledge or
  memory entry is a *data-lifecycle control* operation: DOSSIER.md §3 puts "security-critical
  audit and lifecycle control" in Rust, §21.3 defines the deletion workflow (authoritative
  delete → tombstone → derived-store purge → audit), and OPS-002 (Rust,
  `crates/server/src/control/data_lifecycle/`) owns that workflow across authoritative and
  derived stores. The alternative — a Rust gRPC client to the Python plane with new forget
  RPCs — does not exist today (no crate in the server links `quansio-contracts`; the plane's
  scope gate requires a run's capability projection, which an administrative command has
  no honest value for) and would put the tombstone on the plane that DOSSIER.md §4.2 says
  proposes rather than commits. So `control/data_lifecycle/intelligence.rs` applies the
  §11.4 tombstone edges only (`→ deleted` from every non-deleted state; `→ superseded` from
  `verified|active`, naming an existing successor), purges the entry's `derived.embeddings`
  rows in the same transaction, and stages `knowledge.deleted` / `knowledge.superseded` /
  `memory.deleted` RuntimeEvents — the events INT-006's reconciliation already assigns to
  "the runtime's outbox path, not this plane". The Python model remains the full ladder and
  its own forgetting path (`forget_entry`, `forget_memory`) is untouched: both paths apply
  the same terminal edge to the same row under the same RLS, and a Python test pins the
  edge tables against each other. OPS-002's existing module is a placeholder-era value
  model (`DataSubject`) that its own re-verification must address; this task adds beside
  it, not on top of it. `CreateMemory` stays unimplemented (memory is written through the
  policy-gated `memory.propose` tool, INT-007).
- **Persistent state touched:** `knowledge_entries.status/superseded_by`,
  `memory_entries.status` (tombstones; rows stay), `derived.embeddings` (rows removed),
  `skills`/`skill_versions` through `SkillStore`, `capability_projections.inputs` (now
  recorded). No new table, no migration.
- **Contracts (DOMAIN.md §14, all already catalogued):** `GET /v1/knowledge?workspace_id
  [&status][&include_deleted]`, `GET /v1/memory?workspace_id[&scope][&subject_ref]
  [&include_deleted]` (memory rows carry no recovery field — the SQL names only
  `memory_entries`, asserted structurally), `GET /v1/skills` (every version with its ladder
  position and the edges legal from it); `DeleteKnowledge {knowledge_id}`,
  `SupersedeKnowledge {knowledge_id, superseded_by}`, `DeleteMemory {memory_id}`,
  `InstallSkill {scope, name, owner, version {semver, manifest, provenance?}}` (a DRAFT),
  `PromoteSkillVersion {skill_version_id, to}` (the ladder decides; an unapproved version
  cannot become ACTIVE: `RUNTIME_ILLEGAL_TRANSITION`), `DisableSkill {skill_id}`
  (ACTIVE → DEPRECATED). `GET /v1/runs/{id}` gains `capability_projection` (id, inputs in
  layer order, grants, digest, expiry): the task-scoped resolution. Additive; no DOMAIN.md
  change.
- **Capabilities:** a run's seeded projection now records its inputs — the workspace's
  policies (`workspace_policy` layer) and the tenant's ACTIVE skill versions (`active_skills`
  layer, digest of the manifest). Skills contribute no grant (DOMAIN.md §11.5, §12); the UI
  displays `capability_needs` as needs and has no command that widens a projection.
  **External effects / real boundaries / migrations:** none.

## Plan

1. Server: `control/data_lifecycle/intelligence.rs` (tombstone + purge + events);
   `api/knowledge.rs` (three projections, six commands), run projection gains
   `capability_projection`, `seed_projection` records inputs; `quansio-dev-seed
   --intelligence` seeds one active knowledge entry and one active memory entry, each with
   a derived embedding row, for harnesses.
2. Desktop `knowledge/`: `entries.ts` (decode knowledge/memory with provenance and scope,
   the retrievability rule, a structural refusal of recovery fields on memory),
   `skills.ts` (decode skills, legal edges, `activation()` refuses anything but APPROVED →
   ACTIVE before a command is sent), `capabilities.ts` (decode a run's projection into
   layers and grants); renderer `Knowledge.tsx`; bridge channels `knowledge.list`,
   `knowledge.delete`, `knowledge.supersede`, `memory.list`, `memory.delete`,
   `skills.list`, `skills.install`, `skills.promote`, `skills.disable`,
   `runs.capabilities`.
3. Tests: Rust `knowledge_surface.rs` (delete propagation: projection tombstone, search
   loses the entry, derived rows gone, event staged; supersede edges; memory delete; skill
   ladder through the API); Python pin of the tombstone edges against the models; desktop
   unit tests and `knowledge.e2e.test.ts` against the real server (delete propagation
   E2E, skill activation policy E2E).
