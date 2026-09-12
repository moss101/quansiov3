# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-12 (GOV-001 closed)
Repository: `quansiov3` (local)
Branch: `main`
HEAD: see `git rev-parse HEAD` on `main` after the GOV-001 merge
Authority version: V8.1

## Mission

Complete Quansio V8.1 to production readiness from the repository's own V8.1 authority set
(`AGENTS.md`, `DOSSIER.md`, `DOMAIN.md`, `registries/tasks.json`, `registries/progress.json`,
`scripts/validate_v81.py`). Persistent autonomous execution is active. Do not stop unless manually
stopped; when blocked on one task, record the blocker and take the next dependency-ready task.

## Current position

Milestone: M0 — Authority, repository and build foundation
Current task: GOV-001 closed; GOV-002 and GOV-003 are next
Current task status: GOV-001 `PASS`
Current owner: `agent:principal-1`
Current component: `repository` → `governance` (GOV-002), `monorepo` (GOV-003)
Current language: Mixed (M0 is Markdown/Python/Rust/TypeScript scaffolding)

## What was completed

- GOV-001 — repository inventory and V8.1 reconciliation — `PASS`.
  - Greenfield rule applied: **no legacy authority found**; every canonical owner is `GENUINE_GAP`.
  - Deliverables: `docs/review/2026-09-12-gov-001-reconciliation.md`,
    `scripts/ci/inventory.py` (inventory + duplicate-authority scan, exit 1 on findings),
    `tests/architecture/test_inventory.py`, `tests/ci/*` for the dev tooling,
    `evidence/GOV-001/2026-09-12T00-14-04Z/summary.json`.
  - Tests: `python3 scripts/ci/inventory.py --scan` (CLEAN), `uv run --python 3.12 --with pytest pytest tests -q`
    (23 passed), `python3 scripts/validate_v81.py` (PASS).

## What is currently being implemented

Nothing in flight. GOV-001 is merged; the working tree is clean.

## Exact next action

Claim **GOV-002** (`task/GOV-002-<slug>`): install V8.1 as the sole active implementation authority —
create `README.md` pointing at the V8.1 authority set, create `docs/archive/` with a README stating it
is non-authority, and commit evidence that no superseded authority is referenced as active
(greenfield: trivially satisfied with a reference to `docs/review/2026-09-12-gov-001-reconciliation.md`).

GOV-003 (`task/GOV-003-<slug>`) may be taken in parallel by a second agent: create the canonical
monorepo layout (Cargo workspace, `python/pyproject.toml` with `uv`, `pnpm-workspace.yaml`,
`native/`), pin toolchains (`rust-toolchain.toml`, `.python-version`, `.nvmrc`) and prove
`cargo check`, Python import/type check and TypeScript build pass from a clean checkout.

## Ready queue

1. `GOV-002` — depends only on GOV-001 (PASS); governance/docs work.
2. `GOV-003` — depends only on GOV-001 (PASS); creates the monorepo skeleton that unblocks
   GOV-004/006/007/008 and all of M1.

## Blocked work

None. No task has been claimed and blocked.

## Architecture decisions made during implementation

- Reconciliation and architectural conformance are executable, not prose-only:
  `scripts/ci/inventory.py` owns the owner-mapping table (`CANONICAL_OWNERS`) and the rule engine
  (`no-canonical-owner`, `non-rust-authority-write`, `provider-sdk-outside-gateway`,
  `client-direct-database`, `tool-registry-outside-rust`, `hardcoded-model-id`, `new-go-code`).
  GOV-008 extends the same engine into CI; rationale: governance that cannot fail a build is not
  governance. Affected paths: `scripts/ci/inventory.py`, `tests/architecture/`.
- Evidence bundles are produced by a reusable collector (`scripts/dev/evidence.py`) and progress is
  edited by a validating tool (`scripts/dev/progress.py`) so every task's evidence and status follow
  DOSSIER.md §19/§20 exactly. Affected paths: `scripts/dev/`, `evidence/`, `registries/progress.json`.

## Migrations/state changes

- `git init` on `main`; authority set committed as `[GOV-001] initialize repository` (`23a6014`).
- `registries/progress.json`: GOV-001 `NOT_STARTED` → `RECONCILING` → `PASS`.
- No database migrations exist yet.

## Tests

Last successful: `uv run --python 3.12 --with pytest pytest tests -q` (23 passed) at
`1a1b5eb`; `python3 scripts/ci/inventory.py --scan` → CLEAN; `python3 scripts/validate_v81.py` → PASS.
Last failed: none.
Failure reason: n/a.
Tests still required: per-task tests for GOV-002…GOV-008, then M1 onward.

## Runtime/recovery state

Relevant checkpoints: none (no runtime yet).
Effect reconciliation concerns: none.
Unsettled effects: none.
Generation/lease concerns: none.

## Known defects

None recorded.

## Working tree

Modified: none at GOV-001 merge.
Untracked: none.
Generated: `TASKS.md`, `registries/task-graph.json`, `MANIFEST.json` — regenerate only with
`python3 scripts/validate_v81.py --write`.
Do not overwrite: the V8.1 authority set (see `MANIFEST.json`).

## Commands

Build: not yet defined (GOV-003 adds `cargo check`, `uv sync`, `pnpm build`).
Test: `uv run --python 3.12 --with pytest pytest tests -q`
Validate: `python3 scripts/validate_v81.py`
Run locally: not yet defined.
Qualification: `python3 scripts/ci/inventory.py --scan` (architecture/duplicate-authority gate).

## Environment requirements

Services: Docker daemon available; a local Postgres 17 / Redis 8 / MinIO stack is running for later
integration and qualification work (GOV-007 will define the reproducible compose stack).
Credentials/handles: no `QUANSIO_TEST_*` production credentials are set; real-boundary tasks must
record `BLOCKED_EXTERNAL` until they are provided. Never place raw secrets in this file.
Ports: not yet fixed.
External dependencies: Rust 1.97, Node 26 + pnpm 11, Python 3.12 + uv, protoc 36 — all present.

## Resume instructions

1. Read `AGENTS.md`.
2. Read `DOSSIER.md` (and the `DOMAIN.md` sections named by the selected task).
3. Read this `HANDOFF.md`.
4. `git status` / `git log --oneline -5`; confirm `main` is green.
5. Run `python3 scripts/validate_v81.py`.
6. Run `python3 scripts/validate_v81.py --ready` and take the next dependency-ready task.
7. Continue without asking for another kickoff. Do not stop unless manually stopped.
