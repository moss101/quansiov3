# migrations

Authoritative PostgreSQL schema for Quansio V8.1. Canonical owner: `crates/server`
(`crates/server/src/control/schema/`); the files here are SQL, applied only through that
runner (`quansio_server::control::schema::migrate`).

## Policy

- **Forward only.** Each file is applied once, in filename order, and recorded in
  `_sqlx_migrations`.
- **Rollback = forward-fix.** There is no `down` script: a release never ships a
  destructive downgrade. A defect is corrected by a new forward migration; data
  recovery uses backup/restore (`OPS-005`).
- **Additive within a release.** Dropping a column or narrowing a type requires an
  approved `D-###` decision and a data migration plan.
- **Tenant scope is structural.** Every tenant table carries `tenant_id` (and
  `workspace_id` when workspace-scoped) with row-level security `ENABLE`d and `FORCE`d.
  With no `quansio.tenant_id` context the policies match nothing, so a missing context
  fails closed rather than leaking rows (DOSSIER.md §9, §16).
- **`derived` stays rebuildable.** Indexes, embeddings and index epochs live in the
  `derived` schema and can be dropped and rebuilt from authoritative rows and object
  storage; they are never a source of truth.

## Inventory

| Migration | Contents |
|---|---|
| `0001_canonical_schema.sql` | DOMAIN.md §2–§13: identity/tenancy, conversation, work graph, runtime (runs/turns/steps/attempts/protocol state/checkpoints), capability/policy/approvals, Universal Effect Ledger, tool calls and command idempotency, RuntimeEvent + outbox + cursors, execution fabric (targets/leases/browser/terminal), artifacts/evidence, intelligence state (context, model routes/calls, knowledge, memory, skills, packs), automation/notification/usage/audit, `derived` embeddings + index epochs, RLS policies, the `quansio_app` role and `updated_at` triggers. |
| `0002_runtime_event_sequences.sql` | Per-tenant `tenant_event_sequences` counter row used to assign the tenant-monotonic RuntimeEvent `sequence` inside the commit transaction (CORE-003). |
| `0003_graph_heads.sql` | WorkGraph aggregate revision head per (tenant, workspace) for the batch-level compare-and-set of a GraphTransaction (DOMAIN.md §1.2, §4.5): RLS, `quansio_app` grant and `updated_at` trigger (CORE-004; renumbered from 0002 to keep migration versions unique). |

## Applying

```bash
scripts/dev/up                                  # Postgres 17 + pgvector on 55440
QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio \
  cargo test -p quansio-server --test schema_bootstrap
```

The integration tests create and drop their own scratch databases, so they never touch
a development database.
