# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## unit

1 — the Rust epoch lifecycle (`crates/server/src/runtime/compaction/`) with its tests, which is
INT-008's first acceptance statement at the persistence boundary. The Python half (the bounded
conversation projection) and the task's closing evidence remain.

## acceptance

**Fork/revert never installs compaction from abandoned history.** An epoch may only be installed while
the position the caller holds still contains the range it summarises (a revert moved it back) and while
it belongs to the run the caller works on (a fork left it behind). Either refusal moves the epoch to
`rejected_stale` and keeps the row: the refusal is the auditable fact, and the schema has a status for
it because the decision is durable.

**Exact protocol replay does not depend on summary text.** Asserted where it is authoritative:
RUN-009's declared `RECOVERY_READ_TABLES` names no compaction table, and the recovery module's own
sources never query `compaction_epochs`. The property is maintained by the compaction store *not* being
in any replay path, which is why this module reads and writes no protocol state.

## recorded_decisions

- **A stale refusal is not an error — except when the caller is fenced out.** The first version
  returned a typed error after writing `rejected_stale`, and the tests caught the defect: a caller that
  rolls back on `Err` discards the very record of the refusal. So a refusal about *history* is returned
  as `InstallOutcome::Rejected` with the epoch already moved and the caller commits, while a stale
  *generation* is returned with the epoch **unchanged**: a fenced-out writer must not write a decision
  on the current controller's behalf, which is exactly what fencing exists to prevent.
- **Staleness is decided inside the installing transaction** (the row is read `FOR UPDATE`), so a
  concurrent revert cannot land between the decision and the status write.
- **The decision is taken from what the schema records.** `compaction_epochs` has no
  authoring-generation column, and adding one would be a CORE-001 schema change outside this task's
  paths. It does not need one: a generation is a *fence the caller holds*, so a mismatch identifies a
  stale controller, while abandoned history is identified by the run the epoch belongs to and by the
  position that still contains its range.
- **`seq` is `INTEGER`** in the schema while the source sequences are `BIGINT`; the store maps them
  as the table declares rather than as a guessed uniform width. A test that failed on the wrong width is
  what surfaced this.
- No protocol state is read or written here, so compaction cannot become part of what a run obeys;
  deleting the rows would lose an optimisation, never a fact.
