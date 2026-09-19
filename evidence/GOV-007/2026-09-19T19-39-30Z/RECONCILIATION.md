# GOV-007 regression — reconciliation and plan (fix/GOV-007-dev-stack-test-env-file)

## Regression

`tests/integration/test_dev_stack.py` rewrote the developer's gitignored `.env` with the
suite's compose project (`quansio-dev-test`) and offset ports (Postgres 56440, MinIO 59110)
after every run. Observed 2026-09-18 (evidence/INT-002/2026-09-18T06-37-45Z/NOTES.md,
"`.env` had been left pointing at the test project since 2026-09-16") and 2026-09-19
(evidence/APP-003/2026-09-19T18-40-42Z/NOTES.md). Exposed by every commit since the suite
was added at 1614dd1 / 7add8c5 (the `write_env_file` refresh behaviour that makes the
overwrite unconditional landed at 4af68c9).

Failing observable: after the suite, `scripts/ci/ci_summary.py::dev_database_url` derives
`QUANSIO_TEST_POSTGRES_URL` from the test stack, and `crates/server` MinIO tests (canonical
59010) fail whenever the canonical `quansio-dev` stack is down.

## Canonical owner

GOV-007 `paths`: `scripts/dev/`, `infra/compose/`, `config/`; its declared test module is
`tests/integration/test_dev_stack.py`. The defect is in the test's process environment, not
in the scripts: `scripts/dev/_common.sh` already honours `QUANSIO_DEV_ENV_FILE`
(`ENV_FILE="${QUANSIO_DEV_ENV_FILE:-$ROOT/.env}"`, used by GOV-005's CI simulation), and
the test simply never set it.

## Existing code and coverage

- `scripts/dev/_common.sh::write_env_file` — creates the env file, or refreshes every
  non-credential key from the process environment (credentials preserved). Correct for a
  developer's stack; it is the caller's job to name a different file for a different stack.
- `tests/integration/test_dev_stack.py::run_dev` — sets `ISOLATION` (project + ports) but
  not `QUANSIO_DEV_ENV_FILE`. `_minio_bucket_present` passed `ROOT/.env` to
  `docker compose --env-file`. GENUINE_GAP for the env-file isolation; no test asserted the
  developer's `.env` survives the suite.
- `scripts/ci/ci_summary.py::dev_database_url` — reads `$ROOT/.env`; unchanged (it is
  correct once `.env` stays canonical).

## Persistent state / contracts / effects / capabilities / migrations

None. No canonical store, DOMAIN.md contract, event or effect class is touched; no
migration. The only durable state involved is the developer's gitignored `.env`, which the
fix stops touching.

## Real boundaries

Real Docker daemon (skips `BLOCKED_EXTERNAL` without it), as before. `real_boundary=false`
for the task.

## Rollback / recovery

Test-only change plus comments/README; reverting restores the previous behaviour. A
developer whose `.env` was already rewritten restores it with `scripts/dev/up` (no
overrides), which refreshes the non-credential values from `config/dev.yaml`.

## Plan

1. `dev_env_file` module-scoped fixture: `tmp_path_factory.mktemp("dev-stack") / ".env"`;
   `run_dev(..., env_file=)` exports it as `QUANSIO_DEV_ENV_FILE`;
   `_minio_bucket_present(env_file)` uses the same file.
2. Positive check in `test_bootstrap_smoke`: the generated file exists and carries the
   suite's project and ports.
3. Regression guard `developer_env_file_untouched` (autouse, function-scoped): `$ROOT/.env`
   bytes (or absence) identical before and after each test.
4. Negative evidence: the guard against the pre-fix `run_dev`, for both the create case
   (no `.env` in the checkout) and the modification case (canonical-valued `.env` staged).
5. `_common.sh` / `infra/compose/README.md`: say what `QUANSIO_DEV_ENV_FILE` is for.
6. Gates: dev-stack suite, root `pytest tests`, validator; evidence; progress notes; merge.
