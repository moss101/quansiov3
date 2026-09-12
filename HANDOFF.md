# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-12 (M0 complete; M1 started: CORE-001, CORE-002 PASS)
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

Milestone: M1 — canonical state, events and persistence
Current task: CORE-003 (RuntimeEvent store and outbox) and CORE-004 (graph stores) delegated in parallel
Current task status: 10 of 99 tasks `PASS`; the baseline pipeline is green on `main`
Current owner: `agent:principal-1`
Current component: `persistence` (`migrations/`, `crates/server/src/control/schema/`)
Current language: Rust + SQL

## What was completed

- CORE-001 — authoritative persistence schema and migrations — `PASS`. `migrations/0001_canonical_schema.sql`
  (61 public + 2 derived tables for DOMAIN.md §2–§13), RLS enabled and forced on every tenant table
  (no-context queries return zero rows and inserts are refused), `derived` schema for rebuildable
  pgvector structures, `quansio_app` role, `updated_at` triggers and the sqlx migration runner in
  `crates/server/src/control/schema/`. Proof: `crates/server/tests/schema_bootstrap.rs` (5 tests) plus
  the full pipeline.
- CORE-002 — identity, generation and idempotency primitives — `PASS`. `crates/core`: typed
  `<prefix>_<ULID>` ids (46-entry prefix table cross-checked against the generated `EntityPrefix`
  enum), monotonic ULID generator with injectable clock/entropy, generations, revisions, lease fence
  tokens, resumable cursors, effect idempotency keys and duplicate-command classification.
  Proof: `crates/core/tests/identity.rs` (19 tests) and `crates/server/tests/command_idempotency.rs`
  (duplicate command replay/conflict, stale generation updates zero rows).
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

TASK: CORE-003 — RuntimeEvent store and transactional outbox (`crates/events/`), delegated to a
subagent in an isolated worktree. Acceptance: no committed state transition lacks its event; a
publisher restart cannot duplicate an externally visible event; transaction rollback and cursor resume
are proven.
TASK: CORE-004 — WorkGraph/AgentGraph/StateGraph stores and GraphTransaction (`crates/graph/`),
delegated in parallel. Acceptance: revision-checked atomic mutations, acyclic `depends_on`/`parent_of`
enforcement, and narrowing-only child delegation.

## Exact next action

Integrate the two subagent branches (review → merge → re-run their tests on `main` → set progress
`PASS` with evidence and the merge commit), then take CORE-005 (GraphTransaction) which depends on
both. `QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio` and
`scripts/dev/up` are required for the database-backed suites.

## Ready queue

1. `CORE-003` — RuntimeEvent store and transactional outbox (in flight, delegated).
2. `CORE-004` — graph stores and GraphTransaction (in flight, delegated).
3. `CORE-005` — GraphTransaction (depends on CORE-003 + CORE-004).
4. `CORE-006` — durable protocol state and checkpoints.
5. `CORE-007` — artifact and evidence storage.
6. `INT-001` — Python intelligence service and typed RPC boundary.
7. `OPS-007` — supply-chain and dependency security (in flight, delegated).

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
