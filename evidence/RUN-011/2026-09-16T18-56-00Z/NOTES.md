# RUN-011 defect fix — protocol_states upsert conflict target

Known, previously-recorded-but-undiagnosed defect (`HANDOFF.md` "Known defects":
*"`turn_loop.rs` protocol-state write races under parallel load ... not yet
diagnosed past the symptom"*). Independently reproduced during this session's
investigation via `bash scripts/dev/bootstrap.sh` under concurrent host load
(Docker, other cargo processes): `independent_tool_calls_run_together_and_settle_in_proposal_order`
panicked with

```
turn: InvalidArgument("effect protocol state: protocol state store: error returned
from database: duplicate key value violates unique constraint \"protocol_states_pkey\"")
```

## Root cause

`ProtocolState::store` (`crates/server/src/runtime/protocol_state/mod.rs`) computes
its row id deterministically:

```rust
let row_id = format!("pst_{}", state.run_id.trim_start_matches("run_"));
```

This is by design — one protocol-state row per run — but the upsert targeted the
wrong constraint:

```sql
INSERT INTO protocol_states (id, ...) VALUES ($1, ...)
ON CONFLICT (run_id) DO UPDATE SET ...
```

`id` (the primary key) and `run_id` (a separate `UNIQUE` column) always collide
together for this table, but Postgres's `ON CONFLICT` clause only arbitrates the
*exact* constraint/index it names. Two concurrent first-ever writes for the same
run — e.g. two independent tool calls settling within one turn, dispatched via
RUN-011's cooperative single-task `parallel::join_all` (`crates/server/src/runtime/turn_loop/parallel.rs`),
which interleaves multiple `dispatch()` futures on one task across their own
`.await` points rather than running them on separate OS threads — both compute the
identical `id`. Whichever transaction's `INSERT` lands second hits the raw,
uncaught `protocol_states_pkey` violation: the `ON CONFLICT (run_id)` clause never
even sees a `run_id` conflict fire, because Postgres detects and raises the `id`
conflict as a hard error before that constraint is checked.

This is why the failure was load-sensitive and looked host/timing-dependent
rather than a code defect: it requires two writers for the *same* run's protocol
state to race across an `.await` boundary, which only happens reliably under
enough concurrent load to perturb the cooperative scheduler's interleaving.

## Fix

Target the primary key directly:

```sql
ON CONFLICT (id) DO UPDATE SET ...
```

Since `id` is a pure function of `run_id`, this is both correct (it is the
constraint that actually collides) and complete — Postgres's standard
insert-or-update-under-concurrency guarantee applies directly to a conflict
target on the row's own primary key, with no separate locking needed.

## Scope check — same anti-pattern elsewhere?

Every other `ON CONFLICT` site in the Rust workspace was checked
(`crates/events/src/{cursor,store,projection}.rs`, `crates/graph/src/store.rs`,
`crates/server/src/artifacts/metadata.rs`, `crates/server/src/runtime/agents/store.rs`,
`crates/server/src/api/commands.rs`, `crates/server/src/effects/ledger.rs`).
Each targets a genuine composite business key with no separate deterministic
surrogate PK, or — where a separate `id` column exists — generates it with a
fresh `new_ulid()` (`crates/server/src/runtime/agents/store.rs`), not a
deterministic derivation from the conflict-target column. `protocol_states` was
the only table combining a deterministic PK with a non-PK conflict target; the
fix is scoped to it alone, no other table needs the same change.

## Verification

- `independent_tool_calls_run_together_and_settle_in_proposal_order`: 30x serial
  runs + 32x under genuine concurrent OS-process load (4 rounds × 8 parallel
  processes hitting the same dev Postgres simultaneously) — 0 failures.
- `cargo test -p quansio-server --test turn_loop` — 12/12
  (`turn-loop-suite.log`).
- `cargo test --workspace` — the exact load condition that originally surfaced
  the race (per `HANDOFF.md`) and that independently reproduced it during this
  session's investigation — every suite green, 0 failures across 102 reported
  `test result: ok` blocks (`workspace-suite.log`).
- `cargo fmt --all --check` / `cargo clippy -p quansio-server --all-targets -- -D
  warnings` — clean.
- `python3 scripts/validate_v81.py` — PASS, 43 PASS tasks unchanged (this is a
  fix to already-PASS RUN-011's implementation, not a new task; no status
  transition needed).

## Files changed

`crates/server/src/runtime/protocol_state/mod.rs` only — one line (`ON CONFLICT`
target) plus an explanatory doc comment. No schema migration needed: `id` was
already the primary key.
