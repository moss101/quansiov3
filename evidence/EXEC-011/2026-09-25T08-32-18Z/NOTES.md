# EXEC-011 regression fix: the connector broker reads under row-level security

**Found by:** the APP-001 fix `fix/APP-001-api-queries-under-rls` (2026-09-25), which ran every
statement in `crates/server/src/api/` inside a tenant-scoped transaction and exposed the same
gap in the connector broker.

**Failing test:** `cargo test -p quansio-server --test connector_broker
the_connector_lifecycle_works_as_the_application_role_under_rls` (new). It migrates and seeds as
the superuser, then drives begin -> complete -> degrade -> re-connect -> revoke on a pool whose
connections `SET ROLE quansio_app`. Before the fix, at `c207b11` (main) plus the test
(`cargo-red.log`):

```
complete as quansio_app: NotFound { id: "cnx_01M3BSQ8RTCYSJPY7X6SQ640R5" }
```

**Cause:** `ConnectorBroker::load_async` ran its `SELECT ... FROM connector_instances` on the bare
pool, with no transaction and no tenant context.
- `connector_instances` has FORCED row-level security. As `quansio_app` with no
  `quansio.tenant_id` set, the policy hides every row, so every instance was `NotFound`.
- Its callers broke with it: the OAuth callback (`complete_auth`) and every `transition`
  (MarkDegraded, re-connect, RevokeConnector).
- The dev stack connects as the superuser, which bypasses RLS, so no existing test saw it.

**Fix:** `load_async` opens the event store's tenant transaction
(`EventStore::begin_tenant_transaction`, the same `set_config('quansio.tenant_id', $1, true)` as
`schema::set_tenant_context`), reads, and commits. The module's three writes already ran in
that transaction through `commit_mutation`. The broker no longer holds a bare pool, so an
unscoped statement cannot be written in this module without reaching for one.

**Rest of the owner:** `crates/server/src/control/connectors/` is one file. Its only other
statements are the INSERT in `begin_auth` and the UPDATEs in `complete_auth` and `transition`,
all inside `commit_mutation`. Outside the owner, the only `connector_instances` read is the admin
projection in `crates/server/src/api/admin.rs`, which already runs under `set_tenant_context`.

**Tenant scope:** the new test also checks that a broker bound to another tenant gets
`NotFound` for the instance, both for `load_async` and for `transition`.

**Status:** EXEC-011 stays `BLOCKED_EXTERNAL` (implementation complete). The provider-sandbox
tier of acceptance 3 still needs the `QUANSIO_TEST_CONNECTOR_*` credentials. No transient FAIL
was committed (fix branch, merge commit, then a progress commit).

**Verification** (a private database `quansio_fix_exec011_rls` on the canonical 55440 server,
because the shared `quansio` database carries another session's unmerged migration 20):
- At the fix commit `dec5d81`: `cargo test -p quansio-server --no-fail-fast` passed (51 test
  binaries, 421 tests). `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --all --check` are clean.
- At `de2d3aa` (the fix merged with main `24dc5fe`, which carries the APP-001 API RLS fix):
  `connector_broker` 5/5, `api` 17/17, `schema_bootstrap` 5/5.
- `bash scripts/ci/ci.sh` at `de2d3aa`, run from a detached worktree outside the checkout:
  11/11 gates PASS (`ci-summary.json`, `ci-pipeline.log`). The toolchains gate ran the new
  test, and the Rust workspace passed 118 test binaries (696 tests).
