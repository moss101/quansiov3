# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the
summary; a bundle may carry other evidence files).

## unit

4 — retrieval through the semantic channel and the forgetting path. This closes INT-007's
implementation.

## acceptance

- **Runtime restart succeeds with memory disabled.** Proved structurally rather than by a shutdown
  test: the model may not carry `checkpoint`, `cursor`, `position`, `effect_id`, `generation`,
  `lease`, `protocol_state`, `resume_token`, `attempt` or `backoff`, a test asserts those fields do
  not exist, and the store's statements name only `memory_entries` (never `runs`, `steps`,
  `attempts`, `protocol_states`, `checkpoints` or `effect_records`). Nothing in this plane is read on
  a restart path, so a restart with memory disabled has nothing to miss.
- **Memory deletion prevents future retrieval after index refresh.** `forget_memory` applies the
  model's terminal edge and then clears the memory's derived rows through INT-011's deletion seam, and
  the suite checks both planes afterwards: the row is retained (count unchanged, status `deleted`),
  the semantic query returns nothing for it, and an unrelated memory still answers. `retrieve` also
  re-checks each hit against the fabric *and the clock*, so a row the index still holds during the
  refresh window is not served — the acceptance statement holds even before the index catches up.

## recorded_decisions

- Memory needs no source reader, unlike the Knowledge Fabric: `memory_entries.content` stores the text
  itself, so the channel reads the memory. A structural test asserts the module names no object reader,
  which documents the difference rather than leaving it to a comment.
- The snapshot a hit cites is the digest of the memory's text (`content_digest`, INT-011's helper), so
  a hit is attributable to what it actually answered with; memory has no version column, and a
  content-addressed snapshot is the honest equivalent.
- An expired memory is *pruned* by the next synchronisation, not deleted: it is no longer retrievable
  by the model's own predicate, so the derived channel must not hold it, and because the channel is
  derived the pruning is reversible by re-indexing. Its row stays `active` in the fabric, which the
  suite asserts.
- The index calls in `forget_memory` are not wrapped: an unreachable index is reported, because it is
  rebuildable while a forgotten memory that was never recorded as forgotten is not.
- The channel refuses an index and a fabric bound to different tenants, so one channel serves one
  tenant by construction; retrieval applies the same check before it reads anything.
