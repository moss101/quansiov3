# APP-001 regression fix: the public command surface under row-level security

**Found by:** OPS-001 reconciliation (2026-09-24). An in-process router on a pool that runs
`SET ROLE quansio_app` answered `POST /v1/commands/CreateServicePrincipal` with
500 `new row violates row-level security policy for table "commands"`.

**Owner:** APP-001. Its paths are `crates/server/` and `crates/server/api/`, and its build item
requires the API to expose endpoints "with auth, tenant scope, idempotency". The `commands` log is
the one table the API owns (`api.rs` ownership scan).

**Failing tests before the fix** (red run on the unfixed tree, at base `c207b11`):

- `cargo test -p quansio-server --test api the_public_surface_works_as_the_application_role_under_rls`:
  the first `PostMessage` answered
  `{"code":"INTERNAL","message":"error returned from database: new row violates row-level security policy for table \"commands\""}`
  (500, expected 200).
- `every_statement_the_api_runs_is_inside_a_tenant_scoped_transaction` listed 13 sites:
  - 8 in `commands.rs`: the log INSERT, the applied UPDATE, the rejected UPDATE, the replay,
    `thread_of_run`, the legacy teammate-thread read, `load_work_node` and `load_target`;
  - 1 in `projections.rs`: the run's capability projection;
  - 3 `acquire()` sites in `artifacts.rs`: upload, download and evidence metadata;
  - 1 `acquire()` site in `trust.rs`: evidence bytes.

**Cause.** Row-level security is `FORCE`d on every tenant table. The tenant setting is
transaction-local (`set_config('quansio.tenant_id', $1, true)`), so two kinds of statement run
with no tenant:

- **A statement on the bare pool.** As `quansio_app` it may write nothing: the INSERT is refused,
  and an UPDATE silently matches no row, so a command stays `accepted` forever. It reads nothing,
  so every handler read returned empty: no thread for an answered question, no legacy teammate
  thread, "the workspace has no work node", no execution target, and a `null` capability
  projection.
- **A pooled connection used outside a transaction.** The artifact and evidence stores do set the
  tenant on the connection they are given. With no transaction open, though, that setting lapses
  as soon as its own statement ends.

The development stack connects as a superuser, which bypasses the policies, so no existing API test
saw either kind.

**Fix.** `ApiState::tenant_tx(tenant_id, correlation)` begins a transaction and sets the tenant.
Every one of the 13 sites now runs in one:

- the command record still commits before the owner is called, so a resend while the owner works
  is still told to come back;
- an upload's artifact and version commit together, before the search index learns of them;
- `fetch_version` no longer sets the context on its connection itself, because its caller's
  transaction has already done it.

The readiness probe (`SELECT 1`) names no table and stays on the pool. The owners reached along the
way (conversation, collaboration, teammates, runtime, capability projection, policy, artifacts,
evidence) needed no change under `quansio_app`.

**Guard.**

- `the_public_surface_works_as_the_application_role_under_rls` runs on one router whose pool is
  `quansio_app`. It drives:
  - `PostMessage`, routed to the default teammate, bound to the workspace's target, projected
    with the tenant policy and parked on a question;
  - its replay;
  - `AnswerQuestion`, whose reply lands in the run's thread;
  - `PostMessage` in a workspace with a legacy teammate thread;
  - `CancelRun`, then a refusal and its replay;
  - `CreateArtifact` and its bytes;
  - evidence metadata and evidence bytes.

  The command log is then read as the superuser: five commands `applied`, one `rejected`.
- `every_statement_the_api_runs_is_inside_a_tenant_scoped_transaction` scans `src/api` for
  executors on the bare pool and for `acquire()`.

**Mutation checks** (`mutation-checks.log`; every mutant built first, logged `COMPILES`):

- 13 "transaction without a tenant" mutants: each `tenant_tx` call site in turn, plus the helper
  pointed at another tenant.
  - 10 were KILLED.
  - 3 survived, and are correct as they stand: the upload, evidence metadata and evidence bytes.
    Their stores set the tenant on the caller's connection, so what the API owes them is a
    transaction.
- For those three sites and the download, a "revert to a bare pooled connection" mutant was run
  (`mutation-checks-bare.log`). All 4 were KILLED.

**Also found, out of scope:** `control/connectors::load_async` reads `connector_instances` on the
bare pool (owner EXEC-011). It is raised as a separate task, not fixed here.

**Verification** (`summary.json` names the commands and logs):

- `cargo test -p quansio-server --no-fail-fast`: 52 binaries, 422 passed.
- `bash scripts/ci/ci.sh` on the fix commit: 11/11 gates PASS (`ci-summary.json`). It ran from a
  detached worktree outside the checkout, because in a nested worktree the contract-gen step fails.
- The database was a private one on the dev server (`quansio_fix_app001_rls`). The shared `quansio`
  database already carries OPS-001's unmerged migration 20.
- The first `ci.sh` run failed only `toolchains` (`ci-summary-run1.json`,
  `ci-run1-qworkerd-browser.log`):
  - The failing test was `quansio-qworkerd --test browser`, in
    `the_dom_is_read_first_and_a_screenshot_only_when_it_says_nothing`, with
    `open a page: Unreachable("127.0.0.1:…: Connection refused")`.
  - That is the headless browser's debugging port not up yet while another project's build loaded
    the machine. The crate does not touch `src/api`.
  - Run alone, the test passed 3/3.
  - The unchanged commit then passed the full pipeline.
