# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-12 (M0 complete: GOV-001…GOV-008 PASS)
Repository: `quansiov3` (local)
Branch: `main`
HEAD: see `git rev-parse HEAD` on `main`
Authority version: V8.1

## Mission

Complete Quansio V8.1 to production readiness from the repository's own V8.1 authority set
(`AGENTS.md`, `DOSSIER.md`, `DOMAIN.md`, `registries/tasks.json`, `registries/progress.json`,
`scripts/validate_v81.py`). Persistent autonomous execution is active. Do not stop unless manually
stopped; when one task is blocked, record the blocker and take the next dependency-ready task.

## Current position

Milestone: M0 complete (GOV-001…GOV-008 all `PASS`); starting M1
Current task: CORE-001 — authoritative persistence schema and migrations
Current task status: 8 of 99 tasks `PASS`; the baseline pipeline is green on `main`
Current owner: `agent:principal-1`
Current component: `persistence` (`migrations/`, `crates/server/src/control/schema/`)
Current language: Rust + SQL

## What was completed

- GOV-001 — repository inventory and V8.1 reconciliation — `PASS`.
  Greenfield rule applied: **no legacy authority found**; every canonical owner `GENUINE_GAP`.
  Deliverables: `docs/review/2026-09-12-gov-001-reconciliation.md`, `scripts/ci/inventory.py`
  (inventory + duplicate-authority scan), `tests/architecture/test_inventory.py`,
  `evidence/GOV-001/2026-09-12T00-14-04Z/`.
- GOV-002 — install V8.1 as the sole active implementation authority — `PASS`.
  Deliverables: `README.md` (authority pointers, build/test commands), `docs/archive/README.md`
  (non-authority banner), `scripts/ci/check_authority.py`, `tests/architecture/test_authority_check.py`,
  `evidence/GOV-002/2026-09-12T00-15-40Z/`.
- GOV-003 — canonical monorepo and language boundaries — `PASS`.
  Deliverables: `Cargo.toml` workspace with 12 members (`crates/{core,contracts,events,graph,
  capability,tools,indexer,machine,qworkerd,server,cli}` + `native/windows`), `python/pyproject.toml`
  with `uv.lock` (Python 3.12), `pnpm-workspace.yaml` with `apps/desktop`, `apps/web`, `sdk/typescript`,
  `native/macos` SwiftPM bridge, `config/models.yaml` + `config/flags.yaml`, `scripts/ci/workspace_check.py`,
  `scripts/dev/bootstrap.sh`, `evidence/GOV-003/2026-09-12T00-24-44Z/`.
  Proof: `bash scripts/dev/bootstrap.sh` green — authority gate, architecture gates, `cargo fmt/clippy/test`,
  ruff + mypy strict + pytest (15 tests), pnpm build/typecheck/test (8 tests) + eslint, `swift test` (4 tests),
  and 53 repository architecture tests via `uv run --project python pytest tests -q`.

## What is currently being implemented

TASK: CORE-001 — Implement authoritative persistence schema and migrations.
Goal: one authoritative PostgreSQL data model for DOMAIN.md §2–§13, forward migrations with
forward-fix rollback, RLS enabled and forced on every tenant table, and a separate `derived` schema
for rebuildable pgvector structures.
Files: `migrations/0001_canonical_schema.sql`, `crates/server/src/control/schema/mod.rs`,
`crates/server/tests/schema_bootstrap.rs`.

## Exact next action

Finish CORE-001 on `task/CORE-001-schema`: apply the migration through the shipped sqlx runner against
the running dev stack (`QUANSIO_TEST_POSTGRES_URL=postgres://quansio:…@127.0.0.1:55440/quansio`), prove
bootstrap-from-zero, tenant isolation (zero rows without context), forward-fix re-run and trigger/RLS
presence, then collect evidence, set progress `PASS` and merge.

## Ready queue

1. `CORE-001` — authoritative persistence schema (M1, depends on GOV-004+GOV-007); unblocks all of M1–M7.
2. `OPS-007` — supply-chain and dependency security (M7, depends on GOV-003+GOV-005).

## Blocked work

None. No `QUANSIO_TEST_*` credentials exist, so real-boundary tasks will be `BLOCKED_EXTERNAL` when
reached (27 tasks declare `real_boundary: true`).

## Architecture decisions made during implementation

- Reconciliation, authority pointers and workspace mapping are executable gates
  (`scripts/ci/inventory.py`, `check_authority.py`, `workspace_check.py`), not prose. Rationale:
  governance that cannot fail a build is not governance. GOV-008 consolidates them behind
  `scripts/ci/arch_check.py`.
- Root-level architecture tests run in the intelligence-plane environment
  (`uv run --project python pytest tests -q`) because `config/*.yaml` validation needs PyYAML, which is
  now a declared dependency of `python/intelligence` (INT-003 will consume the catalog).
- Repo-level tooling, caches, build output and Swift `/.build` are gitignored; `Cargo.lock` and
  `pnpm-lock.yaml` and `python/uv.lock` are committed for deterministic builds (DOSSIER.md §18).

## Migrations/state changes

- `git init` on `main`; authority set committed as `[GOV-001] initialize repository` (`23a6014`).
- `registries/progress.json`: GOV-001, GOV-002, GOV-003 `PASS` (merge commits recorded).
- No database migrations exist yet (CORE-001 owns `migrations/`).

## Tests

Last successful: `bash scripts/ci/ci.sh` — all ten baseline gates PASS (authority, dossier consistency,
architecture, authority pointers, workspace, legacy map, contract drift, contract lint/compat,
toolchains, repository tests) at `9604c1a`; `uv run --project python pytest tests -q` → 104 passed.
Last failed: none.
Tests still required: GOV-004 schema lint + regeneration diff + compatibility fixtures; GOV-005 CI
negative tests; GOV-008 forbidden-wiring fixtures; then per-task tests from M1 onward.

## Runtime/recovery state

Relevant checkpoints: none (no runtime yet).
Effect reconciliation concerns: none.
Unsettled effects: none.
Generation/lease concerns: none.

## Known defects

None recorded.

## Working tree

Modified: none on `main` at GOV-003 merge.
Untracked: none.
Generated (never hand-edit): `TASKS.md`, `registries/task-graph.json`, `MANIFEST.json` —
regenerate with `python3 scripts/validate_v81.py --write`. Contract bindings are generated by GOV-004
tooling; regenerate, never hand-edit.
Do not overwrite: the V8.1 authority set (`MANIFEST.json` lists digests).

## Commands

Build: `bash scripts/dev/bootstrap.sh` (or `cargo check --workspace`, `pnpm build`, `(cd python && uv sync --frozen)`)
Test: `uv run --project python pytest tests -q` · `(cd python && uv run --frozen pytest -q)` · `pnpm test` · `cargo test --workspace`
Validate: `python3 scripts/validate_v81.py` (regenerate views with `--write`)
Run locally: pending GOV-007 (`scripts/dev/up`)
Qualification: `scripts/ci/inventory.py --scan`, `check_authority.py --check`, `workspace_check.py`

## Environment requirements

Services: the repository's own dev stack starts with `scripts/dev/up` (Docker Compose project
`quansio-dev`): Postgres 17 + pgvector on 55440, NATS JetStream on 54230/54231, MinIO on 59010/59011,
stub model provider on 59020, optional Qdrant on 59030. Dev-only credentials live in the gitignored
`.env` generated by `scripts/dev/up`; the documented dev defaults are `quansio` / `quansio-dev-only`.
`scripts/dev/_common.sh` puts Docker Desktop's credential helper on `PATH` before any pull.
Credentials/handles: no production `QUANSIO_TEST_*` credentials are set; real-boundary tasks must record
`BLOCKED_EXTERNAL` until provided. Database-backed tests read `QUANSIO_TEST_POSTGRES_URL`, for example
`postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`. Never place raw secrets in this file.
Ports: dev stack as above; product ports are fixed by later tasks.
External dependencies: Rust 1.97.1 (+ rustfmt/clippy), Node 26 + pnpm 11.8, Python 3.12 + uv 0.12,
protoc 36, Swift 6.3, Docker 29. All present.

## Resume instructions

1. Read `AGENTS.md`.
2. Read `DOSSIER.md` (and the `DOMAIN.md` sections named by the selected task; §0 glossary always).
3. Read this `HANDOFF.md`.
4. `git status` / `git log --oneline -5`; confirm `main` is green.
5. Run `python3 scripts/validate_v81.py`.
6. Run `python3 scripts/validate_v81.py --ready` and take the next dependency-ready task.
7. Continue without asking for another kickoff. Do not stop unless manually stopped.
