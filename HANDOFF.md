# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-12 (GOV-002, GOV-003 closed)
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

Milestone: M0 — Authority, repository and build foundation
Current task: GOV-004 in progress (contract generation); GOV-007 delegated in an isolated worktree
Current task status: GOV-001 `PASS`, GOV-002 `PASS`, GOV-003 `PASS`; 3 of 99 tasks complete
Current owner: `agent:principal-1`
Current component: `contracts` (GOV-004), `dev-environment` (GOV-007)
Current language: Rust/Python/TypeScript

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

TASK: GOV-004 — Establish canonical contract generation.
Goal: contract sources in `schemas/` (Protobuf, OpenAPI 3.1, JSON Schema) derived from DOMAIN.md, with
generated Rust/Python/TypeScript bindings and a regeneration-diff CI gate.
Delegated in parallel (isolated worktree): GOV-007 — deterministic local development stack
(`infra/compose/`, `scripts/dev/up|down|reset|seed`, health check).

## Exact next action

Finish GOV-004 on `task/GOV-004-contracts`: add `schemas/domain/` (ids, events, errors, commands),
generators, generated bindings under `crates/contracts`, `python/intelligence/contracts`, `sdk/typescript`,
plus the DOMAIN.md drift check, then evidence → PASS → merge. GOV-006 (`Define legacy migration and
deletion plan`) is a greenfield trivial close referencing GOV-001; GOV-008 depends on GOV-003+GOV-004.

## Ready queue

1. `GOV-004` — contracts (depends on GOV-003 `PASS`); unblocks GOV-005, GOV-008 and every RPC/event task.
2. `GOV-006` — legacy migration map (greenfield: trivially satisfied, evidence references GOV-001).
3. `GOV-007` — deterministic local dev stack (depends on GOV-003+GOV-004); being implemented by a
   delegated agent in an isolated worktree.
4. `GOV-008` — architecture conformance rules (depends on GOV-003+GOV-004).

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

Last successful: `bash scripts/dev/bootstrap.sh` (all gates) and `uv run --project python pytest tests -q`
(53 passed) at GOV-003's merge.
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

Services: Docker daemon available; local Postgres 17 (port 54329), Redis 8 (54330) and MinIO
(54331/54332) containers are running for later integration/qualification work. GOV-007 will define the
reproducible compose stack and the standard dev credentials.
Credentials/handles: no `QUANSIO_TEST_*` credentials are set; real-boundary tasks must record
`BLOCKED_EXTERNAL` until provided. Never place raw secrets in this file.
Ports: fixed by GOV-007.
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
