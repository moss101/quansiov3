# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## acceptance

**Only machine-control mutates target/lease lifecycle.** `crates/machine/src/control/` is the only
module that writes `execution_targets` or `leases`, and that is a *test* rather than a convention: the
suite scans every `.rs` file in `crates/` (outside this module and test directories) for the six write
statements and fails if any other owner has one. It also asserts it actually walked the workspace, so a
scan that silently found nothing cannot pass.

**Lease expiry or generation change fences old controllers.** A generation is the controller epoch: a
transition and a lease acquisition are both guarded on the generation the caller holds, so a controller
that was replaced is refused — and a *fenced-out* caller writes nothing at all, because writing a
decision on the current controller's behalf is exactly what fencing prevents. Expiring a lease bumps its
target's generation, so a disconnected worker that reconnects holding the old generation finds every
attempt refused for a reason it cannot argue with; revoking does the same. `validate_lease` is the check
a worker applies before it acts — the `(lease_id, generation)` pair qworkerd must present — and a lease
that is not held, was granted to another generation, or has expired fails it.

## recorded_decisions

- **Staleness is decided from the schema's own vocabulary.** The lease table has `holder_generation`
  and `expires_at`; the target has `generation`. Nothing new was needed, and the generation is bumped in
  exactly two places — replacing a failed target, and expiring or revoking a lease — so "the generation
  moved" always means "the controller changed".
- **Health is derived, never reported.** A worker's claim about itself is not authority, so
  `observe` stores what the target reports and `ExecutionTarget::classify_health` classifies it from
  heartbeat freshness and the desired/observed states. The freshness comparison is made in SQL (an
  interval belongs to the database that stores the instants), so there is one piece of interval
  arithmetic in the module rather than two that could disagree.
- **Instants are rendered canonically.** The first version returned Postgres's own rendering
  (`2026-09-13 10:01:00+00`), which does not compare with the ISO-8601 a caller passes in; the store now
  renders through `to_char(... AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MM:SS"Z"')`, so a returned instant
  is the shape the caller gave. A test comparing them is what surfaced it.
- **Every statement carries an explicit tenant predicate**, not only the table's forced row-level
  security. The isolation test failed on the first version precisely because a superuser connection
  bypasses the policy: the predicate is what makes the store refuse a cross-tenant read on its own.
- **`acquire_lease` takes a `LeaseRequest`** rather than eight arguments: clippy's `too_many_arguments`
  pointed at a real readability problem, and the request struct also documents the acquisition as one
  decision (identity, holder, fence, window).
- **Acquisition locks the target row first**, so two racing controllers cannot both be granted a lease;
  the race is a test (`tokio::join!` on two connections), not an assumption.
- Two edges the §8.2 diagram leaves implicit are admitted and named in the code: `STOPPED → DESTROYED`
  (a stopped target without a snapshot is still destroyed) and `SNAPSHOTTED → READY` (a restorable
  target is brought back rather than cloned).
