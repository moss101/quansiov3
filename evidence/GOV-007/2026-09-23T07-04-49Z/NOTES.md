# GOV-007 regression: dev database whose init scripts Docker skipped

**Found:** APP-010 CI on `task/APP-010-routines` at c9f2dd3 (and b154d2a) failed the
`tests` gate only on `tests/integration/test_dev_stack.py::test_reset_then_reseed_is_deterministic`:
`scripts/dev/up` after `scripts/dev/reset` never became ready.

**Cause:** Docker Desktop can start a fresh Postgres container before the
`infra/compose/initdb` bind mount is populated. The entrypoint then logs
`ignoring /docker-entrypoint-initdb.d/*`, initialises the empty volume without running
the scripts, and never runs them again for that volume. `pgvector` is missing, so
`check_postgres` (which requires `vector` in `pg_extension`) never reports ready and the
reset/up cycle times out. Nothing in the product code is involved.

**Fix (7d41a6e):** `scripts/dev/_common.sh::check_postgres` — when Postgres accepts
connections but the extension is absent, it applies the init scripts (idempotent:
`CREATE EXTENSION IF NOT EXISTS`, `CREATE SCHEMA IF NOT EXISTS`) inside the container
with `ON_ERROR_STOP` and re-checks. A genuinely broken init still fails readiness.

**Test:** `test_a_database_whose_init_scripts_were_skipped_is_repaired` brings the test
stack up, drops the `vector` extension (the state Docker leaves behind), runs the
healthcheck and asserts the extension and the `derived` schema are present.
Against the pre-fix probe it fails (93 s timeout, `repair-test-against-prefix-probe.log`);
with the fix the dev-stack suite passes (3 passed) and the full `tests` gate passes
(286 passed).
