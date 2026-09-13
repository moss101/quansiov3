# INT-007 reconciliation — semantic memory with provenance

Recorded 2026-09-13 before editing, per AGENTS.md RECONCILE.

## Canonical owner
`python/intelligence/memory/` (INT-007's `paths`). The package was a docstring stub; there is no prior
implementation to migrate and no legacy authority to quarantine. Coverage: `GENUINE_GAP`.

## What already exists and is reused rather than re-created
- **Persistence shape (CORE-001, PASS).** `migrations/0001_canonical_schema.sql` declares
  `public.memory_entries`: id `CHECK (id LIKE 'mem\_%')`, tenant_id, workspace_id, scope
  `user|workspace|teammate`, subject_ref, content `TEXT NOT NULL`, provenance_kind
  `explicit_user|verified_run`, provenance_ref, confidence 0..1, status `candidate|active|deleted`,
  `last_used_at`, `expires_at`, timestamps, and `memory_entries_subject_idx (tenant_id, scope,
  subject_ref, status)`. Row-level security is enabled and forced with the tenant context, and the
  policy carries a `WITH CHECK`, so a cross-tenant write is refused by the database as well as by the
  store. No migration is needed by this task.
- **Wire contracts (GOV-004, generated).** `quansio.v1.intelligence.MemoryEntry` (Scope,
  ProvenanceKind, Status) and the `ProposeMemory` RPC (`MemoryCandidate` → `ProposalAck`) already
  exist. The servicer reports `ProposeMemory` as owned by INT-007; implementing it is this task's, the
  same way INT-011 implemented `Embed`.
- **The write path's seam is Rust (RUN-011, PASS).** `crates/server/src/runtime/turn_loop/dispatch.rs`
  routes the `memory.propose` tool to a `MemoryProposalPort`:
  `propose(MemoryProposalContext { run_id, workspace_id, candidate_json, scope }) -> Result<Value,
  RuntimeError>`. Its default implementation is `UnavailableMemoryProposals`, which fails closed naming
  `MEMORY_OWNER`. This task supplies the behaviour the runtime will call; the cross-process binding
  (the Rust port fulfilled by this Python plane) belongs to the composition root (APP-001), exactly as
  INT-005's context bridge and RUN-008's semantic verifier do.
- **The semantic channel (INT-011, BLOCKED_EXTERNAL implementation complete).** `derived.embeddings`
  is keyed by source kind and ref, and `IndexSourceDeletion` is the seam a deletion calls. INT-006's
  `indexing.py` is the pattern to follow for keeping a derived channel in agreement with the owner.

## Ownership decision (recorded, not assumed)
The authority set assigns semantic memory to the intelligence plane: DOSSIER.md §4.2 lists `memory/`
under `python/intelligence/`, and INT-007's `paths` is that directory alone — the same shape INT-006's
Knowledge Fabric has. Python owns these rows as its own durable data; it still proposes rather than
committing privileged runtime/graph/effect/machine state, and every write is tenant-scoped through
forced row-level security.

## The invariant that shapes this task
**Memory is not recovery** (AGENTS.md invariant 7, D-007, DOMAIN.md §11.4). Two consequences are
design constraints rather than documentation:

1. **Nothing here may be on a restart path.** No recovery, checkpoint, protocol-state or Effect Ledger
   code may read `memory_entries`, and this plane exposes no API that a restart would need. The task's
   own acceptance statement is that a runtime restart succeeds with memory *disabled*, which is only
   meaningful if retrieval is a pure enrichment that can be absent.
2. **Deletion must reach retrieval.** "Deleting memory removes it from retrieval after index refresh"
   (DOMAIN.md §11.4, bounded by §21.3's 60 s SLO), which is the INT-011 deletion seam again: the row's
   lifecycle moves, and the derived rows go.

## Persistent state touched
`public.memory_entries` (rows), and `derived.embeddings` indirectly through the INT-011 deletion seam.
No new table, no schema change.

## Contracts / events
No `DOMAIN.md` change needed: §11.4 already names every field and state used here. The `ProposeMemory`
RPC exists; implementing it changes no proto.

## External effects, capabilities, approvals
`memory.propose` is policy-gated in the runtime (RUN-006/RUN-011) before this plane sees a candidate:
the runtime checks the capability projection and policy, and this plane's own gate is provenance and
scope (memory comes from an explicit user direction or verified work, and nothing else). No external
effect is created here; a write is a database row.

## Real boundaries and environment
`real_boundary: false`. Database-backed units read `QUANSIO_TEST_POSTGRES_URL` and prepare their
scratch database through `scripts/dev/seed_test_database.py` (added by INT-006 for exactly this
reason: canonical identity seeding is tooling's job, not the product's or a test's); no provider
credentials are involved.

## Rollback / recovery
The rows are additive and versioned by their own lifecycle, so a change here cannot strand runtime
state; and by construction memory is never read on a restart path, so a defective release can be
disabled without affecting recovery.

## Plan of units
1. **[next]** The entry model, its provenance kinds and its closed lifecycle (`candidate → active →
   deleted`, with an expiry rule), as pure values with tests.
2. The durable store over `public.memory_entries`: tenant-bound, provenance-addressed, with the
   `last_used_at` mark a retrieval records, and database-backed tests.
3. Candidate creation from explicit user direction and verified work, gated by provenance and scope —
   including the `ProposeMemory` RPC on the servicer, so the runtime's `memory.propose` path has a
   real destination.
4. Retrieval through the semantic channel (INT-011, source kind `memory_entry`) plus the deletion path
   that removes a memory from retrieval after index refresh, and the memory-off property.
