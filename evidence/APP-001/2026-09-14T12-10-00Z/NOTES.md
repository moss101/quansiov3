# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

**This is the first unit of APP-001, not the whole task.** The registry entry is `IN_PROGRESS`, and the
remaining work is named at the end of this file rather than implied.

## acceptance

**Every mutating endpoint calls the canonical owner rather than writing foreign tables.** The one mutating
endpoint this unit ships is `POST /v1/commands/CancelRun`, and it cancels a run by asking the runtime — the
module that owns a Run's lifecycle — and reporting the runtime's own answer. It writes exactly one table of
its own: `commands`, the command log §1.2's idempotency is defined over. The test drives it end to end
against real PostgreSQL and asserts the run's status in the *owner's* table changed, which is what
distinguishes "the owner did it" from "the handler wrote a column".

The half of that acceptance a handler's output cannot show is checked structurally:
`an_api_handler_reads_and_writes_only_its_own_table` scans `crates/server/src/api/` for `INSERT INTO`,
`UPDATE` and `DELETE FROM` and fails on any statement that does not name `commands`. It asserts it actually
read the module, so a scan that silently found nothing cannot pass.

**Generated clients pass contract tests.** Not verified in this unit: the existing `contract-lint-compat`
and `contract-drift` gates pass, but no OpenAPI-generated *client* tests were added for the new endpoints,
and that is named as remaining rather than counted.

**The walking-skeleton smoke test passes in CI with the conformance-stub provider.** Not done. It is the
next unit and the reason this task is not `PASS`.

## what this unit does ship, and how it is checked

- **§15's error taxonomy, held as a closed enum and checked against the generated table.**
  `the_error_vocabulary_is_the_one_the_catalog_generates` compares `ApiErrorCode::ALL` against
  `schemas/catalog/errors.yaml` **in both directions** — a code the table has and the enum does not is a
  missing refusal, and one the enum has and the table does not is an invented one. Either way the two
  disagree about what a client may be told, so either fails. `every_code_maps_to_a_status_and_says_whether_a_retry_could_help`
  checks every code's status and the shape's exact key set — a fifth key would be a contract change, so the
  assertion is on the count rather than on presence alone.
- **Tenant scope, with a missing credential and a malformed one kept apart.** No tenant is
  `AUTH_REQUIRED` (401); a tenant that is not a canonical `tn_` id is `VALIDATION_BOUNDS` (400), because a
  client needs to tell "I forgot" from "I mistyped". The test drives both, and also that a command outside
  the catalog is `NOT_FOUND` rather than a 500.
- **The §14 command catalog and §10 read projections, embedded at build time.** So a deployment cannot
  serve a catalog its own binary was not built from, and the test asserts the served catalog holds commands
  the surface must route to.
- **The composition root and the deployable.** `crates/server/src/composition.rs` opens one pool, applies
  the schema through its canonical runner, builds the API state, and `crates/server/src/bin/quansio-server.rs`
  is the binary. `the_deployable_composes_from_the_environment` drives it: a missing `QUANSIO_DATABASE_URL`
  is a startup refusal, and a real one composes.
- **Idempotency by `command_id`.** The decision and the record are one statement — `INSERT … ON CONFLICT
  (tenant_id, command_id) DO NOTHING` — so a concurrent replay cannot insert a second row and both callers
  see the row that won. The test asserts a resend returns the *same* result marked `replayed`, and that the
  same `command_id` with different parameters is `CONFLICT_IDEMPOTENCY_MISMATCH` rather than a second
  application.

## found while building, and worth recording

- **The catalog has no `CreateRun`.** The first draft of this unit implemented it. Writing a test against
  the served catalog is what caught it: the command that creates a run is `PostMessage` (a message creates
  its run) or `TriggerCreateObjective`-style work commands, and a handler for `CreateRun` would have been a
  handler for a command no client can send. The unit was retargeted to `CancelRun`, whose owner is
  unambiguous, and the handler refuses any catalog command it has no handler for rather than
  half-applying one.
- **`crates/server/tests/common::fresh_database` returns an *empty* database.** The schema is applied by
  the test through `quansio_server::control::schema::migrate`, the same runner the deployment uses. A test
  that forgets it fails with `relation "tenants" does not exist`, which is a better failure than a silent
  empty database, but it is worth knowing before writing the next suite.
- **`scratch_name` keys only on the process id**, so several tests in one binary sharing a prefix collide
  on `CREATE DATABASE`. Each test in the new suite takes its own prefix.
- **A `CREATED` run has no edge to `CANCELLED`.** The state machine draws `CREATED → QUEUED → RUNNING`, so
  the fixture walks the ladder through the runtime's own transitions rather than asking the API to make an
  illegal move — which is what a 409 from the first attempt meant.

## remaining for APP-001, named

1. The **walking skeleton**: one chat turn end to end over HTTP — command → runtime → model gateway with the
   conformance stub → tool proposal → Effect Ledger → projection → WebSocket event.
2. The **WebSocket surface** and its reconnect test.
3. **Per-tenant rate limits** (`RATE_LIMITED`) and **feature flags** from `config/flags.yaml`.
4. **OpenAPI-generated endpoints** for the whole §14 catalog and §10 read projections, and generated-client
   contract tests.
5. Handlers for the rest of the catalog, and read projections actually served.
