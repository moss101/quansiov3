# CORE-006 regression — concurrent writes to one run's ProtocolState

Branch `fix/CORE-006-protocol-state-concurrent-writes`.

## Failing test and exposing commit

`crates/server/tests/turn_loop.rs::a_turn_executes_tool_proposals_through_capability_policy_and_the_effect_ledger`
panicked once under `bash scripts/dev/bootstrap.sh` (`cargo test --workspace`, every test binary in
parallel against the dev PostgreSQL) at `9eb2794` with

    turn: InvalidArgument("effect protocol state: protocol state store: error returned from database:
    duplicate key value violates unique constraint \"protocol_states_run_id_key\"")

then passed 3/3 alone (`evidence/INT-002/2026-09-18T08-19-07Z/ci-run1-turn-loop-flake.log`). The
defect was introduced by `d6bad3a` (RUN-011, 2026-09-16), which changed the same statement's
`ON CONFLICT (run_id)` to `ON CONFLICT (id)` after the identical failure on `protocol_states_pkey`.

## Root cause (real runtime bug, not test isolation)

`protocol_states` has two unique indexes: `id` (primary key, derived as `pst_<run suffix>`) and
`run_id` (`UNIQUE`). `ProtocolStateStore::store` is one `INSERT ... ON CONFLICT (<one of them>) DO UPDATE`.
PostgreSQL arbitrates `ON CONFLICT DO UPDATE` only on the named index; every other unique index is
checked with an ordinary error-raising uniqueness check. When two transactions perform the run's
first-ever write at the same moment, both pre-check the arbiter index before either row is visible,
both insert, and the loser raises on the index that is *not* the arbiter. Because `id` and `run_id`
always collide together, swapping the target only swaps the constraint in the error message.

The turn loop legitimately issues such concurrent writes: `Engine::dispatch_tool_calls` runs the
independent calls of one round through `parallel::join_all`, and every Effect Ledger transition
(`reserve`, `mark_dispatched`, `settle_*`, ...) is its own transaction ending in
`sync_pending_tool_call` → `ProtocolStateStore::load` → modify → `store`. Two calls in one round
therefore race on the first write. Nothing in the test shares state across binaries: each test owns a
scratch database; parallel load only widens the window.

Reproduced against the real schema (`repro_protocol_state_race.py`, two transactions, 1000 fresh runs each):

| statement | failures |
|---|---|
| `ON CONFLICT (run_id) DO UPDATE` (before d6bad3a) | 84 × `protocol_states_pkey` |
| `ON CONFLICT (id) DO UPDATE` (shipped) | 70 × `protocol_states_run_id_key` |
| claim (`INSERT ... ON CONFLICT DO NOTHING`, no target) + `UPDATE ... WHERE run_id` | 0 |

The same harness shows the quieter half of the race: two unlocked load → modify → store cycles for
one run lose one writer's pending tool call in 1000/1000 overlapping runs; with the claim followed by
`SELECT ... FOR UPDATE` the count is 0. Fixing only the statement would turn the panic into a silently
dropped `pending_tool_calls` entry in exactly the failing test's scenario.

## Canonical owner

- `crates/server/src/runtime/protocol_state/` (CORE-006) owns the store and gets the fix: a race-free
  `store` (claim with every unique index arbitrating, then update) and a `lock` primitive
  (claim, then `SELECT ... FOR UPDATE`) that serializes read-modify-write cycles for one run for the
  rest of the caller's transaction, whether or not a row exists yet.
- Callers that modify protocol state concurrently with an in-flight turn switch to `lock`:
  `crates/server/src/effects/ledger.rs::sync_pending_tool_call` (RUN-007),
  `crates/server/src/policy/store.rs::append_pending_approval` (RUN-006),
  `RunStore::cancel` in `crates/server/src/runtime/state_machine/store.rs` (RUN-001, a user
  cancellation must not be overwritten by a concurrent tool-call transition),
  and the cross-transaction load/store pairs in the engine and the dispatcher
  (`park_*`, `park_for_approval`) through a new `RunStore::update_protocol_state` that runs the whole
  cycle in one transaction under the lock (RUN-001, RUN-011).
- Coverage: GENUINE_GAP already recorded; this is a defect in existing code, no new module.

## Persistent state, contracts, effects, migrations

- Table `protocol_states` unchanged; no migration. Committed row contents are identical to before
  (the claim inserts the empty §5.7 state and the update in the same transaction overwrites it).
- No contract/event change; DOMAIN.md §5.7 fields unchanged.
- No external effects; no capabilities/approvals; no real boundary (PostgreSQL only, via
  `QUANSIO_TEST_POSTGRES_URL`).
- Lock order stays acyclic: every writer takes [its aggregate row: effect_records / runs /
  approval_requests] → `protocol_states` row → tenant event-sequence row (taken by
  `commit_staged` after the mutation closure). No transaction takes an aggregate row after the
  protocol-state row.

## Recovery impact

Improves it: recovery decides from `pending_tool_calls`; the lost-update half of the race could hide a
dispatched call from `next_safe_action` between two transitions.

## Plan

- [x] `ProtocolStateStore::store`: claim (no-target `ON CONFLICT DO NOTHING`) + `UPDATE ... WHERE run_id`,
      fail closed when the update touches no row.
- [x] `ProtocolStateStore::lock(conn, tenant, run_id, generation) -> ProtocolState`.
- [x] `sync_pending_tool_call`, `append_pending_approval`, `RunStore::cancel` use `lock`.
- [x] `RunStore::update_protocol_state`; engine `park_wait`/`park_question`/`park_child`/
      `park_unsettled_effect` and dispatcher `park_for_approval` use it.
- [x] Regression tests in `crates/server/tests/protocol_state.rs`: concurrent first-ever `store`
      (many rounds) never raises and leaves one row; concurrent `lock` → modify → `store` cycles never
      lose an entry. Both fail on the previous code.
- [x] Negative: `store` for an unknown run still fails; `lock` outside the writer's tenant is invisible.
- [x] `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test -p quansio-server --test protocol_state`, `--test turn_loop` (×25),
      `cargo test --workspace` (×2), `python3.12 scripts/validate_v81.py`; `bash scripts/ci/ci.sh`
      (its contract-gen step cannot build inside this nested worktree — see NOTES.md — so the
      remaining toolchain gates were run individually here and the full pipeline is re-run on `main`
      at merge).
- [x] Evidence bundle + progress notes for CORE-006 (and RUN-011, whose test exposed it).
