# CORE-006 — concurrent writes to one run's ProtocolState (regression fix)

## The failure

`crates/server/tests/turn_loop.rs::a_turn_executes_tool_proposals_through_capability_policy_and_the_effect_ledger`
panicked once under `bash scripts/dev/bootstrap.sh` (`cargo test --workspace`, every test binary in
parallel against the dev PostgreSQL) at `9eb2794`:

    turn: InvalidArgument("effect protocol state: protocol state store: error returned from database:
    duplicate key value violates unique constraint \"protocol_states_run_id_key\"")

and passed 3/3 alone (`evidence/INT-002/2026-09-18T08-19-07Z/ci-run1-turn-loop-flake.log`). HANDOFF.md
had carried the same symptom as an undiagnosed known defect since EXEC-008, then with
`protocol_states_pkey` in the message; RUN-011's `d6bad3a` (2026-09-16) changed the upsert's conflict
target from `run_id` to `id` and the message changed to `protocol_states_run_id_key`.

## Diagnosis: a real runtime bug in the store, not test isolation

- Every integration test creates and drops its own scratch database, so nothing is shared across
  binaries; the parallel workspace run only widens a timing window that exists in production.
- The turn loop issues the colliding writes itself: `Engine::dispatch_tool_calls` runs the independent
  calls of one round through `parallel::join_all`, and every Effect Ledger transition (`reserve`,
  `mark_dispatched`, `settle_*`, …) is its own transaction that ends in `sync_pending_tool_call`
  (`ProtocolStateStore::load` → modify → `store`). For a fresh run the two calls' `reserve`
  transactions perform the run's first-ever protocol-state write at the same moment.
- `protocol_states` has two unique indexes: the primary key `id` (deterministic, `pst_<run suffix>`)
  and `run_id UNIQUE`. PostgreSQL arbitrates `INSERT … ON CONFLICT (…) DO UPDATE` only on the index
  the clause names (`UNIQUE_CHECK_PARTIAL` on the arbiter, ordinary `UNIQUE_CHECK_YES` on every other
  unique index). Two concurrent inserters both pass the arbiter pre-check before either heap tuple is
  visible, both insert, and the loser raises on the index that was *not* named. Because `id` and
  `run_id` always collide together, naming either one leaves the other to raise — `d6bad3a` swapped
  the constraint in the error, nothing more.
- The same overlap has a quieter face: two unlocked load → modify → store cycles for one run make the
  second writer overwrite the first writer's `pending_tool_calls` entry. Each later transition re-adds
  its own entry, so the loss is transient, but between transitions the durable state that
  `next_safe_action` and `resume_pending_calls` decide from was missing a dispatched call.

`repro_protocol_state_race.py` (this directory) reproduces both against the real migrations, two
transactions per fresh run, 1000 runs per variant (`repro-race-variants.log`):

| variant | result |
|---|---|
| `ON CONFLICT (run_id) DO UPDATE` (before d6bad3a) | 112 × `protocol_states_pkey` |
| `ON CONFLICT (id) DO UPDATE` (shipped at 9eb2794) | 67 × `protocol_states_run_id_key` |
| claim (`INSERT … ON CONFLICT DO NOTHING`, no target) + `UPDATE … WHERE run_id` | 0 |
| unlocked load → append own call → store | 1000 × lost update |
| claim + `SELECT … FOR UPDATE` → append → store | 0 |

## The fix (commit `f0fcf58`, canonical owner `crates/server/runtime/protocol_state/`)

- `ProtocolStateStore::store`: claim the row with a target-less `INSERT … ON CONFLICT DO NOTHING`
  (every unique index arbitrates, so a concurrent first write waits for the other transaction and
  then updates), then replace the state with an `UPDATE` under the row lock. The claim selects from
  the tenant-visible `runs` row, so a run the tenant cannot see yields `NotFound` instead of a row
  created through the foreign key (constraint checks bypass row-level security).
- `ProtocolStateStore::lock(conn, tenant, run_id, generation) -> ProtocolState`: claim, then
  `SELECT … FOR UPDATE`. Serializes read-modify-write cycles for one run for the rest of the
  caller's transaction, whether or not a row existed.
- Callers whose cycles overlap an in-flight turn now use it: `effects/ledger.rs::sync_pending_tool_call`
  (RUN-007), `policy/store.rs::append_pending_approval` (RUN-006), `RunStore::cancel_once` (RUN-001 —
  a user cancellation must not be overwritten by a settling tool call, and is now recorded even when
  the run had no protocol state yet), and the engine's `park_*` and the dispatcher's
  `park_for_approval`, whose load-in-one-transaction/store-in-another pairs became
  `RunStore::update_protocol_state` (one transaction under the lock; RUN-001, RUN-011).
  `RunStore::store_protocol_state` stays as the wholesale seeding write used by tests.
- Lock order is unchanged in shape and acyclic: [effect_records | runs | approval_requests] →
  `protocol_states` row → tenant event-sequence row (taken by `commit_staged` after every mutation
  closure). `sync_pending_tool_call` is the last lock-taking step of every ledger mutation.
- Not changed: `scheduler/wait.rs` (`register_in`/`resolve_in`), the release-wait path in
  `RunStore` and the child-join path in `agents/store.rs` still read with `load`. They run only for a
  parked run (no turn in flight), so no concurrent writer exists by construction; the primitive is
  available to them if that changes.
- No migration; committed row contents are identical to the upsert's. No contract or event change.

## Tests

- New in `crates/server/tests/protocol_state.rs`:
  `concurrent_first_writes_for_one_run_never_raise_and_keep_one_row` (120 rounds of two
  barrier-aligned first writes; asserts no error, one row, a whole-state write),
  `concurrent_cycles_keep_every_writers_change` (120 rounds of two `lock` → append → `store`
  cycles; asserts both pending calls survive), and
  `lock_and_store_fail_closed_for_foreign_and_unknown_runs` (negative: unknown run and another
  tenant's run yield `NotFound` and create nothing).
- Against the unfixed store (statement reverted, `lock` as a plain load) the two race tests fail:
  11 unique violations on `protocol_states_run_id_key` in 120 rounds and a lost update in round 0
  (`regression-tests-on-unfixed-store.log`). With the fix: 8/8 in the suite.
- `cargo test -p quansio-server --test turn_loop` ×25: 25/25 (`turn-loop-repeated.log`).
- `cargo test --workspace` ×2 (the condition that exposed the flake): 102 suites, 589 passed,
  0 failed each time (`cargo-test-workspace-run{1,2}.log`).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `bash scripts/ci/ci.sh` (`ci.log`): 8 of 11 gates PASS in this worktree; `contract-drift`,
  `toolchains` and `tests` fail at one and the same step, `scripts/dev/contract-gen` failing to
  build because this worktree lives inside the primary checkout (`.claude/worktrees/…`): the
  package is `workspace.exclude`d, so cargo walks up past the worktree's `Cargo.toml` to
  `/Users/mohsin/projects/quansiov3/Cargo.toml` and reports "current package believes it's in a
  workspace when it's not". That is an environment property of nested worktrees, not of this change,
  which touches no contract or schema; the same gates passed on `main` at `9eb2794`
  (`evidence/INT-002/2026-09-18T08-19-07Z/ci.log`). The gates behind that step were therefore run
  individually here and pass: the Rust toolchain (above), the Python plane — ruff, ruff format, mypy,
  pytest 444 passed / 82 skipped (`toolchains-python.log`) — and the TypeScript workspace, the
  architecture check and the macOS bridge (`toolchains-typescript-native.log`); the `tests` gate's
  own suite is 280 passed with only `test_generated_bindings_are_current` failing on that build
  step. Re-run on `main` at the merge commit `90e0aee` in the primary checkout (`ci-main.log`):
  `contract-drift` now passes, but three gates fail for two reasons, neither in the fix. (a)
  `supply-chain` and four `tests/ci/test_supply_chain.py` cases scan the checkout's tree and find the
  nested worktree's copy of the deliberately non-compliant fixture
  `.claude/worktrees/…/tests/ci/fixtures/supply_chain/skills/self-promoted/skill.json`; the scan
  does not skip `.claude/worktrees`, so any nested worktree contaminates a run from the primary
  checkout. (b) `toolchains` (via `uv run … dossier_consistency.py`) and two `tests` cases rejected
  this bundle's first `summary.json`, which carried a top-level `regression` key and per-command
  `note` keys that `schemas/json/evidence-summary.schema.json` forbids — a defect of the bundle,
  corrected in the commit that records this paragraph (the notes now live in the command strings and
  in this file). The standalone `dossier-consistency` gate had passed because system `python3.12`
  lacks `jsonschema` and `validate_evidence` silently skips schema validation without it.
  The pipeline was then re-run on `main` from a detached worktree outside the checkout, where neither
  environment effect applies, at `b548372` (the merge plus the summary correction): all eleven gates PASS —
  Rust 102 suites / 589 passed / 0 failed, Python 444 passed / 82 skipped, TypeScript and the macOS
  bridge green, `tests` 281 passed (`ci-main-external-worktree.log`).

## Status bookkeeping

The regression protocol names one owning task; the store is CORE-006's (`paths`:
`crates/server/runtime/protocol_state/`). Following the repository's practice for earlier regression
fixes (RUN-006, CORE-008), the transient `FAIL` was not committed — a committed `FAIL` on CORE-006 or
RUN-011 makes every downstream dependency "not ready" in `validate_v81.py`, and `main` must stay green
— and both tasks return to `PASS` with this fix's merge commit after their full test sets re-ran.
RUN-011 (whose test exposed the defect and whose `d6bad3a` was the incomplete fix) carries the same
note.
