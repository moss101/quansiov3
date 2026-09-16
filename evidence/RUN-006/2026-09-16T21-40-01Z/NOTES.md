# RUN-006 fix — approval receipt signature vs. Postgres timestamp precision

## Discovery

Only diagnosable because two prior fixes landed first: OPS-007's postgres
`services:` fix let CI reach the `toolchains` gate at all
(`evidence/GOV-005/2026-09-16T20-22-27Z/`), and GOV-005's full per-gate log
capture preserved enough of a very verbose gate's output to actually reach the
failures — the summary's own 25-line tail had cut off before them
(`evidence/GOV-005/2026-09-16T20-55-27Z/`). With both, the real GitHub Actions
run's `toolchains.log` showed three failures in the same file, all
approval/receipt-signature related:

```
a_run_parks_in_waiting_approval_and_resumes_only_on_its_matching_receipt:
  panicked at crates/server/tests/policy.rs:1202: consume after resume: ApprovalInvalid(SignatureInvalid)
approval_substitution_fails_closed_and_writes_nothing:
  panicked at :627: assertion failed: matches!(error, PolicyError::ApprovalInvalid(ApprovalFailure::ParamsChanged { .. }))
exactly_one_approval_event_per_transition_and_denial_changes_no_effect_state:
  panicked at :1106: consume: ApprovalInvalid(SignatureInvalid)
```

## Root cause

`ApprovalReceipt::signing_message()` (`crates/server/src/policy/approval.rs`)
includes `self.granted_at.to_rfc3339()`. chrono's `DateTime` carries
nanosecond precision. `PolicyStore::grant_approval`
(`crates/server/src/policy/store.rs`) signs a receipt whose `granted_at` is
the caller's raw `now`, then persists that *same* value into a `TIMESTAMPTZ`
column — which PostgreSQL stores at **microsecond** precision, silently
dropping anything finer. The next time that row is read (every later
`verify`), `granted_at` has different low-order digits than the value that
was signed, `signing_message()` produces a different string, and the HMAC no
longer matches — `SignatureInvalid` on a receipt that was, in fact, correctly
granted.

`approval_substitution_fails_closed_and_writes_nothing`'s failure is the same
bug wearing a different assertion: its first checked path also calls `verify`
internally, so `SignatureInvalid` fires before the test ever reaches the
`ParamsChanged` case it's actually checking. Confirmed by the fix resolving
all three failures identically (see verification below) — this was never
three separate bugs.

Whether this manifests at all depends only on whether the host clock's actual
resolution goes finer than a microsecond on a given call to `Utc::now()` —
which is why it passed on every local run this entire session (macOS) and
failed reliably on GitHub Actions' Linux runner. The codebase already knows
this class of bug: `crates/events/src/envelope.rs` explicitly truncates a
timestamp with `to_rfc3339_opts(SecondsFormat::Millis, true)` for exactly this
reason. That pattern never reached `policy/approval.rs`'s signing path.

## Fix

Truncate `now` to microsecond precision
(`chrono::SubsecRound::trunc_subsecs(6)`) at the top of `grant_approval`,
before it is signed or persisted — the value that gets signed is now
byte-identical to what Postgres will hand back on every later read, on any
platform, regardless of clock resolution.

## Regression test

Added `a_receipt_granted_with_sub_microsecond_precision_still_verifies_after_reload`,
which grants with an *explicit, forced* nanosecond-precision timestamp
(`"2026-09-16T20:32:01.123456789Z"`, parsed rather than sourced from
`Utc::now()`), so it reproduces deterministically on **every** host — not only
ones whose clock happens to have finer-than-microsecond jitter, which this
Mac's does not.

## Verification

- **Red/green, directly against the fix** (`red-green-proof.log`): reverted
  `store.rs` with `git stash`, rebuilt — the new test failed with the exact
  same `SignatureInvalid` the real CI run hit; restored the fix — it passes.
  The full file (all 12 tests, including the two others CI's fuller run had
  also failed) passes with the fix in place, confirming one root cause, not
  three.
- `cargo test -p quansio-server --test policy` — 12/12 (`policy-suite.log`).
- `cargo test --workspace` — every suite green, 102 `test result: ok` blocks,
  0 failures (`workspace-suite-tail.log`) — the fix touches shared
  `grant_approval`/`grant_and_resume`, used by `turn_loop.rs` and others.
- `cargo fmt --all --check` / `cargo clippy -p quansio-server --all-targets
  -- -D warnings` — clean.
- A standalone chrono-semantics check (`red-green-proof.log`, final section)
  confirms the mechanism independent of the product's own code: a
  nanosecond-precision instant's `to_rfc3339()` differs from the same instant
  truncated to microseconds; truncating first makes them match.
- Checked for the same pattern elsewhere: only two HMAC usages exist in the
  codebase (`policy/approval.rs`, fixed here, and `artifacts/sigv4.rs`, which
  signs outbound AWS requests at send-time — never persisted then reloaded for
  re-verification, so it is not subject to this bug class).

Host note: hit this repository's own previously-documented "host stops
executing freshly-linked binaries" incident (see `HANDOFF.md`) partway through
this verification — applied its documented workaround (write the new
binary's bytes into a pre-existing, already-executed inode) to get the first
red/green result, and confirmed the incident had cleared shortly after by
running fresh binaries directly, which is how the rest of this evidence was
produced.

## Files changed

`crates/server/src/policy/store.rs` (one import, four lines in
`grant_approval`), `crates/server/tests/policy.rs` (one new regression test).
