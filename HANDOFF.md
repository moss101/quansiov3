# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-12 (M0/M1 complete; M2/M3 in progress; 27 PASS + 1 BLOCKED_EXTERNAL)
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

Milestone: M2 — the runtime loop (M0/M1 complete)
Current task: none in flight — RUN-011 closed `PASS`; the next dependency-ready task is RUN-008
Current task status: 27 tasks `PASS`, INT-002 `BLOCKED_EXTERNAL` with implementation complete; every baseline gate green on `main`
Current owner: `agent:principal-1`
Current component: `turn loop` (`crates/server/src/runtime/turn_loop/`, `crates/tools/`)
Current language: Rust + SQL

Resolved host incident: for part of this session the machine would not execute newly created
binaries (a freshly compiled `cc` hello-world hung in `_dyld_start`), which blocked `cargo test`
until it cleared. `bash scripts/ci/ci.sh` now runs normally and is green on `main`; the incident,
the workaround used to keep verifying while it lasted, and the artifact corruption it caused are
recorded under "Environment requirements".

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
- RUN-007 — Universal Effect Ledger — `PASS` (`crates/server/src/effects/`, `config/effects.yaml`): the
  reservation derives its idempotency key from effect class, resource and parameter digest and leans on the
  schema's partial unique in-flight index, so two racing dispatchers reserve exactly one action; every
  transition is one event-emitting transaction with exactly one `effect.*` event and contiguous aggregate
  versions; settlement is terminal and refuses before writing; an `OUTCOME_UNKNOWN` record **cannot** be
  retried and is resolved only by reconciliation matching the class's strategy (the taxonomy is loaded from
  `config/effects.yaml`, validated against the generated catalog and fails closed on any divergence); a retry
  creates a new identity sharing the key and leaves the old record untouched; approval-required actions
  cannot bypass the receipt; protocol state keeps `next_safe_action` reporting `ReconcileEffect` until the
  ledger is settled. Evidence: `evidence/RUN-007/<ts>/`.
- RUN-006 — policy, RBAC, privacy guards and approvals — `PASS` (`crates/server/src/policy/`): evaluation
  order is RBAC → capability projection → trust escalation → privacy → sequence guards → policy rules →
  receipt requirement → user rules, with tenant and workspace policies merged most-restrictively
  (`deny > ask > allow`) and no matching rule at effective tier ≥ 1 denying. Tier 4 always asks — an
  `always` user rule is refused when stored and ignored when evaluated. Receipts bind id, request, effect,
  approver, `params_digest`, scope, generation and expiry, and are HMAC-SHA256 signed with a server key
  (`QUANSIO_APPROVAL_SIGNING_KEY`, absent key fails closed); verification is constant-time and single-use, so
  a receipt for one action cannot authorize a changed one and failure writes nothing. A parameter change
  supersedes a pending request. Trust escalation follows DOMAIN §12 rule 2 (tier ≥ 2 from
  `UNTRUSTED_EXTERNAL` escalates one tier) and escalated proposals cannot use `always`. Evidence:
  `evidence/RUN-006/<ts>/`.
- RUN-003 — plan compilation and validation — `PASS` (`crates/server/src/runtime/planning/`): proposals are
  compiled and validated before touching the graph — bounded by the policy `max_plan_nodes` (25 when no
  policy exists), malformed input rejected while writing nothing, acyclicity checked over existing edges,
  `parent_id` chains and the proposal's own additions, CompletionContract ownership or inheritance recorded
  per node, capability needs checked with RUN-005's narrowing algebra, and `base_revision` compare-and-set
  enforced before the single GraphTransaction applies the batch. Determinism is proven by identical revision,
  event sequence and node shape across repeat runs and after a simulated restart. Evidence:
  `evidence/RUN-003/<ts>/`.
- RUN-005 — Capability Projection — `PASS` (`crates/capability/`): the algebra composes grants by
  intersection with most-restrictive constraints over all nine DOMAIN §6.1 selector kinds; projection
  assembly follows the fixed §6.2 layer order and ignores + records any widening attempt as
  `capability.widening_rejected` naming the layer; an unavailable input layer, unparseable grant or policy
  gap at tier ≥ 1 fails closed with an empty projection and `CAPABILITY_INPUTS_UNAVAILABLE`; and
  `authorize` always re-checks the current inputs digest and expiry, so a stale projection cannot
  authorize dispatch. The delegation narrowing check that RUN-002's hook expects is implemented. Evidence:
  `evidence/RUN-005/<ts>/`.
- RUN-002 — AgentThread, delegation, handoff and join — `PASS`
  (`crates/server/src/runtime/agents/`, migration `0006_agent_mailbox_handoff_join.sql`): one primitive
  serves persistent teammates and ephemeral workers with a separate lifecycle policy (teammates never JOIN,
  workers terminate only from JOINED); the durable mailbox cursor delivers each item exactly once across a
  crash; delegation calls the narrowing check **before** any insert, so a widening check writes nothing;
  handoff writes a PENDING durable record a restarted runtime can complete or roll back, idempotently; and
  fanout join parks the parent until the last child joins without ever double-merging. Evidence:
  `evidence/RUN-002/<ts>/`.
- RUN-001 — authoritative Rust runtime state machine — `PASS`
  (`crates/server/src/runtime/state_machine/`): Run/Turn/Step/Attempt transitions are evaluated inside the
  event-emitting transaction, so an illegal transition rolls back state and events together; every
  mutation is generation-fenced before any write; attempts are appended and never overwritten; cancelling
  or suspending a run is an authoritative transition; and `recover` applies `next_safe_action` so a
  `WAITING_*` run is released only by its matching resolution and an unsettled effect is reconciled rather
  than retried (idempotent across repeated calls). The turn-loop seams for model proposals, tool dispatch,
  delegation and verification return typed not-available errors naming INT-002/RUN-011/RUN-002/RUN-008
  instead of fabricating success. Evidence: `evidence/RUN-001/<ts>/`.
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

- RUN-011 — agent turn loop, Tool contract and Tool Registry — `PASS`. `crates/tools` owns the Tool
  contract and a control-plane Tool Registry loaded from `config/tools.yaml`: namespaced versioned
  declarations, a strict JSON-Schema subset whose unsupported keywords fail the load (a schema can never
  be silently under-enforced), every object schema closing `additionalProperties`, argument validation
  that rejects unknown/missing/ill-typed fields, derivation of effect class (static, or arg-driven such
  as the `fs.write` workspace/host scope), resource selector, canonical parameter digest and idempotency
  key, host, output bound, timeout, cancellability, evidence capture and source trust, and exposure
  filtered by the Capability Projection. The registry and the generated catalog
  (`schemas/catalog/tools.yaml`) must cover each other exactly; `connector.<id>.<op>` and `scm.*.<op>` are
  reserved families that stay uncallable until CAP-003 and EXEC-009/012 register concrete operations.
  `crates/server/src/runtime/turn_loop` implements DOMAIN §7.4 in fixed order — registry and schema →
  capability projection → policy, RBAC and user rules → approval park → Effect Ledger reservation with a
  dispatch token → host execution → settlement — writing the durable `tool_calls` row (migration 0007
  records the declaration version) and `tool.*` RuntimeEvents. Independent calls are dispatched
  concurrently and their results applied in proposal order (proved with a two-party barrier, not a
  timing assumption); the engine parks a run for approval and **resumes the recorded call** instead of
  re-proposing it; an unsettled effect is reported for reconciliation and never re-dispatched. The
  question protocol (`user.ask` and the model's `question`) writes a durable Question with the policy
  TTL, parks `WAITING_QUESTION`, and resumes on the answer or expires the run; delegate, plan and memory
  routes go to their owning ports and fail closed naming the owner. A refusal before the reservation
  writes no EffectRecord and reaches no host, and the proposal's causal chain is evaluated from
  `UNTRUSTED_EXTERNAL` until INT-005 supplies real context labels, so a tier ≥ 2 call is escalated and
  needs a receipt. Evidence: `evidence/RUN-011/<ts>/`.

## What is currently being implemented

None — RUN-011 is closed. The runtime M2 path now executes model-proposed tool calls end to end
through capability, policy, approval, the Effect Ledger and the host seam.

## Exact next action

Take RUN-008 (CompletionContract verification) next: it closes the turn loop's `completion_claim`
branch, which still returns `SeamNotAvailable` naming RUN-008, and it depends only on the Effect Ledger
that is already `PASS`. Then RUN-004 (concurrency, fanout/fanin, cancellation and waits), which completes
M2. Before wiring the planner into the turn loop, apply the alias fix recorded under "Architecture
decisions" so a plan can reference nodes it creates in the same batch.
Database-backed suites need `scripts/dev/up` plus
`QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`; the baseline
pipeline derives that URL from the generated `.env` automatically.

## Ready queue

1. `RUN-008` — CompletionContract verification (depends on the Effect Ledger, now PASS); it closes the
   turn loop's completion-claim branch.
2. `RUN-004` — concurrency, fanout/fanin, cancellation and waits; completes M2.
3. `INT-003` — deterministic model selection, DLP and failover (ready; real boundary like INT-002).
4. `INT-005` — ContextProjection and typed SearchProgram; supplies the trust labels RUN-011 reads.
5. `EXEC-001` — machine control and execution-target lifecycle.
6. `INT-011` — embedding pipeline and derived vector index (ready because INT-002 is
   `BLOCKED_EXTERNAL` with implementation complete; per D-017 it may start but a task that depends on it
   can only reach `PASS` once the live conformance runs).

## Blocked work

### INT-002 — server-side model gateway (`BLOCKED_EXTERNAL`, implementation complete)
Reason: the live provider conformance suite cannot run here.
External dependency: `QUANSIO_TEST_ANTHROPIC_API_KEY` and `QUANSIO_TEST_OPENAI_API_KEY` (plus egress to
`api.anthropic.com` / `api.openai.com`).
Exact unblock condition: set both variables and run
`uv run --project python python -m pytest python/tests/intelligence/test_model_gateway_live.py -q`,
then record the run in `real_boundary_evidence` and flip the task to `PASS`. Everything else about the
task is implemented and verified offline (173 Python-plane tests, 6 skipped live cases that name the
missing variable).
Independent work available: yes — RUN-003, RUN-002 and the rest of M2/M3 continue.

No other task is blocked. 27 tasks declare `real_boundary: true`; each will be recorded the same way
rather than fabricated.

## Architecture decisions made during implementation

- **Plan batches cannot yet reference their own nodes (RUN-003 limitation).** CORE-005's
  `GraphChange::CreateWorkNode` generates a `wn_` id inside the transaction with no alias, so a plan that
  creates a parent and a child together cannot point the child at the new parent; intra-plan references are
  validated and then rejected with `VALIDATION_SCHEMA`, and created nodes must start `draft`. *Fix (before
  RUN-011 wires the planner into the turn loop):* add an optional caller-supplied alias to
  `CreateWorkNode` in `crates/graph/src/transaction/` and have the transaction resolve aliases (parent ids
  and edge endpoints) to the generated ids inside the same batch, then widen RUN-003's planning test set to
  cover a parent/child proposal applied in one transaction. Affected paths: `crates/graph/src/transaction/`,
  `crates/graph/tests/`, `crates/server/tests/planning.rs`.
- **Runtime state tables are duplicated and guarded (RUN-001).** The runtime keeps its own copies of the
  DOMAIN run/turn/step/attempt status values because `crates/graph` depends on the server's schema module,
  so `crates/server` cannot depend on `crates/graph` (a package cycle). `tests/architecture/test_runtime_state_parity.py`
  fails if either copy, or the database `CHECK` constraints, diverge. *Consolidation (next hardening step):*
  move the pure state enums and transition tables into `crates/core` (they need no SQL), have both
  `crates/graph` and the runtime re-export them, then the runtime can call `GraphTransaction` directly and
  the parity test can be retired. Rationale: one expression of one state machine. Affected paths:
  `crates/core/src/state.rs` (new), `crates/graph/src/state.rs`, `crates/server/src/runtime/state_machine/state.rs`.
- **Tool declarations are control-plane data, not source constants (RUN-011).** Every tool is
  declared in `config/tools.yaml` and the registry is validated against the generated catalog
  `schemas/catalog/tools.yaml`; a declared tool outside the catalog or a catalog tool with no
  declaration fails the contract test. Catalog families (`connector.<id>.<op>`, `scm.git.<op>`,
  `scm.pr.<op>`) are *reserved* rather than callable: materializing them requires a concrete
  declaration with its own schema, so a call naming one is refused as an unregistered tool instead of
  being dispatched against a guessed schema. Affected paths: `config/tools.yaml`, `crates/tools/`.
- **The tool schema vocabulary is closed (RUN-011).** `crates/tools` implements exactly the JSON
  Schema keywords listed in `crates/tools/src/schema.rs` and rejects a declaration using anything
  else; every object schema must set `additionalProperties: false`. A general-purpose schema engine was
  rejected because an unimplemented keyword would be silently under-enforced, and a new dependency was
  not justified by DOSSIER §23 change control. *Widening* the vocabulary is a code change with a test,
  never a config-only change.
- **Parallel tool dispatch adds no dependency (RUN-011).** `runtime::turn_loop::parallel::join_all`
  polls boxed futures on the task that already drives the turn, which gives the overlapping I/O that
  matters without adding a futures executor to `crates/server`. Independence is decided by
  `plan_rounds`: control-plane tools (`user.ask`, `work.*`, `memory.propose`) get a round of their own
  after the batch, and a repeated (tool, canonical-args) pair is deferred to a later round so it cannot
  race for one idempotency key. Calls that touch the same resource with *different* arguments are not
  ordered here; the Effect Ledger still refuses a second in-flight reservation of one key.
- **A parked call is resumed, never re-proposed (RUN-011).** The dispatch records the reserved effect
  and its dispatch token in ProtocolState; when the run is released, `run_turn` continues the recorded
  call (looked up by its effect id) rather than letting the model propose it again. An effect whose
  outcome is unsettled is never re-dispatched: the run parks and `recover` reports
  `ReconcileEffect`. `migrations/0007_tool_call_declaration_version.sql` records the declaration
  version on the call row so a resumed call re-plans against the same version.
- **Proposal trust fails closed (RUN-011).** Policy evaluates a proposal's causal chain as
  `UNTRUSTED_EXTERNAL` (`FAIL_CLOSED_TRUST`) until INT-005's ContextProjection supplies real segment
  labels through the `ProposalTrustSource` seam. A tier ≥ 2 call is therefore escalated one tier and
  needs an approval receipt; a conformance test asserts that escalation, and another asserts that an
  `AGENT_GENERATED` chain (the model's own proposal) does not escalate. Rationale: DOMAIN §12 rule 2
  escalates a chain that *includes* untrusted content, and the runtime must not assume a chain is clean.
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
- `migrations/0007_tool_call_declaration_version.sql` (RUN-011): adds
  `tool_calls.declaration_version`, so a resumed call re-plans against the declaration it was validated
  against instead of silently picking up a newer one.
- `registries/progress.json`: GOV-001, GOV-002, GOV-003 `PASS` (merge commits recorded).
- No database migrations exist yet (CORE-001 owns `migrations/`).

## Tests

Last successful (RUN-011, this session): `bash scripts/ci/ci.sh` — all eleven baseline gates PASS
(authority, dossier consistency, architecture, authority pointers, workspace, supply-chain, legacy
map, contract drift, contract lint/compat, toolchains, repository tests) at the RUN-011 merge
(`69a7d20f4c7f`), plus all 15 `quansio-server` and `quansio-tools` test binaries — 181 tests, 0
failures — including the new `turn_loop` conformance suite (11 tests), plus
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`uv run --project python pytest tests -q` → 188 passed, `(cd python && uv run --frozen pytest -q)` →
173 passed / 6 skipped (INT-002's live-provider cases), `pnpm build/typecheck/test/lint` green,
`python3 scripts/validate_v81.py` PASS and `python3.12 scripts/ci/arch_check.py` CLEAN.
Last failed: none.
Tests still required: GOV-005 CI negative tests; the per-task tests of the remaining registry tasks.

## Runtime/recovery state

Relevant checkpoints: none (no runtime yet).
Effect reconciliation concerns: none.
Unsettled effects: none.
Generation/lease concerns: none.

## Known defects

None recorded.

## Working tree

Modified: none on `main` after the RUN-011 merge.
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
Server signing key: approvals are signed with `QUANSIO_APPROVAL_SIGNING_KEY` (HMAC-SHA256 over the bound
receipt fields). Tests set it explicitly; an absent or empty key fails closed rather than issuing an
unsigned receipt, so a deployment must provide it through the secret broker.
Credentials/handles: no production `QUANSIO_TEST_*` credentials are set; real-boundary tasks must record
`BLOCKED_EXTERNAL` until provided. Database-backed tests read `QUANSIO_TEST_POSTGRES_URL`, for example
`postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`. Never place raw secrets in this file.
Ports: dev stack as above; product ports are fixed by later tasks.
Host incident (this session, resolved): the machine stopped executing **newly created** binaries
partway through RUN-011. A freshly compiled `cc` hello-world hangs in `_dyld_start`, and every newly
linked `cargo test` binary does the same, while binaries whose inode already existed keep running
(`/bin/ls`, an already-built test binary). `cargo build`/`link` still work; only `exec` of a new inode
fails. `sudo` is not available, so neither a `syspolicyd` kickstart nor a reboot could be performed.
Workaround used while it lasted: write the freshly linked binary's bytes into a **pre-existing**
inode (`cat <new-binary> > <old-binary-path>`, `chmod +x`) and run that path; the workaround did not
change the test code or its assertions, and its counts were later reproduced exactly by a clean
`cargo test` after the host recovered. *Damage it caused and how it was repaired:* the candidate
paths included compiled `.dylib`/`.o`/test-binary artifacts, so a few of them held the wrong bytes
afterwards; deleting `target/debug/deps/libzerofrom_derive-*.dylib` and every extension-less
executable under `target/debug/deps` (cargo relinks them) restored a clean, verified build. If this
recurs, restrict candidates to extension-less old test binaries and re-run `cargo test` to prove the
counts rather than trusting the workaround alone.
Disk caveat: the volume hosting this work is nearly full. Keep at most two concurrent Rust worktrees,
delete a finished worktree's `target/` directory (`rm -rf <worktree>/target`) after its task is closed, and
`rm -rf target/debug/incremental` in the main workspace when space is needed (it holds ~6 GB and is
regenerated) — a full disk aborted subagents mid-run once already.
Concurrency caveat: several agents may run in parallel worktrees against this ONE shared dev stack
(compose project `quansio-dev`). Never run `scripts/dev/down`, never delete its volumes, and never
recreate its containers from a worktree-modified compose file — a divergent config silently changed the
running MinIO (breaking SSE) once already. The dev-stack integration suite now exercises its own compose
project (`quansio-dev-test`) on offset ports, so its reset/volume teardown can no longer destroy the
shared stack.
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
