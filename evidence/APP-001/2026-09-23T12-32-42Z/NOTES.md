# APP-001 regression: a refused command replayed as 200 success

**What exposed it.** Found by reading `main` at 7d54410 on 2026-09-23. `api/commands.rs::invoke`
inserted the `commands` record as `'accepted'` and then called the owner with `?` on every arm.
Only on success did it write `status = 'applied', result = …`. Nothing wrote `'rejected'` or the
`error` column. After an owner refusal the record stayed `'accepted'` with `result` and `error`
both NULL. A resend with the same `command_id` and params hit `ON CONFLICT … DO NOTHING`, and
`replay()` matched the digest and answered `200 {replayed: true, result: null}`. The client
saw a refused command as succeeded, and so did a concurrent duplicate that arrived while the
first request was still in flight. DOMAIN.md §1.2 says a duplicate returns the original result,
and §14 says commands return `result | error`.

**Reproduced by** two new tests in `crates/server/tests/api.rs`, both red against the unfixed
handler (`cargo-test-api-before-fix.log`):

- `a_refused_command_is_recorded_and_a_resend_gets_the_same_refusal`. The owner fails the run,
  then `CancelRun` is refused by the runtime with `409 RUNTIME_ILLEGAL_TRANSITION`. Before the
  fix the record stayed `accepted`.
- `a_resend_while_the_first_is_still_being_applied_is_told_to_come_back`. A resend against an
  `accepted` record got `200 {"replayed":true,"result":null}`.

A stale generation cannot be the refusal here: run generations never advance past 1 in this
codebase (`Generation::accept` rejects only `received < current`), and generation 0 is refused
earlier by the handler's own `Generation::new` with `400 VALIDATION_BOUNDS`. The test covers
that validation refusal as well: it is recorded, and its resend gets the identical 400.

**Fix** (`fix/APP-001-record-refused-commands`, 19f49e3). The per-command match moved unchanged
into `dispatch()`. `invoke` records any refusal from it as `status = 'rejected', error = <§15
ApiError JSON>` and returns that refusal. `replay()` now reads `status`:

- `applied` returns the stored result, as before;
- `rejected` returns the stored refusal verbatim, with its original HTTP status and
  correlation id. `ApiError` gained `Deserialize` to read it back;
- `accepted` (the first submission has not recorded an outcome yet) returns `409
  CONFLICT_STATE`, `details: {command_status: "accepted"}`, `retryable: true`. That value is
  set on this one instance: a later resend returns the outcome, so the same call can succeed
  later. The §15 table has no UNAVAILABLE code, and no code was invented.

**Retryable refusals are recorded, not freed.** Owners are not idempotent by `command_id`. They
tag events with it but do not deduplicate on it, and some commit part of their work before they
can still fail: `PostMessage` stores the message and creates a run before its turn, and
`runtime_error` maps an unknown runtime failure there to `INTERNAL`. Freeing the id would let
an automatic resend apply that part twice. So a retry after a retryable refusal is a new
command. `RATE_LIMITED` is refused before the record is written, so its `command_id` stays free;
`a_tenant_over_the_rate_limit_is_rate_limited` now asserts no record exists. If recording the
refusal itself fails, the client still gets the owner's refusal, the failure goes to stderr,
and a resend gets the `accepted` answer. No migration: `status IN ('accepted', 'applied',
'rejected')` and `error JSONB` were already in 0001. Every new statement is against `commands`
only, and the ownership scan passes.

**Known gap, not fixed here.** A handler future cancelled by a client disconnect between the
insert and the outcome write leaves a record `accepted` for good, and every resend gets the
retryable `CONFLICT_STATE`. This predates the fix (before it, those resends got `200 null`).
It is left as a follow-up.

**Verification.** The new tests were red before the fix and green after. `cargo test -p
quansio-server --no-fail-fast` passed every binary. fmt and clippy (`-D warnings`) were clean,
and `validate_v81.py` passed. `QUANSIO_TEST_POSTGRES_URL` pointed at a private scratch database
(`quansio_fix_app001_rejected`) on the canonical 55440 stack, because the shared `quansio`
database can carry other sessions' unmerged migrations. `bash scripts/ci/ci.sh` was not run: it
cannot pass from a nested worktree (contract-gen), and a fresh detached-worktree build does not
fit in the free disk.
