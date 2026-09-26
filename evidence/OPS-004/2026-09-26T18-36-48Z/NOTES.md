# OPS-004 — usage is metered over the whole history

Found by QA-009 (P1, cost controls): `GET /v1/usage` rebuilt the projection on every read from
`read_events_after(tenant, None, 20_000)` — the tenant's **oldest** 20,000 RuntimeEvents. A busy
workspace passes that in hours (every thread, message, run and step is an event), after which nothing
new was metered at all, and every read re-projected twenty thousand events. The canonical schema
already had the persisted projection DOMAIN.md §13.4 describes (`usage_records`, unique on tenant,
source event and meter), but nothing wrote it.

Fix (`crates/server/src/usage/store.rs`):
- `UsageStore::materialize` projects the events after the projection's consumer cursor
  (`usage:events` / `usage-projection`) into `usage_records` — idempotently (`ON CONFLICT DO NOTHING`
  on the deterministic `use_` identity) and in the transaction that advances the cursor, so a crash
  repeats or loses nothing; `catch_up` runs batches of 1,024 until caught up (at most 8 per pass). The
  background loop calls it for every tenant on each pass.
- `UsageStore::summary` sums the rows and reads the cursor in one statement (one snapshot), then
  projects only the events after that cursor, so a read is exact wherever the projection stands and
  costs the same however long the history. More than 20,000 unmaterialized events (the loop far
  behind) is reported as `complete: false` rather than guessed.
- `GET /v1/usage` keeps its shape: `meters` over the whole history, `events_read` (the last event
  sequence reflected), the newest 500 `records`, plus `record_count` and `complete`; the web admin
  counts `record_count` (`apps/web/src/review/admin.ts`).

Tests: `usage_store` (DB) — with nothing materialized, the cursor mid-history, at the end, and new
events after it, a read (tenant-wide and for one workspace) equals a rebuild from every event; replaying
the whole stream after losing the cursor meters nothing twice; the listing is bounded and ordered while
the count is not; the usage, web admin surface and gateway skeleton suites (`tests.log`); the web decode
test (`web.log`); fmt and clippy (`lint.log`). Mutation (`mutation.log`): a read that ignores the events
after the cursor fails the equality. QA-009 measures storage metering against actual bytes past twenty
thousand events on the served build.

Open (recorded, not fixed here): OPS-004's "enforce hard/soft quotas through runtime/capability policy"
has no production caller — `usage::quota` is a library the tests exercise — and DOMAIN.md defines no
quota/entitlement settings to enforce. QA-009 records it as a finding.
