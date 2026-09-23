# APP-001 — regression: a command_id reused by another tenant failed with 500

**Failing test / what exposed it.** Found by review on 2026-09-23 against `main` at 6f88144:
`api/commands.rs` stored the client's `command_id` as the `commands` row's primary key. The new
`two_tenants_may_send_the_same_command_id_and_each_replays_its_own` in
`crates/server/tests/api.rs` reproduces it: tenant B's first `CancelRun` with a `command_id`
tenant A had already used returned `500 INTERNAL` ("duplicate key value violates unique
constraint \"commands_pkey\"", `retryable: true`) — `cargo-test-api-before-fix.log`.

**Cause.** `commands` has `id TEXT PRIMARY KEY` and `UNIQUE (tenant_id, command_id)`; the insert
bound the client's `command_id` into both columns with `ON CONFLICT (tenant_id, command_id) DO
NOTHING`. A cross-tenant duplicate hits the primary key, which is not the conflict target, so the
insert errors instead of either applying or replaying. One tenant could break another tenant's
idempotent retry and, from the 500 versus a fresh result, learn that the id existed. DOMAIN.md
§1.2 makes `command_id` unique per tenant, and §1.1 says entity ids are minted by the owning
authority, never by clients.

**Fix** (`fix/APP-001-tenant-scoped-command-rows`, a9da8f6). The handler mints the record's own
`cmd_` id (`CanonicalId::generate(Prefix::Command, …)`); `command_id` stays the per-tenant replay
key, and replay, mismatch and the applied-result update are still keyed on `(tenant_id,
command_id)`. **No migration:** the schema already has the right constraint; rows written before
the fix keep `id = command_id`, which is unique, and a freshly minted ULID cannot collide with
them. The test covers that case with a record in the pre-fix shape.

**Test strengthened (CORE-002).** `duplicate_commands_replay_and_param_changes_conflict` in
`crates/server/tests/command_idempotency.rs` inserted its duplicate with the same row id as the
original, so the "database rejects a duplicate (tenant, command_id)" assertion passed on the
primary key and could not see this defect. Each insert now gets its own minted id, so the
refusal must come from the per-tenant constraint.

**Verification.** The new test was red before the fix and green after. `cargo test -p
quansio-server` passed every binary (api 13, command_idempotency 2). fmt and clippy were clean.
`validate_v81.py` passed. No other crate depends on `quansio-server`.
`QUANSIO_TEST_POSTGRES_URL` pointed at a private scratch database (`quansio_fix_app001_cmd`) on
the canonical 55440 stack: `the_deployable_composes_from_the_environment` migrates the DSN's own
database, and the shared `quansio` database already carries an unmerged migration 17 from the
concurrent APP-015 session. `bash scripts/ci/ci.sh` was not run. It cannot pass from a nested
worktree (contract-gen), and a fresh detached worktree build did not fit in the 7 GiB of free
disk.
