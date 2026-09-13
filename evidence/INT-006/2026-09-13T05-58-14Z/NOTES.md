# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the
summary; a bundle may carry other evidence files).

## unit

3 — ingestion from approved sources and verified run outcomes, and the forgetting path.

## scope

`python/intelligence/knowledge/ingestion.py` is what a model or runtime may propose into the
fabric, and how a source is forgotten. It owns no new storage: proposals become rows through the
unit 2 store, and forgetting goes through INT-011's `SourceDeletionPort`.

## recorded_decisions

- A proposal carries no identity, no tenant, no scope and no lifecycle state — a structural test
  asserts those fields do not exist — because identity is minted by the canonical owner, the tenant
  and scope are the caller's authority, and the lifecycle starts at `candidate` for anything a
  model proposes (AGENTS.md invariant 10: no model self-certification of completion).
- The provenance-kind vocabulary is closed to `approved_source` and `verified_run`: knowledge comes
  from something the platform evaluated, and an unknown origin is refused rather than stored.
- Re-ingesting the same claim is not a duplicate write: the same kind, content address and
  provenance among the entries still in force returns the existing entry with `created=False`,
  while an entry that was superseded or withdrawn is new knowledge when proposed again.
- Forgetting a source has three steps in the order of their authority: quarantine the knowledge
  derived from it in the fabric (committed first, and it stands even if the index cannot be
  reached), remove the source's own derived index rows, then remove the rows of every entry the
  deletion quarantined — because a quarantined entry must stop answering retrieval
  (DOSSIER.md 21.3) rather than stay reachable through the semantic channel.
- The index calls are not wrapped: an unreachable derived index is reported to the caller rather
  than hidden, and the index is rebuildable while an unrecorded decision is not.
- `delete` remains the model's terminal lifecycle edge: `forget_entry` withdraws the entry (the row
  is retained for audit) and then forgets it as a source.
- Seeding canonical identities is tooling's job, not the product's or a test's: the repository's
  architecture gate rejects `python writes canonical authority table 'tenant'`, and
  `knowledge_entries.tenant_id` references `tenants`, so the suites prepare their scratch database
  through `scripts/dev/seed_test_database.py` (the counterpart of `scripts/dev/seed` for the dev
  stack). No Python product module and no test contains that write, and the tool refuses an
  identity that is not a canonical ULID — which is how a non-Crockford id in an earlier fixture was
  caught.
- The evidence summaries for INT-011 and INT-006 units 1 and 2 were rewritten to the canonical
  `evidence-summary` schema after the contract gate rejected extra top-level fields; the narrative
  moved into `NOTES.md` beside each summary.
