# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

**This is not a second EXEC-008 deliverable.** EXEC-008's first full-workspace run surfaced a
concurrency defect in the cancel path, and this bundle is its repair, filed under EXEC-008 because that
is the task whose testing found it and whose `progress.json` entry points at it. The defect is in
`crates/server`, which is RUN-008's area, so no task's acceptance statement is what this repairs: the
property repaired is one the product depends on.

## the defect

`crates/server/tests/orchestration.rs::a_cancel_storm_cancels_each_run_once_and_blocks_dependents`
fires eight concurrent cancellations of the same subtree and asserts that exactly two runs are
cancelled, "once each". It intermittently saw three.

`RunStore::cancel` (`crates/server/src/runtime/state_machine/store.rs`) returned
`Ok(run-in-cancelled-state)` **both** when it performed the cancellation and when it found the run
already cancelled — it has to, because a transaction that stages no event is refused by the event
store, so the already-cancelled case short-circuits before opening one.
`OrchestrationService::cancel_runs` then counted `Ok(after) if after.status == Cancelled` as a *new*
cancellation. Under a storm, the window between that caller's own read and `cancel`'s read lets two
callers both be told they cancelled the same run. The resulting state is identical either way, so the
caller cannot recover the distinction afterwards.

## the fix

`RunStore::cancel_once` reports which of the two happened (`CancellationWon::Cancelled` or
`AlreadyCancelled`); `cancel` keeps its shape for callers that only want the resulting state, and
delegates. `cancel_runs` counts only a call that performed the cancellation, and still maps a
concurrent winner's `StateConflict` to "already cancelled".

One thing was deliberately **not** done. An intermediate version also mapped
`FencedStaleGeneration` to "already cancelled", which would have been wrong: a run whose generation
moved while still running is not cancelled at all, and swallowing that would have silently skipped a
run while reporting success. That arm was removed before the measurements below, and the storm still
passes 20 of 20 without it.

## what was measured, on the same machine and the same database

| State | `cargo test --workspace` |
|---|---|
| `dd5e024`, before EXEC-008 existed | 4 clean / 6 (2 failures, both this test, 0 lock exhaustion) |
| `40327f6` + this fix, stack at default limits | 4 clean / 6 (2 lock exhaustion) |
| `fb50673`, stack sized (`max_locks_per_transaction=1024`, `max_connections=100`, shm 1g) | **9 clean / 10** (1 lock exhaustion, 0 correctness failures) |

Focused: the orchestration suite passed **20 of 20** consecutive runs after the fix, against 8 of 10
before it.

The regression is covered by a deterministic test rather than only by the storm:
`only_the_cancellation_that_did_the_work_reports_that_it_cancelled_the_run` cancels a running run,
asserts the first call reports `Cancelled` and the second `AlreadyCancelled`, and asserts the two
returned runs are equal — which is exactly why the distinction must be reported rather than inferred.

## the residual, recorded rather than papered over

One run in ten still fails, and not with a wrong answer: Postgres refuses with

```
out of shared memory ... You might need to increase "max_locks_per_transaction"
```

while a test suite's scratch database is being migrated. Every database-backed test applies the whole
canonical schema to its own scratch database, `cargo test --workspace` runs test binaries in parallel
(18 cores here) and each binary runs its tests in parallel, so dozens of migrations are in flight at
once and a migration transaction holds a lock on every object it creates. Adding a migration makes each
of those transactions hold more locks, which is how EXEC-008's `0009_egress_grants.sql` brought the
limit into reach: zero exhaustions in 6 baseline runs, 2 in 6 with it.

Two changes were made, both in repository configuration rather than the product:
`infra/compose/compose.yaml` and `.github/workflows/ci.yml` now set
`max_locks_per_transaction=1024`, `max_connections=100` and a 1 GB shared-memory segment — the lock
table is sized from that value, so it needs the larger segment too. That took the failure rate from
2 in 6 to 1 in 10. It is not zero, so this is recorded as an environment capacity limit and not
claimed fixed.

The better fix, not taken here because it spans every crate's test harness at once and is not
EXEC-008's to make: migrate one template database per test binary and clone it with
`CREATE DATABASE ... TEMPLATE ...`, which removes the concurrent-migration pressure entirely instead
of buying headroom for it.

## the second pre-existing failure

`tests/turn_loop.rs::a_turn_executes_tool_proposals_through_capability_policy_and_the_effect_ledger`
failed once with `duplicate key value violates unique constraint "protocol_states_pkey"` under full
parallel load and passes standalone. It did not reappear in any of the 28 workspace runs after the
fix. Scratch databases are **not** shared (each test names its own from its prefix and the process id),
so this is a concurrency-sensitive write in the turn loop rather than a harness collision. It stays
recorded in `HANDOFF.md` as a known defect, undiagnosed past the symptom.
