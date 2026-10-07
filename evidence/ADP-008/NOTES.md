# ADP-008 notes

What ran for real, what did not, what was deferred, and the traps worth keeping. `RECONCILIATION.md` and `PLAN.md` sit beside this file.

## What ran for real

- **PostgreSQL.** Every server suite creates and drops its own database (migration 0029 applied by the suite). `pack_install` also runs the
  whole install, report and rollback flow as the least-privileged role `quansio_app` (row-level security applies; a bare connection sees no
  row in either table; another tenant sees nothing).
- **The real Python plane.** `pack_install` and `intelligence_gateway` start `python -m intelligence.server` as a child process and talk gRPC
  to it: `CompilePack` (verify and compile), and `FulfillModel` refusing a host-only tool. The plane is also killed under the installer (its
  listener, not just `uv`) to prove "no plane" fails closed at `compile`. `python/tests/capability_compiler/test_pack_install.py` runs the
  same RPC through an in-process gRPC server.
- **The real dispatch path.** `tool_visibility` and `outputs_preview` drive the engine (`create_run` .. `run_turn`) and the real
  `ToolDispatchService`: capability projection, policy, Effect Ledger, host. A model attempt, a host call, a host call with an empty
  projection and a host call to a model-only tool are all rows in `tool_calls` with their caller, `tool.rejected` events and effect records
  read back from the database. `outputs_preview` runs the host-only `webdev.preview_scopes` against the real loopback preview listener.
- **The API.** `pack_install::the_commands_and_the_read_work_over_the_api_and_leak_nothing` posts `InstallPack` / `RollbackPack` through the
  router (idempotent replay, `CONFLICT_REVISION`, `PACK_INSTALL_FAILED`, 400s, 403 for a non-administrator) and reads `GET /v1/packs`.
- **A stopped install.** The install's future is aborted between `compile` and `activate` (what a crashed server leaves): the previous revision
  is still active, the report is `running` at `compile`, a restarted store closes it as interrupted, the same command id replays the failure
  and a new one installs.

## Not exercised live

- **A second `quansio-server` process racing the first.** Concurrency is two installs on one pool (the second completes while the first waits
  between phases), serialized by the pack row's lock. A crash *inside* the activation transaction is not injected; atomicity is the single
  transaction, and the aborted-between-phases test shows nothing leaks across phases.
- **A host call that parks for approval and resumes.** `tool_calls.caller` is persisted and read back by `tool_call_by_effect`, but no
  host-callable tool in the registry needs an approval, so the parked-host-call resume path has no test.
- **The desktop Knowledge section in a browser or Electron.** Tested as pure decoders and controls (vitest) and compiled with `tsc` and
  `eslint --max-warnings 0`; no jsdom in the repository, so clicks and focus are not driven. The IPC handler `packs.rollback` is exercised
  only through the preload contract test.
- **The Rust contract generator from a worktree.** `test_generated_bindings_are_current` is deselected here; the Rust bindings were generated
  in a throwaway detached worktree outside the primary checkout and copied back. It runs in the finalize dry run.
- **A real model.** None is involved (`real_boundary` is false).

## Deferrals (recorded, not hidden)

- **Standalone `InstallSkill` has no revision and no install report.** A skill version is already immutable per semver and climbs the §11.5
  ladder; the pack's content (its `skill_version_ids`) is what an install pins. Giving a lone skill install the same phases and report is a
  separate change; this task covers capability packs, which are what "install by revision" can mean in the repository today (there was no
  server-side pack install at all: the CAP-005 lifecycle was in memory).
- **`SubmitPack`, `QualifyPack`, `PublishPack` stay catalog-only.** `InstallPack` is the one install path: the pack ships its governance
  record and the plane checks it (CAP-004 credential rule, OPS-007 quarantine record, CAP-005 gate). The authoring flow from compiler
  candidates to a published pack is unchanged and still has no handlers.
- **Per-tool subject tokens** (a tool that must run under the person's token instead of the run's) are out of scope: a host call runs under
  the run's own projection, as a model's does. Stated in DOMAIN 7.5.
- **No public command calls a host-only tool.** The host path is `ToolDispatchService::dispatch_as_host` (a trusted runtime caller); there is
  no `CallHostTool` command, because that would be a new public authority. `webdev.preview_scopes` has no desktop caller yet.
- **No tool-search, MCP or SDK tool export exists** in this build (ADP-009 adds MCP). The single filter (`exposed_*`, `model_visible_names`)
  is what they must use; a source scan fails if any crate but `crates/tools` reads `declarations()` / `callable_names()`.
- **A recovered (interrupted) report emits no event.** `pack.install_failed` is emitted by the install that fails; closing a stale `running`
  report is a sweep, not a state change of the pack.
- **`builtin` source** reads from `install.builtin_root` (or `QUANSIO_PACKS_ROOT`) on the server's own disk; a deployment that does not ship
  `packs/` fails at `fetch`, typed.

## Traps worth recording

- **A composing test migrates the database named by `QUANSIO_TEST_POSTGRES_URL` itself.** `api.rs::the_deployable_composes_from_the_environment`
  applied the unreleased 0029 to the shared dev database during this task; it was undone by hand (drop the objects, delete
  `_sqlx_migrations` row 29) and every later run used a private database on the same server. Point the URL at a private database *before* the
  first server test of a branch with a migration.
- **`Service::drop` in `tests/common/intelligence.rs` kills `uv`, not the Python server it started**, which keeps listening. A test that
  needs the plane gone must kill the listener (`lsof -iTCP:<port> -sTCP:LISTEN -t`); this suite does, in `stop_plane`, and a plain `drop`
  made the "no plane" case install successfully.
- **The QA-002 composition gate flags a trait whose only production implementation is a `No*` default** (`PhaseObserver` / `NoObserver`
  was `DEFAULT_ONLY`). A test seam between phases is a `PhaseHook` closure type, not a trait.
- **`observability::redact::redact_text` corrupts non-ASCII text** (it rebuilds the string byte by byte as `char`s). Pack diagnostics use only
  its canary list, not the function.
- **A report's pack name is text a person chose.** It is stored only when it is a plain name (`report_name`), else `(invalid name)`; the plane
  echoes file paths in finding pointers, so pointers are redacted too.
- **`uv run ... python -` with a heredoc, `source`, `cd ..` compounds and `git -C` are refused by the harness**; edits were made with scripts run
  by path.
- **The Python full run, not the targeted one, caught `test_the_production_gateway_offers_the_registry_schemas`** (it compared the offered
  schemas to every declaration). Run the whole `pytest` before closing a change to `config/tools.yaml`.
- **`cargo fmt` reflowed lines the mutation patterns named**: four mutants reported `NO-VERDICT: pattern found 0 times` and were re-run with
  the formatted text (the log keeps both).

## Mutation checks

`mutation-checks.py` breaks 31 guards on purpose: the registry's model-visible filter, the model-visible ceiling, the latest-version rule,
the caller check, the default caller, unknown keys, the "nobody can call it" rule, the gateway's schema skip, the recording compile's tool
list, the dispatch caller (host treated as model; refused model attempt recorded as host), the plane's host-only tool check, credential scan,
revision over governance and over files, the runtime's revision (against the plane's), the plane verdict, the expected-active check,
retention, retirement of the replaced revision, report expiry (by id and by sweep), closing interrupted reports, redaction of messages,
pointers, home directories and URL credentials, the diagnostic count bound, the report's pack name, the administrator-only read and the
strict reading of the plane's answer. A mutant counts only when it compiles, its control passed and a named test then fails.
All 31 are killed in the final script. The first run's log also holds one survivor (replaced, below) and four stale-pattern `NO-VERDICT`s that were re-run and killed.

- **The survivor is equivalent by layering and is replaced, not hidden:** removing `expires_at > now()` from the `reports()` SELECT survives
  because the same transaction's sweep has just deleted every expired row. The filter on the by-id read, where no sweep runs, is the one
  with a test (`an-expired-report-is-read-by-id`, killed).
