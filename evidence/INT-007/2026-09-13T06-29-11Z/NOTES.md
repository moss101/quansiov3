# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the
summary; a bundle may carry other evidence files).

## unit

2 — the durable store over `public.memory_entries`. Units 3 (gated candidate creation and the
`ProposeMemory` RPC) and 4 (retrieval through the semantic channel with its deletion path) remain.

## recorded_decisions

- Instants gained one canonical shape (`YYYY-MM-DDTHH:MM:SSZ`, `memory.instant`) in the model. It is
  the precondition for persistence rather than a nicety: `TIMESTAMPTZ` reads back as a `datetime`, so
  without one canonical rendering a stored instant would not equal the one written, and the model's
  lexicographic expiry comparison would be unsound. The store renders stored instants into that shape
  and refuses anything it cannot (`_instant_to_text`).
- The store evaluates the expiry in SQL, against an instant the caller supplies
  (`retrievable(now)`), so an expired memory cannot be returned by a read that filtered in Python
  afterwards; and neither the model nor the store keeps a clock, because a wrong clock must not be
  able to make memory retrievable.
- Expiry is exclusive (`expires_at > now`): at the instant itself the memory is no longer
  retrievable, which is the stricter reading and the one the boundary test pins.
- The lifecycle guard is tested where it is reachable: through the fabric an illegal edge is refused
  by the model before the store is touched, so the guard on the state the caller read is exercised
  directly against the store, deterministically (no race), including against real PostgreSQL with a
  concurrent actor's committed move.
- `delete` retains the row: a deleted memory is still stored (count 1) and merely not retrievable,
  which keeps it auditable and re-proposable. Nothing here is destroyed to make a query stop
  returning it.
- The structural test asserts that every statement names only `memory_entries` and none of
  `runs`, `steps`, `attempts`, `protocol_states`, `checkpoints`, `effect_records`: memory is not
  recovery, and the read set proves it rather than a comment claiming it.
- `READ_TABLES`/`STATEMENTS` are exported so that invariant lives in an auditable place, as RUN-009's
  `RECOVERY_READ_TABLES` does for the runtime's own read set.
- Marking a memory used is a record, not a state change, so the store does not filter on status; the
  fabric reads the entry first and refuses nothing about a deleted one, because a use mark on a row
  that is about to be audited is information rather than harm.
