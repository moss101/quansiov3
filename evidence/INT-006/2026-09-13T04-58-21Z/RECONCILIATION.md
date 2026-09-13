# INT-006 reconciliation — Knowledge Fabric

Recorded 2026-09-13 before editing, per AGENTS.md RECONCILE.

## Canonical owner
`python/intelligence/knowledge/` (INT-006's `paths`). The package was a docstring stub; there is no
prior implementation to migrate and no legacy authority to quarantine. Coverage: `GENUINE_GAP`.

## What already exists and is reused rather than re-created
- **Persistence shape (CORE-001, PASS).** `migrations/0001_canonical_schema.sql` declares
  `public.knowledge_entries` with exactly the DOMAIN.md §11.4 fields: id `CHECK (id LIKE 'kn\_%')`,
  tenant_id, workspace_id, scope `tenant|workspace|pack`, kind, content_ref, content JSONB,
  provenance JSONB, confidence 0..1, version, status
  `candidate|verified|active|superseded|quarantined|deleted`, superseded_by, timestamps, plus
  `knowledge_entries_scope_idx`. Row-level security is enabled and forced with the tenant context.
  No migration is needed by this task.
- **Wire contract (GOV-004, generated).** `quansio.v1.intelligence.KnowledgeEntry` with `Scope`,
  `Status` and `Provenance` already exists in `schemas/proto/quansio/v1/intelligence/intelligence.proto`;
  the model's vocabulary is taken from it rather than invented.
- **The semantic channel (INT-011).** `derived.embeddings` is keyed by source kind and ref, and
  `IndexSourceDeletion` is the seam this task calls when knowledge is deleted.
- **`KnowledgeEntry.embedding_ref`** is the derived pointer; the index remains INT-011's authority.

## Ownership decision (recorded, not assumed)
The authority set assigns the Knowledge Fabric to the intelligence plane: DOSSIER.md §4.2 lists
`knowledge/` under `python/intelligence/`, and INT-006's `paths` is that directory alone. Python
therefore owns these rows as its own durable data. This does not cross invariant 3 or 11: Python still
proposes rather than committing privileged runtime/graph/effect/machine state, and every write is
tenant-scoped through forced row-level security. The alternative (a Rust control-plane store, as
INT-009's skills took) is not what the task's `paths` say.

## Persistent state touched
`public.knowledge_entries` (rows), and `derived.embeddings` indirectly through the INT-011 deletion
seam. No new table, no schema change.

## Contracts / events
No `DOMAIN.md` change needed: §11.4 already names every field and state used here. RuntimeEvents for
`knowledge.*` are emitted by the runtime's outbox path, not by this plane.

## External effects, capabilities, approvals
None: this unit is pure rules over values. Ingestion of approved sources and verified run outcomes
(and therefore any external read) is a later unit of the same task.

## Real boundaries and environment
`real_boundary: false`. Database-backed units read `QUANSIO_TEST_POSTGRES_URL`
(`postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`); no provider credentials are involved.

## Rollback / recovery
The model is pure, so a change here cannot strand durable state; rows follow the schema's own
constraints. The lifecycle's fail-closed ladder is what keeps a restart from reading a state the
platform does not understand.

## Plan of units
1. **[done this run]** The entry model, provenance addressing, the lifecycle ladder and quarantine on
   source deletion (`knowledge/models.py`, `tests/intelligence/test_knowledge_models.py`).
2. The durable store over `public.knowledge_entries`: create, read by provenance address, promote,
   supersede, delete, all tenant-scoped, with database-backed tests.
3. Ingestion from approved sources and verified run outcomes, including the quarantine call on a
   source deletion and the INT-011 deletion seam.
4. The semantic channel wiring (index knowledge text through INT-011) and the evidence bundle.
