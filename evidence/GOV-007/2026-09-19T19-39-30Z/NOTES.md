# GOV-007 — the dev-stack suite must not rewrite the developer's `.env`

## Defect

`tests/integration/test_dev_stack.py::run_dev` ran `scripts/dev/{up,healthcheck,seed,reset}`
with `QUANSIO_DEV_COMPOSE_PROJECT=quansio-dev-test` and the offset ports in the process
environment, but without `QUANSIO_DEV_ENV_FILE`. `scripts/dev/_common.sh::write_env_file`
therefore refreshed the developer's gitignored `$ROOT/.env` with those values on every run
(credentials preserved, everything else follows the process environment). Since
`scripts/ci/ci_summary.py::dev_database_url` derives `QUANSIO_TEST_POSTGRES_URL` from that
file, the next DB-backed test run or `quansio-server` start targeted whichever stack the
suite had left, and the `crates/server` MinIO tests (canonical 59010) failed whenever the
canonical `quansio-dev` stack was down. Seen 2026-09-18 (INT-002) and 2026-09-19 (APP-003).

At the start of this session the primary checkout's `.env` read
`QUANSIO_DEV_COMPOSE_PROJECT=quansio-dev-test`, `QUANSIO_DEV_POSTGRES_PORT=56440`,
`QUANSIO_DEV_MINIO_PORT=59110` (sha256 e4612c9d…), with both compose projects running.

## Fix (de6937f)

- `dev_env_file` (module-scoped fixture): `tmp_path_factory.mktemp("dev-stack") / ".env"`.
  `run_dev` exports it as `QUANSIO_DEV_ENV_FILE`, which `_common.sh` already honoured;
  `_minio_bucket_present` passes the same file to `docker compose --env-file`. One file per
  module, so the second `up` exercises `write_env_file`'s upgrade path as well as create.
- `test_bootstrap_smoke` asserts the generated file exists and carries the suite's project
  and ports (the override reached the scripts).
- `developer_env_file_untouched` (autouse, function-scoped) snapshots `$ROOT/.env` bytes
  (or its absence) before each test and asserts identity after — the requested regression
  test, applied around `test_bootstrap_smoke` and `test_reset_then_reseed_is_deterministic`.
- `_common.sh` comment and `infra/compose/README.md` explain `QUANSIO_DEV_ENV_FILE`.

No product code, contract, store or migration is touched.

## Verification (this nested worktree, `ROOT` = the worktree, which has no `.env`)

- `guard-catches-prefix-run-dev.log` — pre-fix `run_dev` (temporary edit, not committed),
  create case: `scripts/dev/up` created `<worktree>/.env` with `quansio-dev-test`; the guard
  errors at teardown ("was rewritten by this test") and the positive check fails because the
  suite's own file was never written. Exit 1.
- `guard-catches-prefix-run-dev-existing-env.log` — pre-fix `run_dev`, modification case: a
  canonical-valued `.env` (quansio-dev, 55440, 59010; sha256 ee63410e…) staged in the
  checkout came out as quansio-dev-test/56440/59110 with sha256 e4612c9d… — byte-identical
  to the developer's clobbered file. Guard errors at teardown. Exit 1.
- `pytest-dev-stack.log` — fixed suite, no `.env` in the checkout: 2 passed; none created;
  the suite's file landed in the pytest temp dir with the test project and ports; the
  primary checkout's `.env` hash unchanged (e4612c9d… before and after).
- `pytest-dev-stack-existing-env.log` — fixed suite with the canonical-valued `.env`
  staged: 2 passed; sha256 ee63410e… before and after.
- `pytest-tests-gate.log` — the ci.sh `tests` gate (`uv run --project python pytest tests -q`)
  with `QUANSIO_TEST_POSTGRES_URL` on the canonical stack: 281 passed, 1 failed —
  `tests/contract/test_contracts.py::test_generated_bindings_are_current`, the known
  `scripts/dev/contract-gen` cargo-workspace resolution failure under `.claude/worktrees`
  (an environment property of nested worktrees, see evidence/CORE-006/2026-09-18T08-56-07Z;
  unrelated to this change). Re-run on `main` from the primary checkout after the merge.
- `python3.12 scripts/validate_v81.py` — PASS.
- ruff/mypy over the module: the only diagnostics are pre-existing (identical count on
  `main`); root `tests/` is outside the `python/` ruff/mypy CI scope.

## Restoring a developer's `.env`

`bash scripts/dev/up` with no overrides refreshes the non-credential values back to
`config/dev.yaml` (`quansio-dev`, 55440, 54230/54231, 59010/59011, 59020) and keeps the
existing credentials.
