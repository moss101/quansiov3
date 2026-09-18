# APP-001 — re-verification on main after INT-002 reached PASS

## Why

APP-001 (`real_boundary: false`) was `BLOCKED_EXTERNAL` with `implementation_complete: true`
solely because its dependency INT-002 was not PASS (DOSSIER.md §20). INT-002 is PASS as of
6725718; nothing in `crates/server/` changed. This bundle is the task's recorded test set
re-run on the current `main` (e631fb5) against the dev PostgreSQL.

## Results

- `cargo test -p quansio-server --test api` — 12 passed: §15 vocabulary, ownership scan,
  OpenAPI/catalog contract, feature flags, per-tenant RATE_LIMITED, CancelRun owner path,
  composition (`cargo-test-api.log`)
- `cargo test -p quansio-server --test walking_skeleton` — 2 passed: HTTP PostMessage →
  conformance-stub `fs.read` → Effect Ledger → projections; WebSocket reconnect-from-cursor
  (`cargo-test-walking-skeleton.log`)
- `cargo fmt --all --check` / `cargo clippy -p quansio-server --all-targets -- -D warnings` —
  clean (`fmt-check.log`, `clippy.log`)
- generated clients: `tests/architecture tests/contract` — 139 passed
  (`architecture-contract.log`); the CI `contract-drift` and `contract-lint-compat` gates PASS
- `python3.12 scripts/validate_v81.py` — PASS (`validate.log`)
- `bash scripts/ci/ci.sh` — PASS, including the walking-skeleton smoke test under the
  `toolchains` gate with the conformance-stub provider (`ci.log`)

Acceptance: every mutating endpoint calls its canonical owner (ownership scan), generated
clients pass contract tests, and the walking-skeleton smoke test passes in the baseline
pipeline with the conformance-stub provider.
