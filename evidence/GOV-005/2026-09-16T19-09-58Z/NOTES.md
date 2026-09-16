# GOV-005 fix — CI pipeline hermeticity + dead import

Found while independently verifying the OPS-007 (`evidence/OPS-007/2026-09-16T18-43-39Z/`)
and RUN-011 (`evidence/RUN-011/2026-09-16T18-56-00Z/`) fixes through the actual
baseline pipeline (`bash scripts/ci/ci.sh`), not through targeted commands alone.
Both issues below are pre-existing drift on `main`, unrelated to those two
fixes — nothing had run the full pipeline end-to-end in a while.

## Finding 1 — supply-chain gate not hermetic

`scripts/ci/ci_summary.py`'s `GATES` list invoked
`python3.12 scripts/ci/supply_chain/check.py` — bare system Python, not the
pinned `python/` venv. That check's quarantine scan (`skills.py`) needs PyYAML
to parse the YAML manifests it audits. On a host whose ambient `python3.12` has
no PyYAML installed globally, the identical commit reports the gate `FAIL`
("cannot be parsed without PyYAML; quarantine cannot be verified") purely
because of that host's site-packages — reproduced here even on a clean,
pre-OPS-007-fix commit (`3e664c6`), confirming it predates and is unrelated to
today's other fixes. Fixed by routing through `uv run --project python python`,
matching the three other gates that already need the venv (`tests`,
`contract-drift`, `contract-lint-compat`). The six purely-standard-library gates
(`authority`, `dossier-consistency`, `architecture`, `authority-pointers`,
`workspace`, `legacy-map`) are left on bare `python3.12` — they need nothing the
venv adds and stay usable without a `uv sync`.

## Finding 2 — dead import

`python/tests/adapters/test_connector_adapters.py` imported `run_contract_tier`
without calling it. `run_suite()` — the function every test in the file actually
calls — already invokes `run_contract_tier` internally per adapter, so the
import was leftover, not a missing assertion. `ruff` (run as part of
`bash scripts/dev/bootstrap.sh`, the `toolchains` gate) correctly flagged it;
removed per its own suggested fix.

## Verification

- `uv run ruff check .` — clean across the entire `python/` tree (this import
  was the only finding).
- `test_connector_adapters.py` — 6/6, unchanged coverage (both tiers still
  exercised via `run_suite`).
- `uv run --project python python scripts/ci/supply_chain/check.py` — CLEAN.
- `bash scripts/ci/ci.sh` on the merge commit — 10/11 gates PASS (`authority`,
  `dossier-consistency`, `architecture`, `authority-pointers`, `workspace`,
  `supply-chain`, `legacy-map`, `contract-drift`, `contract-lint-compat`,
  `tests`). `toolchains` still fails, but now purely on a separate, larger,
  pre-existing issue: 14 ESLint problems across 4 TypeScript files (an unused
  variable, 10 unnecessary-boolean-comparison/conditional errors in
  `apps/desktop/src/main/security.ts`, a template-literal type error, and two
  files ESLint's type-aware project service cannot find in any `tsconfig.json`:
  `tests/e2e/journeys.spec.ts`, `tests/e2e/vitest.config.ts`). Not addressed in
  this commit — flagged to the user for scope confirmation rather than fixed
  silently, since it spans APP-003, APP-008, OPS-003 and QA-008 territory and
  is unrelated to the two defects this session was asked to fix.

## Files changed

`scripts/ci/ci_summary.py` (one gate command), `python/tests/adapters/test_connector_adapters.py`
(one import line removed).
