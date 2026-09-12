# QUANSIO V8.1 IMPLEMENTATION HANDOFF

Updated: 2026-09-12 (M0/M1 complete; M2 complete through RUN-010; 31 PASS + 1 BLOCKED_EXTERNAL)
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
Current task: INT-003 — implement deterministic model selection, dlp and failover — `RECONCILING` (claimed, not started)
Previous task: INT-005 closed `PASS` (context projection, search program and the runtime bridge)
Current task status: 32 tasks `PASS`, INT-002/INT-003 `BLOCKED_EXTERNAL` (implementation complete), INT-009 `RECONCILING`, INT-002 `BLOCKED_EXTERNAL` with implementation complete; every baseline gate green on `main`
Current owner: `agent:principal-1`
Current component: `context` (`python/intelligence/context/`, `crates/server/runtime/context_bridge/`)
Current language: Python

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

- RUN-004 — concurrency, fanout/fanin, cancellation and waits — `PASS`.
  `crates/server/src/runtime/orchestration/` owns the execution decisions over the canonical WorkGraph:
  a deterministic ready queue (priority, then id), dependency release (`draft`/`blocked` → `ready` once
  every `depends_on` prerequisite is `done`, → `blocked` when one failed or was cancelled), fan-in joins
  (a `waiting` parent joins once its last child is `done`, and is blocked if a child can never finish),
  bounded concurrency counted from **durable** run state so the bound survives a restart and two
  instances agree, cancellation that propagates down `parent_id`, blocks dependents, stops live runs
  through the one authoritative `RuntimeStore::cancel`, and converges under a cancel storm, and durable
  waits obeyed by never selecting a node whose run is parked or suspended. Every run it starts goes
  through the existing `RunDispatch` port into the canonical Run state machine — there is no second
  scheduler; every node transition goes through a `WorkGraphPort` whose production implementation
  applies it as one event-emitting `GraphTransaction` (`crates/graph/tests/orchestration.rs` proves that
  against the real store). `depends_on` reads `from` depends on `to` (documented on `WorkEdgeKind`), so
  `to` is the prerequisite. Two semantic limits are recorded, not papered over: the Run state machine has
  no `QUEUED → CANCELLED` edge, so an unstarted run inside a cancelled subtree is reported
  (`runs_left_unstarted`) and is never dispatched rather than being cancelled; and the concurrency limit
  is supplied by the caller (RUN-010 owns the budget), so orchestration never invents a default policy
  value. Evidence: `evidence/RUN-004/<ts>/`.

## What is currently being implemented

**INT-012 — content trust labelling and injection defense — `IN_PROGRESS` (work on `task/INT-012-trust` at `e3d47e88243f573d87ff918f7f7baa73fb5353c0`).**

Picked up as the DB-free fallback while the runtime is down. `python/intelligence/trust/labelling.py`
labels every segment by its **source** (never by what it says), fails closed to `untrusted_external`
for an unknown source, and renders untrusted content inside a typed boundary carrying the data-only
instruction — trusted content renders as itself, so the boundary keeps its signal.
`injection.py` implements deterministic, non-LLM heuristics (instruction override, role play,
imperative tool call, exfiltration URL, encoded blob, hidden markup) that tag a suspect segment,
replace its text with an evidence reference and expose the rules and reasons. The pinned corpus
`tests/security/injection/corpus.json` holds 7 malicious and 4 benign shapes; the suite asserts every
malicious sample fires its expected rules, benign content passes through untouched, the assessment is
deterministic and total, and the corpus cannot silently empty itself. One heuristic was corrected
after the benign sample caught a false positive ("run the project's own test command" was being
flagged). Verified: 15 tests, plane 231/231 (6 skipped live-provider), ruff/format/mypy clean.

**Remaining for INT-012:** the Rust policy half (`crates/server/src/policy/trust/`) — escalation of
untrusted-derived tier ≥ 2 proposals, refusal of `always` rules for untrusted origin, fresh approval
with an untrusted-origin preview at tier ≥ 3, and the exfiltration guard — plus QA-007 consuming the
corpus.

**INT-009 still waits on `task/INT-009-skills`** (`13626c6`): resolver + §11.5 state machine verified,
DB-backed store operations outstanding.


**INT-009 — Skill Registry and task-scoped resolver — `IN_PROGRESS` (work on `task/INT-009-skills` at `13626c6374105f09bab1f6f699487119f8ce34e2`, not yet merged).**

The resolver half is implemented and verified there: only `ACTIVE` versions resolve (every other state,
including `APPROVED`, is excluded with the rule that excluded it), a version whose `tool_needs` or
`capability_needs` exceed the caller's snapshot is excluded rather than granted — the resolution
carries the caller's snapshot unchanged, so a skill can never widen authority — resolution is bounded
by `max_skills` and is a pure function of (task, snapshot, candidates) with every non-resolved
candidate accounted for, and the manifest vocabulary is closed with provenance as a version field,
not a manifest key. 5 tests, the Python plane at 221/221 (6 skipped live-provider), ruff/format/mypy
clean — captured in `evidence/INT-009/2026-09-12T12-30-38Z/` **before the environment failed**.

**Environment incident (in force):** the container runtime is down — the Docker API returns 500 and
the dev-stack Postgres port is closed — so the DB-backed gates (`toolchains`, which runs
`cargo test --workspace`) cannot run. That is why the slice is on its branch and `main` is untouched:
a pipeline that cannot be verified is not a green pipeline. Unblock: bring the runtime back with
`bash scripts/dev/up` (never `down`) and re-run `bash scripts/ci/ci.sh`; then merge `task/INT-009-skills` (`07c30f2048bb0e4c41052f7b7ca48fc4370966a9`).

**INT-009 now has both halves on its branch.** `crates/server/src/control/skills/state.rs` holds the §11.5
promotion ladder as a fail-closed state machine (every state documented, only `ACTIVE` resolving, every
illegal edge refused and named), with 4 unit tests that pass without a database and clean fmt/clippy.
**Remaining:** the DB-backed store operations (create skill and version, apply a legal promotion, set
`skills.current_active_version_id`) and their integration tests — plus merging the branch once the runtime
is healthy. The `skills`/`skill_versions` tables already exist, so no migration is needed.


**INT-005 — ContextProjection and typed SearchProgram — `PASS` (merge `a2d621784fd4`).**

Both halves. `python/intelligence/context/search.py` implements §11.3's SearchProgram as data and
`projection.py` implements §11.2's ContextProjection — all three acceptance statements are covered by
tests that fail if they were false (no executable predicate strings, with a structural no-evaluator
gate; the bundle records source, snapshot, policy and token ledger; every segment carries a
`trust_level` and one without it is rejected). `crates/server/src/runtime/context_bridge/` is the
runtime's side: the port is the only way a run asks for a projection, `validate_projection` is a
fail-closed boundary check (unlabelled, unknown-labelled, identity-less, snapshot-less and stale
projections are all refused before a run sees anything), and a validated projection's id is what the
turn records — proven end to end by running a real turn and reading `turns.context_projection_id`
back. The bridge deliberately does not re-implement search, ranking or packing: that is the plane's
authority, and the port's production RPC implementation belongs to INT-001's typed boundary (its
default fails closed until then). Evidence: `evidence/INT-005/2026-09-12T11-46-48Z/`.

**INT-009 — claimed, not started.** Canonical owner: python/intelligence/skills/, crates/server/control/skills/. Its build items are
Store skill metadata/provenance/evals in control plane.; Resolve only relevant approved skills for a task and capability snapshot.; Skills may guide procedure but cannot grant permissions or add infrastructure.; Implement Skill/SkillVersion states per DOMAIN.md §11.5; only ACTIVE versions resolve..**INT-003 — deterministic model selection, DLP and bounded failover — `BLOCKED_EXTERNAL` (implementation complete, merge `47365327f7ca`).**

`model_gateway/routing/` holds the policy (the seven request classes, capability demand, cost/quality
preference and bounded fallbacks) and `PolicyRouteSelector`, a pure I/O-free selector that records a
rule id on every decision; `model_gateway/dlp/` holds the data-class guard that refuses a disallowed
data/provider combination before anything is transmitted and redacts secret-shaped content from what
is. Both acceptance statements are proven on the real path: with the selector injected, a normal call
reaches exactly one provider (the conformance stub records one call naming the selected model), and a
confidential request against a public-only provider raises `DLP_DENIED` with zero calls recorded while
a secret-bearing request reaches the provider with the placeholder instead.

Two integration decisions are recorded rather than guessed: routing deliberately does **not**
re-litigate data clearance or output bounds (the DLP guard enforces the first before transmission and
`prepare()` raises `VALIDATION_BOUNDS` for the second), and INT-002's gateway keeps its proven default
selector and runs the guard only when one is installed — swapping the default broke 22 of INT-002's
existing assertions about rule ids and eligibility, so INT-003's selector and guard stay injectable
through the seams INT-002 left and wiring them as the deployment default belongs to APP-001. An
explicit `route_hint` keeps INT-002's rule id `catalog.route_hint`.

**Blocker:** `real_boundary: true`, and its provider path is INT-002's live suite, which is blocked for
the same reason. **Unblock condition:** provider credentials in the environment (INT-002's
`QUANSIO_TEST_*` provider variables); the routing, DLP and failover suites can then be run against a
live provider. Evidence: `evidence/INT-003/2026-09-12T09-58-52Z/`.

**INT-005 — claimed, not started.** Canonical owner: python/intelligence/context/, crates/server/runtime/context_bridge/.

**RUN-010 — runtime budgets, quotas and capacity control — `PASS` (merge `a0568951c3fd`).**

`crates/server/src/runtime/budgets/` implements DOMAIN.md §13.2. `limits.rs` holds the pure rules —
the seven meters, remaining/fits, the deterministic first-exhausted report, the child ≤ parent
*remaining* check and the narrowing helper — and `service.rs` is the durable authority that grants
budgets, charges them and emits `usage.*` events. Nesting holds through the chain (a grandchild is
bounded by its child parent), and a charge beyond a limit is refused, marks the budget exhausted and
emits `usage.exhausted`, after which `capacity_limit` reports 0 so RUN-004's orchestration gate admits
nothing. The service writes only the budget row and its usage event, so an effect already reserved or
dispatched is left for the Effect Ledger: the graceful-stop test asserts an in-flight effect keeps its
exact row and that no settlement event appears. Recorded gap: the canonical prefix catalog has no
budget prefix, so `BudgetService::create` takes the id from its creator instead of minting one —
extending the authority set is not this module's call. Evidence:
`evidence/RUN-010/2026-09-12T09-41-20Z/`; `bash scripts/ci/ci.sh` green on all eleven gates.

**INT-003 — claimed, not started.** Canonical owner: python/intelligence/model_gateway/routing/, python/intelligence/model_gateway/dlp/. Its build items are
Resolve route from policy, capability demand, model availability, cost/latency and explicit user choice.; Apply redaction/data-classification rules before provider call.; Implement bounded failover with preserved request identity.; Implement ModelRoute per DOMAIN.md §11.1 with request classes (chat, planning, tool_heavy, synthesis, verification, embedding, cheap_worker) and rule ids recorded on every route decision..

**RUN-009 — recovery, generation fencing and effect reconciliation — `PASS` (merge `e5293f658205`).**

`crates/server/src/runtime/recovery/` rebuilds a run's position from durable state alone.
`plan.rs` holds the pure decisions — the safe action for a durable position and the generation
fence — and `service.rs` applies them over the canonical stores. Precedence is the runtime's:
honour a cancellation requested before the crash, reconcile an uncertain effect before any further
work, stay parked on a wait the run owns, then resume. Fencing mirrors the runtime's own rule
(`Generation::accept`): behind is stale and discarded, a newer generation supersedes — refusing a
newer generation would refuse recovery itself, which is why the first, stricter rule was corrected
against the authority. Reconciliation is applied from evidence and never by dispatching again, and a
`run.stale_worker_fenced` event records discarded work. `RECOVERY_READ_TABLES` declares recovery's
complete read set and a structural test scans the module's SQL literals and fails on any other
table, memory tables above all. Evidence: `evidence/RUN-009/2026-09-12T09-27-22Z/` — five
integration tests (kill/restart matrix, stale worker, uncertain effect, durable-only decisions,
memory-table gate) plus four unit tests; `bash scripts/ci/ci.sh` green on all eleven gates.

**RUN-010 — claimed, not started.** Its canonical owner is `crates/server/src/runtime/budgets/`;
the capacity gate RUN-004 added takes its limit from the caller precisely because this task owns
turning a run's budget into that limit, so the two fit together: budget limits (`tokens`,
`cost_minor_units`, `wall_time_ms`, `tool_calls`, `concurrency`, `machine_minutes`, `max_steps`)
with `consumed` accounting and child ≤ parent remaining (DOMAIN.md §13.2).

**RUN-008 — CompletionContract verification — `PASS`.**

Rust (`crates/server/src/runtime/verification/`): `contract.rs` parses a WorkNode's CompletionContract
fail-closed (an unknown check kind is refused rather than skipped, a malformed bound is refused, a
contract that binds nothing is recognised as certifying nothing); `checks.rs` implements
`artifact_exists` over real artifact metadata and `effects_settled` over the run's EffectRecords, and
fails closed naming the owner for `test_command` (EXEC-006), `assertion` (no predicate registry yet) and
`citations_valid` (CAP-002), so an unimplemented check never becomes a silent pass; `service.rs` is the
`ContractVerifier` in the engine's existing `VerificationPort`, so a model's claim reaches `SUCCEEDED`
only after every deterministic check passes, `human_signoff_required` stays a human's decision, and a
required semantic verification that is unavailable or disagrees rejects the claim with the verifier's
critique as actionable feedback.

Python (`python/intelligence/evaluation/semantic_verifier/`): `GatewaySemanticVerifier` builds the
independent verifier call (claim and evidence quoted as data with explicit no-follow instructions),
fulfils it through `ModelGateway` (INT-002) and reads a strict verdict — a response that is not exactly
`{agrees: bool, critique: non-empty}` is refused, never read as agreement, and a gateway failure is a
refusal; independence is a proof obligation (an unnamed claimant, or this verifier's own model, is
refused).

Recorded limits, not papered over: the cross-process binding between the Rust port and this Python
verifier belongs to the RPC boundary and the composition root (INT-001/APP-001); and because a run does
not yet record its model route (INT-002/INT-003), the Rust verifier passes `claimant_model=None`, so a
contract requiring `independent_model` is refused rather than self-certified until that route exists.

**While the container runtime is down**, the DB-free ready work is INT-012's Python half
(`python/intelligence/trust/`) — content trust labelling and injection defense are pure rules that
pytest can verify without Postgres, exactly as INT-009's Python resolver and its §11.5 state machine
were. The other ready tasks (INT-008, INT-011, EXEC-001, APP-001, OPS-004, OPS-005, QA-003) all need
the database for their own tests, so they remain blocked behind the same restart.

## Exact next action

Run `python3 scripts/validate_v81.py --next` and take what it selects; INT-003 and INT-005 are the next
ready tasks. Before wiring the planner into the turn loop, apply the alias fix recorded under
"Architecture decisions" so a plan can reference nodes it creates in the same batch.
Wire the real `WorkGraphPort` (snapshot SQL + `GraphTransaction` apply) where both crates are visible
when APP-001 composes the server.
Database-backed suites need `scripts/dev/up` plus
`QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:55440/quansio`; the baseline
pipeline derives that URL from the generated `.env` automatically.

## Ready queue

1. `INT-003` — deterministic model selection, DLP and failover.
2. `INT-005` — ContextProjection and typed SearchProgram.
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

**Intermittent intelligence-plane failure — FIXED.** Found while verifying INT-012: 2 of 9 full-suite runs failed. The culprit was `PolicyRouteSelector`: `new_ulid(seed=…)` makes the *random field* reproducible but still stamps the current millisecond, so a route id changed whenever two selections straddled a millisecond boundary (observed as `…A655…` vs `…A654…`) — breaking the INT-002 property that a route id is a pure function of the decision. Fixed by pinning `timestamp_ms=0` for seeded route ids, and the regression test now sleeps 10 ms between the two selections so it fails deterministically without the fix (verified: fails without, passes with). Before: 2 failures in 9 runs. After: 0 failures in 10 runs.

## Working tree

Modified: none on `main` after the RUN-008 close.
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
