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
| `0019_sign_in_lookup.sql` | Sign-in under row-level security (APP-002 fix): `identity_user_by_email()` and `identity_sign_in_tenant()`, security definer, ids only, so a returning user's sign-in finds the account and its tenant before the user and tenant context exist, instead of onboarding the address a second time. Additive. |
| `0020_enterprise_identity.sql` | Enterprise identity (OPS-001): `sso_connections` (one OIDC connection per tenant; the client secret is a custody handle), `sso_group_mappings` (external group → tenant or workspace role, never `owner`), `external_identities` (SSO subject, SCIM externalId, origin), `directory_groups`/`directory_group_members` (SCIM groups, `dgr_`), `sso_states` (hashed state and nonce, PKCE verifier, single use), the `auth_flow` gate on `service_principals` so a key can be checked before the tenant is known. SSO and SCIM tell a new address from an existing account through 0019's `identity_user_by_email()`. Additive. |
| `0021_ops_switches.sql` | Operator kill switches (OPS-008): `ops_switches`, one row per engagement kept as history (`ksw_`, switch, target, reason, who and when engaged and released), a partial unique index so a switch is engaged at most once per target, tenant RLS. Additive. |
| `0022_audit_append_only.sql` | Append-only audit (QA-007 F-3): `quansio_app` loses UPDATE/DELETE/TRUNCATE on `audit_entries`, and triggers refuse UPDATE, DELETE and TRUNCATE for every role, so an entry cannot be rewritten (and the unkeyed chain recomputed) with the application's credentials. Additive. |
| `0023_workspace_tenant_integrity.sql` | Workspace/tenant integrity (QA-007 F-7): `UNIQUE (tenant_id, id)` on `workspaces`, and on every table that references `workspaces(id)` from `workspace_id` a composite key `(tenant_id, workspace_id) → workspaces(tenant_id, id)` (`NOT VALID`: new rows enforced, legacy rows not rescanned), so no row can name another tenant's workspace. Additive. |
| `0024_background_next_due.sql` | The background loop's wake-up (CORE-008, found by QA-009): `background_next_due()` (security definer, one timestamp only) — the earliest active routine window and scheduled or expiring timer across tenants — so the loop sleeps until due work instead of a fixed interval (claimed routine fires are the fire executor's). Additive. |
| `0025_browser_guard_state.sql` | Hardened managed browser (ADP-001): `browser_sessions.guard_state` (JSONB object, CHECKed), the worker's last report of each tab's dialog, certificate errors, downloads and in-doubt browser actions plus the user's certificate and permission decisions. One more column on the existing owner table, not a new store; the tabs' `document_revision` rides in the existing `tabs` JSONB. Additive. |
| `0026_skill_recordings.sql` | Skill recording (ADP-003): `skill_recordings`, one row per demonstration (`rec_`): owner, session family and epoch (the owner fence), status (`recording → finished → compiled → submitted`, or `cancelled`/`aborted`), the redaction summary and the compiled proposal under review. The archive's bytes are an Artifact and the submitted skill a DRAFT `skill_versions` row, so this is session state, not a second store. One `recording` per owner (partial unique index), idempotent start (`UNIQUE (tenant_id, owner_user_id, start_key)`), tenant RLS, composite workspace key. Additive. |
| `0027_artifact_quality_record.sql` | Output adapters (ADP-006): `artifact_versions.quality_record` (`{adapter, inputs[], ok, checks[], trust, …}`), written by the artifact authority with a version an output tool made from checks it ran itself; NULL for uploads and for `artifact.create`/`artifact.update`, which is how the deck gate tells the authority's own pages from a file somebody uploaded. Additive. |
| `0028_automation_triggers.sql` | Automation triggers (ADP-007): `routines.revision` (optimistic concurrency), `create_request_id`/`create_digest` (idempotent create), `hook_endpoint_id`/`hook_secret_handle_id` (an inbound hook and its SecretHandle), `routine_fires.payload` and the source `hook`, the `routine_hook_deliveries` replay ledger (RLS, unique on delivery key and signature digest) and `routine_hook_lookup()` (endpoint to tenant and routine ids, for a request that carries no tenant). Additive. |

## Applying

```bash
scripts/dev/up                                  # Postgres 17 + pgvector on 55440
QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio \
  cargo test -p quansio-server --test schema_bootstrap
```

The integration tests create and drop their own scratch databases, so they never touch
a development database.
