# GOV-005 fix — GitHub Actions CI has never passed; broken postgres `services:` block

User report: "ci is failing due to — Node.js 20 is deprecated ... actions/upload-artifact@v4".
That annotation is real but harmless (GitHub's own runner-side Node 20→24 shim
for the action's bundled runtime, unrelated to this repository, not touched
here). Checking the actual run (`gh run view --log-failed`) found the real
failure: **CI has failed on every one of its last 100 recorded runs** (97
failure, 3 cancelled, 0 success — `gh run list --limit 100`), all at the same
step, in well under the time the pipeline actually takes to run (13–29s
against a pipeline that locally takes minutes), confirming it was failing in
job setup, before any real work started.

## Root cause

```
X Exit code 125 returned from process: file name '/usr/bin/docker', arguments
'create --name ... -p 55440:5432 --health-cmd "pg_isready -U quansio" ...
--shm-size 1g -c max_locks_per_transaction=1024 -c max_connections=100 ...'
```

`d9c529d` ([EXEC-008] evidence for the cancel-once repair and the lock-table
sizing) added `-c max_locks_per_transaction=1024` / `-c max_connections=100`
to the `postgres:` service's `options:` field in `.github/workflows/ci.yml`,
matching the reasoning already correctly applied to `infra/compose/compose.yaml`
(a larger lock table needs a larger shared-memory segment; see that file's own
comment). But GitHub Actions' `services:` `options:` maps **only** to `docker
create` OPTIONS — flags that appear *before* the image name
(`docker create [OPTIONS] IMAGE [COMMAND] [ARG...]`) — never to a container
COMMAND override. `-c` is not a `docker create` option Postgres would ever see;
it resolves to `--cpu-shares`, which requires an integer, so `docker create`
refused `-c max_locks_per_transaction=1024` outright with exit code 125 at
"Initialize containers" — before `actions/checkout` even runs. Every commit
since has been reported "green" only by local `bash scripts/ci/ci.sh` runs;
GitHub itself has been red the entire time.

## Fix: stop duplicating the dev stack, use the one that already exists

`infra/compose/compose.yaml` already applies these two flags *correctly*, via
docker-compose's `command:` field (which, unlike GitHub Actions' `options:`,
does let you override CMD):

```yaml
postgres:
  command: [postgres, -c, max_locks_per_transaction=1024, -c, max_connections=100]
  shm_size: 1g
```

And `scripts/dev/healthcheck`'s own comment already says it is *"used by
`scripts/dev/up` and CI"* — the workflow's native `services:` block was never
the intended mechanism; CI was always meant to run the identical deterministic
stack GOV-007 built for local development, not a second, hand-maintained
container definition that could (and did) drift out of sync and break.

Fixed by deleting the `services:` block and adding one step,
`bash scripts/dev/up`, right after toolchain installation.
`scripts/ci/ci_summary.py`'s existing `dev_database_url()` already auto-detects
the `.env` file `scripts/dev/up` writes and derives `QUANSIO_TEST_POSTGRES_URL`
from it (an explicit env value still always wins, per its own docstring) — so
the hand-typed DSN previously set on the pipeline step is removed too, closing
the exact kind of duplication that caused this bug: two independently
maintained descriptions of "how Postgres is configured for tests," one correct
(`compose.yaml`) and one wrong (`ci.yml`'s own `services:` block). Bumped
`QUANSIO_DEV_HEALTH_TIMEOUT` to 180s for cold image pulls on a fresh runner
(local default: 90s, tuned for a machine with warm image caches).

## Verification

Simulated a clean CI runner locally rather than trusting the mechanism on
paper — completely fresh state (`QUANSIO_DEV_ENV_FILE=/tmp/ci-sim/.env`, a
separate compose project and ports), so the real local dev stack
(`quansio-dev`, `quansio-dev-test`) was untouched throughout (confirmed still
running afterward via `docker ps`):

- `bash scripts/dev/up` reached `dev stack: HEALTHY` from nothing —
  `dev-up-simulation.log`.
- `docker exec <sim-postgres> psql ... SHOW max_locks_per_transaction; SHOW
  max_connections;` → `1024` / `100` — the exact settings this whole fix
  exists to apply, confirmed *actually applied* on the running server, which
  the old broken `services:` block never achieved even on the commits before
  it started crashing — `postgres-settings-check.log`.
- `cargo test -p quansio-server --test schema_bootstrap` — 5/5 against that
  simulated stack — `schema-bootstrap-against-sim.log`.
- Simulation torn down cleanly (`docker compose ... down -v`); confirmed
  `quansio-dev-*` and `quansio-dev-test-*` containers still running,
  unaffected.
- `tests/ci/` (55 tests, including
  `test_workflow_runs_the_same_pipeline_and_uploads_the_summary`, which parses
  this exact file) — 55/55, `test-ci-suite.log`.
- `python3 -c "import yaml; yaml.safe_load(...)"` — the edited workflow file
  parses as valid YAML — `yaml-sanity.log`.

Not independently re-verifiable from this sandbox: an actual `push` to GitHub
and a real Actions run (this environment has no GitHub Actions runner). The
fix is pushed on this commit; the next real run against `origin/main` is the
authoritative confirmation.

## Files changed

`.github/workflows/ci.yml` only — removed the `services:` block (20 lines),
added one `bash scripts/dev/up` step (16 lines), removed the now-redundant
hand-typed `QUANSIO_TEST_POSTGRES_URL`.
