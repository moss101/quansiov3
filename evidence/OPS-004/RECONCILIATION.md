# OPS-004 reconciliation

- Canonical owner: `crates/server/usage/`. Runtime budgets remain RUN-010
  (`crates/server/src/runtime/budgets`). UsageRecords are a projection of
  RuntimeEvents (CORE-009 / DOMAIN.md §13.4); billing is not runtime truth.
- Existing: budget charge emits `usage.recorded` for *budget* meters. This task
  projects the §13.4 UsageRecord meters and gates effects on hard/soft quotas.
- Coverage: GENUINE_GAP for the projection + quota gate.
- Persistent state: `usage_records` table already exists (CORE-001). Rebuild is
  a pure function of events; optional persist is idempotent on
  `(tenant_id, source_event_id, meter)`.
- Contracts: DOMAIN.md §13.2 Budget (RUN-010), §13.4 UsageRecord. No new fields.
- External effects: none.
- Rollback: refusing a quota does not write an effect or a usage row.
