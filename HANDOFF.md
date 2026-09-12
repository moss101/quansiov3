# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-12 (M0 complete, M1 complete; M3 started; 20 of 99 tasks complete)
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
Current task: RUN-001 — authoritative Rust runtime state machine (M2), delegated in an isolated worktree
Current task status: 20 of 99 tasks `PASS`; M1 closed, M3 started; the baseline pipeline is green on `main`
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
- CORE-003 — RuntimeEvent store and transactional outbox — `PASS` (`crates/events`): envelope and the 34
  DOMAIN §9.2 event families, state mutation + event + outbox in one transaction, per-tenant sequence
  counter assigned inside that transaction (race-free, gap-free), outbox publishing behind an
  `EventTransport` trait with an explicit `UnavailableTransport` that never silently drops events, and
  durable consumer cursors with resume. Evidence: `evidence/CORE-003/<ts>/`.
- CORE-004 — graph stores — `PASS` (`crates/graph`): WorkGraph/AgentGraph/StateGraph with cycle rejection,
  transition tables, revision compare-and-set, `done` only through verification, append-only attempts and
  one atomic batch under a single graph revision (`migrations/0003_graph_heads.sql`). The delegation
  narrowing check is a trait for RUN-005. Evidence: `evidence/CORE-004/<ts>/`.
- CORE-006 — durable protocol state and checkpoints — `PASS` (`crates/server/src/runtime/`):
  protocol state per DOMAIN §5.7, a pure `next_safe_action` (cancel > reconcile unknown effect >
  approval > question > takeover > child > waits > continue) so an unknown effect outcome is reconciled
  rather than retried, checkpoint metadata with generation/retention fencing, and caller-owned
  connections so several stores compose into one transaction. Evidence: `evidence/CORE-006/<ts>/`.
- OPS-007 — supply-chain and dependency security — `PASS` (`scripts/ci/supply_chain/`): lockfile pinning,
  `deny.toml` completeness, deterministic CycloneDX SBOM with drift verification, a real
  RUSTSEC-2019-0014 vulnerable-lockfile fixture, and skill quarantine-lifecycle checks; `cargo-deny` is an
  explicit informational result when absent. Evidence: `evidence/OPS-007/<ts>/`.
- CORE-008 — scheduler, waits and durable timers — `PASS` (`crates/server/src/scheduler/`, migration
  `0004_durable_timers.sql`): timers and waits persisted in PostgreSQL with exactly-once firing proven
  under two competing scheduler loops, survival across a pool drop, generation fencing, a typed
  saturated-queue error, run dispatch through the canonical Run state machine, and routine due times with
  `skip`/`queue`/`catch_up_once`. Evidence: `evidence/CORE-008/<ts>/`.
- CORE-009 — projections and resumable streaming — `PASS` (`crates/events/src/{projection,stream}.rs`,
  migration `0005_projections.sql`): deterministic rebuildable read models with per-projection checkpoints,
  idempotent re-application, and DOMAIN §9.3 streaming with cursors, transient live frames, channel/tenant
  scoping and bounded backpressure. Evidence: `evidence/CORE-009/<ts>/`.
- INT-004 — derived search index — `PASS` (`crates/indexer/`): typed `SearchProgram` filters over exact,
  lexical (Tantivy) and symbol channels with provenance, budgets and reported truncation, tenant
  isolation, incremental maintenance and a PostgreSQL rebuild that reproduces identical hits. Evidence:
  `evidence/INT-004/<ts>/`.
- INT-001 — Python intelligence service and typed RPC boundary — `PASS`
  (`python/intelligence/server/`): `IntelligenceGateway` over loopback TCP or a unix socket, one
  scope/deadline gate (tenant, workspace, correlation id, capability projection id) that aborts before any
  handler runs, a real deterministic non-LLM `ClassifyTrust`, typed unimplemented failures naming the
  owning task for every later RPC, and a reproducible generated-binding boundary test. The delegating
  subagent died on a full disk after committing, so the principal agent re-ran the full verification on
  `main` before `PASS`. Evidence: `evidence/INT-001/<ts>/`.
- CORE-005 — GraphTransaction — `PASS` (`crates/graph/src/transaction/`): one transaction applies the graph
  batch under a single `graph_heads` compare-and-set and stages every RuntimeEvent (tenant sequence +
  outbox row) before one commit, so a rejected change rolls back state and events together; `PlanProposal`
  application enforces `base_revision`, bounded size, acyclicity and the RUN-005 narrowing hook; change
  kinds that DOMAIN §9.2 does not yet name are rejected before the transaction opens. Evidence:
  `evidence/CORE-005/<ts>/`.
- CORE-007 — artifact and evidence storage — `PASS` (`crates/server/src/artifacts/`): metadata authority
  plus a SigV4 S3 client writing tenant-prefixed, content-addressed keys to the dev MinIO with multipart
  upload and SSE-S3; evidence is insert-only with new identities per capture; grants fail closed;
  retention and legal hold are recorded decisions, never silent deletes. Evidence: `evidence/CORE-007/<ts>/`.
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

TASK: INT-004 — Rust indexer and `SearchIndex` API (`crates/indexer/`): exact/lexical/symbol channels over
artifact text, typed SearchProgram filters, rebuildability, tenant isolation and budgets.
TASK: RUN-001 — the authoritative Rust runtime state machine (`crates/server/src/runtime/`): Run/Turn/
Step/Attempt transitions driven by protocol state, the agent turn loop entry points, recovery, and the
canonical dispatch path. This is the heart of M2.

## Exact next action

Integrate each delegated branch as it completes (review → merge → re-run its tests on `main` → set
progress `PASS` with evidence naming the merge commit). Then take INT-002 (server-side model
gateway; a real-boundary task whose live conformance suite needs `QUANSIO_TEST_ANTHROPIC_API_KEY` or
`QUANSIO_TEST_OPENAI_API_KEY` — without them the live part is recorded as `BLOCKED_EXTERNAL`).
Database-backed suites need `scripts/dev/up` plus
`QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`; the baseline
pipeline derives that URL from the generated `.env` automatically.

## Ready queue

1. `RUN-001` — authoritative Rust runtime state machine (in flight, delegated).
2. `INT-002` — server-side model gateway (M3; real boundary).

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

Services: the repository's own dev stack starts with `scripts/dev/up` (idempotent: it now re-reads
`config/dev.yaml` for images/ports/bucket while preserving credentials and the MinIO SSE key in the
gitignored `.env`) (Docker Compose project
`quansio-dev`): Postgres 17 + pgvector on 55440, NATS JetStream on 54230/54231, MinIO on 59010/59011,
stub model provider on 59020, optional Qdrant on 59030. Dev-only credentials live in the gitignored
`.env` generated by `scripts/dev/up`; the documented dev defaults are `quansio` / `quansio-dev-only`.
`scripts/dev/_common.sh` puts Docker Desktop's credential helper on `PATH` before any pull.
Credentials/handles: no production `QUANSIO_TEST_*` credentials are set; real-boundary tasks must record
`BLOCKED_EXTERNAL` until provided. Database-backed tests read `QUANSIO_TEST_POSTGRES_URL`, for example
`postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`. Never place raw secrets in this file.
Ports: dev stack as above; product ports are fixed by later tasks.
Disk caveat: the volume hosting this work is nearly full. Keep at most three concurrent worktrees, and
delete a finished worktree's `target/` directory (`rm -rf <worktree>/target`) after its task is closed —
a full disk aborted one subagent mid-run.
Concurrency caveat: several agents may run in parallel worktrees against this ONE shared dev stack
(compose project `quansio-dev`). Never run `scripts/dev/down`, never delete its volumes, and never
recreate its containers from a worktree-modified compose file — a divergent config silently changed the
running MinIO (breaking SSE) once already.
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
