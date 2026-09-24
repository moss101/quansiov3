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
| `0014_teammate_definition.sql` | Teammate definition preferences (APP-014): `template_id`, `model_route_preference`, `default_execution_target_id`, `availability` (`available`/`away`) and a `version` bumped by every configuration change, so a run's capability projection can name the definition it was resolved from. Additive. |
| `0015_thread_participant_stamp.sql` | Thread membership (APP-009): the `updated_at` column the shared stamping trigger on `thread_participants` expects (0001 attached the trigger without it, so any membership update failed), and a partial index over active participants. Additive. |
| `0016_routine_fires_notification_delivery.sql` | Routines and notifications (APP-010): `routine_fires`, the firing ledger with one row per fire key (claimed with the routine's next due time, leased while executing, retried with backoff), `notification_deliveries` (email per notification with attempts and backoff), and `notifications.source_event_id`, `title` and the requested `channels`, with a unique index so one event notifies a user once, and `background_tenants()` (security definer, tenant ids only) for the background loop. Additive. |
| `0017_webhook_deliveries.sql` | Outbound webhooks (APP-015): `webhook_subscriptions.created_by` and `start_sequence` (a subscription receives events committed after it), and `webhook_deliveries`, the delivery ledger with one row per (subscription, event), leased per attempt and retried with backoff until delivered or dead-lettered. Additive. |
| `0018_artifact_format_check.sql` | Artifact source formats (CAP-003): `artifact_versions.format_check` (`{ok, rule, detail}`), written by the store for every version it adds and read by the `artifact_valid` completion check. Additive. |
| `0019_enterprise_identity.sql` | Enterprise identity (OPS-001): `sso_connections` (one OIDC connection per tenant; the client secret is a custody handle), `sso_group_mappings` (external group → tenant or workspace role, never `owner`), `external_identities` (SSO subject, SCIM externalId, origin), `directory_groups`/`directory_group_members` (SCIM groups, `dgr_`), `sso_states` (hashed state and nonce, PKCE verifier, single use), the `auth_flow` gate on `service_principals` so a key can be checked before the tenant is known, and `identity_user_by_email()` (security definer, id only) so sign-in can tell a new address from an existing account under RLS. Additive. |

## Applying

```bash
scripts/dev/up                                  # Postgres 17 + pgvector on 55440
QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio \
  cargo test -p quansio-server --test schema_bootstrap
```

The integration tests create and drop their own scratch databases, so they never touch
a development database.
