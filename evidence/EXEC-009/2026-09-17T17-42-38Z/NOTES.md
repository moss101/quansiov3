# EXEC-009 — capture Chrome's own stdout+stderr for diagnosis

## Why

The prior commit's `--no-sandbox --disable-dev-shm-usage` fix worked: the real
GitHub Actions run confirmed Chrome no longer crashes moments after the
debugging port opens (3 of 4 tests now pass; the previously-crashing test gets
much further). But it now hits a *different*, later failure:

```
panicked at crates/qworkerd/tests/browser.rs:269:6:
observe: Cdp(Timeout { method: "Page.captureScreenshot" })
```

`CALL_TIMEOUT` (`crates/qworkerd/src/browser/cdp.rs`) is a real production
constant — every CDP call the managed browser stack makes uses it, not only
this test. Changing its value without knowing *why* the call is slow (or
hanging) would be a guess against production behavior, not a fix. `Browser::launch`
was discarding Chrome's own stdout+stderr (`Stdio::null()`), so there was no
way to tell "genuinely slower hardware, give it more time" from "Chrome is
logging a specific error explaining why this never completes."

## Change (diagnostic only, test-only)

Chrome's combined stdout+stderr now goes to a real temp file — not a pipe,
since nobody reads a pipe continuously over a long-running session and Chrome
would eventually block writing to a full OS pipe buffer — instead of
`/dev/null`. `Browser`'s `Drop` prints the file's content via `eprintln!` and
removes it, unconditionally rather than gated on the test's own outcome:
cargo test's own harness already captures every test's output and only
*displays* it for a test that fails, so a passing run stays exactly as quiet
as before and a failing one reveals what Chrome itself said. No per-test code
changed, and no new CI plumbing — the existing full-gate-log capture (this
session's earlier `evidence/GOV-005/2026-09-16T20-55-27Z/`) already uploads
whatever `cargo test` prints to its own stdout/stderr.

No production code touched — `crates/qworkerd/src/browser/cdp.rs` and
`CALL_TIMEOUT` are unchanged.

## Verification

- `cargo test -p quansio-qworkerd --test browser` — 4/4, and `grep -c "chrome
  stdout" browser-suite-quiet.log` is **0** — confirms a normal passing run is
  exactly as quiet as before this change (`browser-suite-quiet.log`).
- Same suite with `--nocapture` — 4/4, and the same grep is **4** (one banner
  per test) — confirms the file redirection and Drop-time read are wired
  correctly and would surface real content on a failure
  (`browser-suite-nocapture-proof.log`; contains Chrome's actual "DevTools
  listening on ws://..." startup banner).
- `cargo fmt --all --check` / `cargo clippy -p quansio-qworkerd --all-targets
  -- -D warnings` — clean.

## Next step

Not a fix by itself — the next real GitHub Actions run is what this commit is
for: reading what Chrome actually says during the 20s `Page.captureScreenshot`
window that CI hits and this Mac does not, before deciding whether
`CALL_TIMEOUT` genuinely needs raising, a specific flag is missing, or
something else is going on.

## Files changed

`crates/qworkerd/tests/browser.rs` only — `Browser`'s struct fields,
`launch()`'s stdio wiring, and `Drop`.
