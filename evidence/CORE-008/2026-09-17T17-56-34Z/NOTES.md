# CORE-008 fix — routine test hits the same precision bug as RUN-006

## Discovery

Surfaced by the diagnostic commit right before this one (Chrome log capture,
`evidence/EXEC-009/2026-09-17T17-42-38Z/`, added to diagnose browser.rs's
screenshot timeout). That run's browser.rs tests passed outright — the
screenshot timeout was transient, not a deterministic bug — but a different
test failed:

```
thread 'absence_policy_applies_skip_queue_and_catch_up_once' panicked at
crates/server/tests/scheduler.rs:554:5: assertion failed:
fired.iter().all(|request| request.fire_window == missed)
```

## Root cause

Same mechanism as `RUN-006` (`evidence/RUN-006/2026-09-16T21-40-01Z/`), this
time in test code rather than production. The test computes `missed =
Utc::now() - Duration::minutes(3)` at full chrono (nanosecond) precision,
seeds three routines with it — `seed_routine` binds it straight into
`next_due_at TIMESTAMPTZ`, which Postgres stores at microsecond precision,
silently truncated on write — then calls `tick()`.

Read `crates/server/src/scheduler/routine.rs::tick()` carefully before
touching anything: its `window` is `routine.next_due_at`, the value it *read
back from the database*. Production code is correct — it faithfully returns
exactly what was stored, nothing recomputed. The test's own assertion then
compares that DB-round-tripped `fire_window` by exact `DateTime` equality
against the *original*, un-truncated `missed` still held in a local variable.
The two can only match when `missed`'s nanosecond digits already happened to
be zero — which is why this passed on every local run this session (macOS)
and failed on GitHub Actions' Linux runner, exactly like `RUN-006`.

## Fix

Truncate `missed` to microsecond precision
(`chrono::SubsecRound::trunc_subsecs(6)`) once, immediately after computing
it, before it is used for seeding *or* the assertion — both uses now agree
with what Postgres will actually store, on any host.

## Scope check

Swept the workspace for the same pattern (`grep` for `== missed`, `==
granted_at`, `== created_at`, `== expires_at`, `== due`, `== fired_at`, `==
window` across every `src`/`tests` file). This was the only remaining
occurrence — `RUN-006` was the only other exact-`DateTime`-equality-after-a-
database-round-trip site, already fixed.

## Verification

- **Red/green, directly against the fix** (`red-green-proof.log`): reverted
  `scheduler.rs` with `git stash`, ran the test — it passes on this host even
  *without* the fix (this Mac's clock is already microsecond-aligned, the
  same reason `RUN-006`'s bug never reproduced locally either — the real
  GitHub Actions run is the primary proof this fix addresses a real failure,
  not this local check). Restored the fix; still passes.
- `cargo test -p quansio-server --test scheduler` — 10/10
  (`scheduler-suite.log`).
- `cargo test --workspace` — every suite green, 102 `test result: ok` blocks,
  0 failures (`workspace-suite-tail.log`).
- `cargo fmt --all --check` / `cargo clippy -p quansio-server --all-targets
  -- -D warnings` — clean.
- Test-only change; no production code touched.

## Files changed

`crates/server/tests/scheduler.rs` only — one import, two lines.
