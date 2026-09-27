# QA-001 reconciliation (2026-09-27, second claim)

- **Canonical owner:** `scripts/ci/` (the pipeline and its gates) and `tests/contract/` (the static
  suite bans). Tests of `scripts/ci` tools live in `tests/ci/`, where GOV-005 put them.
- **Existing code — `PARTIAL`:** the first claim (2026-09-15, evidence `2026-09-15T11-05-00Z`) added
  `tests/contract/test_qa001_gate.py`: a *source scan* for `pytest.mark.skip/xfail` and Rust
  `#[ignore]` in `crates/*/tests`. Contract drift is already gated (`gen_contracts.py --check`,
  `contract_compat.py`), and the schema suite validates real artifacts. Gaps:
  - nothing checks what the runs actually skipped. pytest runs `-q`, so 16 + 4 skips a run carry
    no reason anywhere; vitest `.skip`/`.todo` and inline `#[ignore]` in `src/` are unscanned;
    imperative `pytest.skip()` is unscanned;
  - DOSSIER §21.4 coverage is not measured at all (no tool installed).
- **Persistent state / contracts / events / migrations:** none.
- **External effects:** none. Tooling: the `llvm-tools` rustup component (download approved by
  the user 2026-09-27). coverage.py and `@vitest/coverage-v8` were not approved: Python is
  measured with a standard-library `sys.monitoring` tracer; TypeScript coverage is
  `BLOCKED_EXTERNAL`.
- **Real boundaries:** none required (`real_boundary: false`). Database-backed suites need the dev
  stack (`QUANSIO_TEST_POSTGRES_URL`), as in the baseline pipeline.
- **Rollback:** remove the gates; nothing persistent.

## Plan

1. `scripts/ci/test_audit.py`: parse the pipeline's own test logs; fail on skips that are not a
   named `BLOCKED_EXTERNAL`, on xfail/xpass, cargo `ignored`, vitest `skipped`/`todo`; publish the
   inventory of what did not run.
2. `ci_summary.py`: run pytest with `-rsxX` (`PYTEST_ADDOPTS`), expose the log directory
   (`QUANSIO_CI_LOG_DIR`), and add the `test-audit` gate last.
3. Static bans widened: `.skip/.only/.todo` in TS tests, `#[ignore]` anywhere in `crates/`,
   imperative `pytest.skip(` without a named variable.
4. `scripts/ci/coverage.py` + `pycoverage.py` + `coverage-expectations.json`: measure Rust
   (instrumented workspace tests, branch regions) and Python against §21.4; `check` fails under a
   floor. A `coverage` job in the workflow; `llvm-tools` in `rust-toolchain.toml`.
5. Tests for each tool on fixtures, with the failure paths.
6. Evidence: a clean checkout (`git clone` of the branch) running `ci.sh`, and the coverage run.
