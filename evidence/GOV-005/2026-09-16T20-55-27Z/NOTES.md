# GOV-005 — full per-gate CI log capture

`scripts/ci/ci_summary.py`'s summary artifact only ever kept the last 25 lines
of a gate's combined stdout+stderr (the `tail` field). For a quiet gate that's
plenty; for `toolchains` (`bash scripts/dev/bootstrap.sh` — `cargo test
--workspace`, then pnpm, then Swift) it wasn't close: the previous commit's
real GitHub Actions run failed with `error: test failed, to rerun pass -p
quansio-server --test policy` as the very last line, with no assertion, no
panic message, not even which of the 11 tests in that file failed — `gh run
view --log` on the same run had exactly the same 24 lines, confirming the
summary's tail was already the ceiling on what was recoverable after the fact,
not a `gh` CLI limitation.

## Fix

`run_gate()` now writes every gate's complete stdout+stderr to
`artifacts/ci/logs/<commit>/<gate>.log` in addition to the existing truncated
`tail`, and the workflow uploads that directory as a second artifact,
`ci-gate-logs`, alongside the existing summary — for every run, not only
failing ones, so whatever fails next is diagnosable from the artifact alone.

## Verification

- `tests/ci/` — 55/55; nothing asserts the exact key set of a gate result, so
  adding `log` is additive.
- A full local `bash scripts/ci/ci.sh` — `pipeline: PASS`, 11/11
  (`local-ci-run.log`), and confirmed every gate actually wrote its own log
  file under `artifacts/ci/logs/<commit>/` (the `toolchains` one: 1351 lines,
  72K — the real `bootstrap.sh` output in full, not a slice).
- The very next real GitHub Actions run (the commit this evidence is filed
  under) proved the feature immediately: downloading its `ci-gate-logs`
  artifact surfaced the actual failing test and its exact panic message —
  `crates/qworkerd/tests/browser.rs:130`, `Connection refused (os error
  111)` — which the old 25-line tail alone would never have shown. See
  `evidence/EXEC-009/` for that finding and its fix.
