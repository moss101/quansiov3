# EXEC-009 fix — Chrome crashing under GitHub Actions' restricted sandbox

## Discovery

Only possible because of two things landing first: OPS-007's postgres
`services:` fix (`evidence/GOV-005/2026-09-16T20-22-27Z/`) let CI reach the
`toolchains` gate at all — every prior run had crashed before any test ran —
and GOV-005's full per-gate log capture
(`evidence/GOV-005/2026-09-16T20-55-27Z/`) preserved more than the summary's
own 25-line tail, which would have cut off before reaching the actual failure.
With both, the real GitHub Actions run's `toolchains.log` showed:

```
test the_dom_is_read_first_and_a_screenshot_only_when_it_says_nothing ... FAILED
thread '...' (19848) panicked at crates/qworkerd/tests/browser.rs:130:10:
open a page: Unreachable("127.0.0.1:46843: Connection refused (os error 111)")
```

## Root cause

Read carefully rather than treated as "browser missing": `Browser::launch()`
polls a raw TCP connect to the remote-debugging port every 200ms for up to
25s and only returns `Some` once one succeeds — and it *did* succeed (the test
reached `open_page()`'s real CDP HTTP request at all, which only runs after
`launch()` returns). The very next call, moments later, got "connection
refused." Chrome accepted a connection and then stopped listening — the
signature of the browser process crashing shortly after start, not of it
never starting or the port never opening.

This is the single most common headless-Chrome-in-CI failure: Chrome's own
sandbox needs kernel privileges (unprivileged user namespaces, or a correctly
-installed setuid `chrome-sandbox` helper) that CI runners routinely restrict,
and Chrome also uses `/dev/shm` for shared memory, sized far smaller on CI
images than on a developer machine — either crashes the browser almost
immediately. Puppeteer, Playwright and Chrome's own CI documentation all
converge on the same fix: `--no-sandbox --disable-dev-shm-usage`.

## Fix

Added both flags to `Browser::launch()`'s Chrome invocation, gated on the `CI`
environment variable (which GitHub Actions, and effectively every CI system,
sets automatically) rather than unconditionally. Real production browser
sessions run inside a microVM (DOSSIER.md §8/§12), never this test's own
process, so nothing about the managed browser stack itself is weakened; a
developer running this suite locally still exercises Chrome's real sandbox,
matching this test file's own stated premise ("nothing here stands in for
Chrome").

## Verification

- `cargo test -p quansio-qworkerd --test browser` — 4/4 (`browser-suite.log`).
- Same, with `CI=true` set (exercising the new code path on this host, where
  it's a no-op since macOS Chrome doesn't hit this failure mode either way) —
  4/4 (`browser-suite-ci-flag.log`).
- `cargo fmt --all --check` / `cargo clippy -p quansio-qworkerd --all-targets
  -- -D warnings` — clean.
- Not independently reproducible in this sandbox by nature — this host
  already runs Chrome successfully without the flags, so there is nothing
  here to fail either way. The next real GitHub Actions run is the
  authoritative confirmation, exactly like the fixes before it this session.

## Files changed

`crates/qworkerd/tests/browser.rs` only — `Browser::launch()`'s Chrome
argument list.
