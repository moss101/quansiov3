# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the
summary; a bundle may carry other evidence files).

## unit

4 — the semantic channel: knowledge retrievable by meaning, and the derived index kept in agreement
with the fabric. This closes INT-006's implementation.

## recorded_decisions

- Synchronisation is incremental and two-directional: only `active` knowledge is indexed, and the
  derived index is pruned down to the entries the fabric still considers retrievable. That second
  direction is why this is a module rather than a call: a quarantined or withdrawn entry must stop
  being reachable by meaning, not merely be filtered out by a caller that remembers to.
- An entry whose text cannot be read keeps the rows it already has and is reported in
  `missing_text`. Pruning answers "this is no longer retrievable", never "the source was momentarily
  unreadable".
- `retrieve` re-checks every hit against the entry's lifecycle instead of trusting the index. The
  index is rebuildable and can lag a deletion by an instant; the fabric agreeing is what makes a
  stale row unservable rather than unlikely. A hit whose entry has gone, whose entry is not
  retrievable, or that belongs to another channel is dropped, never returned with a caveat.
- Text comes from the authoritative source plane through INT-011's digest-verifying object reader
  (`DescribedKnowledgeText`), with descriptors supplied by whoever owns the source metadata
  (CORE-007). The channel reads nothing itself and never calls a whole-index rebuild — rebuilds are
  INT-011's operation, and a structural test asserts the module contains no such call.
- The channel refuses to pair an index and a fabric bound to different tenants, so one channel
  serves one tenant by construction.
- The object-bytes port in the database-backed test is in memory: INT-011's own database-backed
  suite proves that reader against real MinIO object storage, and unit 4's subject is the
  synchronisation between two planes, not the object read.
