# QUANSIO V8.1 — GENERATED TASK CATALOG

> Generated from `registries/tasks.json` by `scripts/validate_v81.py --write`. Do not hand edit.

**Revision:** 2  
**Task count:** 99  
**GA-required:** 99  
**Real-boundary:** 27

## M0 — Authority, repository and build foundation

**Exit criteria:** Clean checkout builds all three workspaces, contracts generate reproducibly, dev stack reaches health, CI runs validator + architecture checks, and every legacy path has a disposition.

### GOV-001 — Inventory repository and reconcile implementation
- **Owner:** Architecture
- **Language:** Mixed
- **Component:** `repository`
- **Paths:** `docs/review/`, `evidence/GOV-001/`
- **Depends on:** none
- **Real boundary:** not required
- **GA:** required
- **Goal:** Create the factual baseline before code changes.

**Build**
- Inventory packages, services, databases, schemas, build systems, CI, runtime paths and product surfaces.
- Classify existing implementation against V8.1 canonical owners as ALREADY_COVERED, PARTIAL, GENUINE_GAP, SUPERSEDED, CONFLICT or IMPLEMENTED_UNDOCUMENTED.
- Record duplicate runtimes, stores, policy/effect paths and direct provider/tool paths.
- If no pre-existing product code exists, apply the greenfield rule in AGENTS.md: classify all tasks GENUINE_GAP and record 'no legacy authority found'.
- Initialize git on main if the directory is not a repository; the reconciliation report is the first evidence bundle.

**Acceptance**
- A committed reconciliation report covers every active package and deployable.
- No unknown canonical owner remains for code that can mutate durable state or cause external effects.

**Required tests/evidence**
- Repository inventory check
- duplicate-authority scan
- manual architecture review

### GOV-002 — Install V8.1 as the sole active implementation authority
- **Owner:** Architecture
- **Language:** Markdown/Python
- **Component:** `governance`
- **Paths:** `docs/archive/`, `README.md`, `AGENTS.md`
- **Depends on:** GOV-001
- **Real boundary:** not required
- **GA:** required
- **Goal:** Prevent agents from implementing against stale or competing dossiers.

**Build**
- Move superseded implementation authorities under docs/archive and exclude them from active agent instructions.
- Install DOSSIER.md, AGENTS.md, registries/tasks.json and progress.json as the only active authority set.
- Update root README/agent pointers to V8.1.
- Greenfield: close as trivially satisfied with evidence referencing GOV-001.

**Acceptance**
- Only V8.1 files are referenced by active implementation instructions.
- Archived material is readable but cannot be consumed as current task authority.

**Required tests/evidence**
- authority-pointer check
- archive exclusion check

### GOV-003 — Create canonical monorepo and language boundaries
- **Owner:** Build
- **Language:** Rust/Python/TypeScript
- **Component:** `monorepo`
- **Paths:** `Cargo.toml`, `python/pyproject.toml`, `pnpm-workspace.yaml`, `native/`
- **Depends on:** GOV-001
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make the target runtime layout compile from one repository.

**Build**
- Create Cargo workspace for trusted runtime/control/machine crates.
- Create Python workspace for intelligence/model/context adapters.
- Create pnpm workspace for desktop/web UI.
- Add minimal native helper projects for macOS and Windows only where OS APIs require them.
- Do not add new Go code; legacy Go may exist only behind a documented migration boundary until removed or retained by explicit ADR.
- Lay out the repository exactly as DOSSIER.md §17 (crates/core, graph, events, capability, tools, indexer, machine, qworkerd, cli, contracts; python/intelligence modules; apps/desktop, apps/web; packs/, sdk/, config/, evidence/).
- Pin toolchains: rust-toolchain.toml, Python 3.12+ with uv lockfile, pnpm with Node LTS; add cargo-deny, ruff, mypy, eslint configs.

**Acceptance**
- cargo check, Python import/type checks and TypeScript build pass from clean checkout.
- Each canonical owner maps to exactly one package/module.
- Directory layout matches DOSSIER.md §17 or documents an equivalent mapping in evidence.

**Required tests/evidence**
- clean bootstrap
- workspace dependency cycle check
- language-boundary import check

### GOV-004 — Establish canonical contract generation
- **Owner:** Platform
- **Language:** Rust/Python/TypeScript
- **Component:** `contracts`
- **Paths:** `schemas/`, `crates/contracts/`, `python/intelligence/contracts/`, `sdk/`
- **Depends on:** GOV-003
- **Real boundary:** not required
- **GA:** required
- **Goal:** Eliminate hand-written DTO drift.

**Build**
- Create Protobuf for internal RPC/events, OpenAPI 3.1 for public APIs and JSON Schema for persisted/config artifacts.
- Generate Rust, Python and TypeScript bindings in CI.
- Version contracts and add compatibility fixtures.
- Derive every message/schema from DOMAIN.md: IDs (§1), entities (§3–§13), command catalog (§14), error taxonomy (§15) and conventions (§17); a contract that disagrees with DOMAIN.md is a defect.
- Generate TypeScript/Python SDK clients from OpenAPI into sdk/ and Rust/Python gRPC bindings into crates/contracts and python/intelligence/contracts.

**Acceptance**
- Generated bindings are reproducible.
- No service owns a divergent copy of a canonical request/event schema.
- Every entity, command and error in DOMAIN.md has a generated contract; a DOMAIN.md drift check runs in CI.

**Required tests/evidence**
- schema lint
- binding regeneration diff
- backward-compatibility fixtures

### GOV-005 — Build CI and dossier consistency validation
- **Owner:** Build
- **Language:** Python/Shell
- **Component:** `ci`
- **Paths:** `scripts/ci/`, `.github/workflows/ or ci/`, `scripts/validate_v81.py`
- **Depends on:** GOV-002, GOV-003, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make drift and superficial completion fail automatically.

**Build**
- Run formatting, linting, type checks, unit tests, contract tests and architecture checks.
- Run scripts/validate_v81.py on every change.
- Publish machine-readable test/evidence summaries bound to commit and artifacts.
- Fail CI if `python scripts/validate_v81.py` fails, if generated views/bindings drift, or if a PASS task's git_commit is not reachable from main.

**Acceptance**
- A clean checkout passes the baseline pipeline.
- Tampering with tasks, dependencies, generated task view or manifest fails CI.

**Required tests/evidence**
- negative manifest test
- negative DAG test
- generated-view drift test

### GOV-006 — Define legacy migration and deletion plan
- **Owner:** Architecture
- **Language:** Mixed
- **Component:** `migration`
- **Paths:** `docs/review/legacy-migration-map.md`, `evidence/GOV-006/`
- **Depends on:** GOV-001, GOV-003
- **Real boundary:** not required
- **GA:** required
- **Goal:** Reuse useful code without preserving legacy authority.

**Build**
- For each legacy module choose KEEP_BEHIND_BOUNDARY, PORT, REPLACE or DELETE.
- Preserve provider/tool parsing and UI utilities only when they fit canonical owners.
- Create explicit removal tasks for parallel orchestrators, direct provider calls and shadow state.
- Greenfield: close as trivially satisfied with evidence referencing GOV-001.

**Acceptance**
- Every legacy authoritative path has an owner and removal/port disposition.
- No release-critical behavior depends on an undocumented compatibility path.

**Required tests/evidence**
- migration-map completeness
- legacy direct-path scan

### GOV-007 — Provide deterministic local development stack
- **Owner:** Platform
- **Language:** Docker/Rust/Python/TypeScript
- **Component:** `dev-environment`
- **Paths:** `infra/compose/`, `scripts/dev/`, `config/`
- **Depends on:** GOV-003, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Allow any implementation agent to run the product locally with repeatable dependencies.

**Build**
- Provide one command to start PostgreSQL, NATS JetStream, S3-compatible object storage and optional vector adapter.
- Seed test tenant/workspace identities and non-secret development config.
- Document reset and fixture commands in DOSSIER.md only.
- Include pgvector-enabled PostgreSQL 16, NATS JetStream, MinIO and a stub model provider for offline development; seed one personal tenant, one workspace and one teammate template.
- Provide `scripts/dev/up`, `down`, `reset`, `seed` and a health-ready check used by CI.

**Acceptance**
- Fresh environment reaches health-ready state without manual database edits.
- Reset produces deterministic empty baseline.

**Required tests/evidence**
- bootstrap smoke test
- reset/reseed test

### GOV-008 — Enforce architecture conformance rules
- **Owner:** Architecture
- **Language:** Rust/Python/TypeScript
- **Component:** `architecture-tests`
- **Paths:** `tests/architecture/`, `scripts/ci/arch_check.py`
- **Depends on:** GOV-003, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Stop shadow runtimes, stores and privileged shortcuts from entering the codebase.

**Build**
- Add dependency/import rules for canonical owners.
- Reject client-to-provider, worker-to-control-database and model-gateway-to-effect execution wiring.
- Reject new persistent stores, deployables or effect paths unless DOSSIER.md decision log is updated.
- Encode the forbidden wirings as fixtures: renderer->provider, python->control tables (write), qworkerd->control DB, gateway->effect execution, any crate other than crates/tools registering tools, any second scheduler/timer loop.

**Acceptance**
- Known forbidden dependency fixtures fail.
- Architecture checks run in CI and locally.

**Required tests/evidence**
- forbidden-import tests
- network/DB ownership tests
- new-deployable guard

## M1 — Canonical state, events and persistence

**Exit criteria:** PostgreSQL schema migrates from zero; commands are idempotent; every committed mutation has its RuntimeEvent; graphs, protocol state, artifacts, timers and projections are durable and tenant-scoped.

### CORE-001 — Implement authoritative persistence schema and migrations
- **Owner:** Core Platform
- **Language:** Rust/SQL
- **Component:** `persistence`
- **Paths:** `migrations/`, `crates/server/control/schema/`
- **Depends on:** GOV-004, GOV-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Create one authoritative control/runtime data model.

**Build**
- Model tenant, user, workspace, work graph, agents, runs, protocol state, approvals, effects, skills, capabilities, schedules and artifact metadata in PostgreSQL.
- Use forward migrations with rollback/forward-fix policy and tenant-scoped keys.
- Enable PostgreSQL row-level security on every tenant table with the tenant set per transaction; application scoping remains mandatory.
- Create the `derived` schema for rebuildable indexes (pgvector) separate from authoritative tables.
- Model every entity in DOMAIN.md §2–§13 including Teammate, Thread/Message/Attachment, Question, Turn/Step/Attempt, ToolCall, UserRule, Budget, Notification, WebhookSubscription, AuditEntry and UsageRecord.

**Acceptance**
- Schema can bootstrap from zero and migrate from supported legacy state.
- All authoritative rows carry stable IDs, tenant/workspace scope and timestamps/version where required.
- A cross-tenant query without tenant context returns zero rows under RLS.

**Required tests/evidence**
- migration up/down or forward-fix test
- tenant isolation test
- legacy fixture migration
- RLS no-context zero-rows test

### CORE-002 — Implement identity, generation and idempotency primitives
- **Owner:** Core Platform
- **Language:** Rust
- **Component:** `core-types`
- **Paths:** `crates/core/`
- **Depends on:** CORE-001, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Give every command, effect, lease and stream stable replay-safe identity.

**Build**
- Implement canonical IDs, command_id, idempotency_key, generation, fence token, cursor and correlation/causation IDs.
- Provide typed validation and database uniqueness constraints.
- Implement the typed prefixed ULID scheme and replay-safety primitives exactly as DOMAIN.md §1 (command_id, idempotency_key, generation, fence_token, revision, sequence, cursor, correlation/causation).

**Acceptance**
- Duplicate commands are replay-safe.
- Stale generations/fences are rejected before mutation or effect execution.

**Required tests/evidence**
- duplicate command test
- stale generation test
- ID round-trip fixtures

### CORE-003 — Implement RuntimeEvent store and transactional outbox
- **Owner:** Core Platform
- **Language:** Rust
- **Component:** `events`
- **Paths:** `crates/events/`
- **Depends on:** CORE-001, CORE-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Persist state transition evidence atomically with canonical mutations.

**Build**
- Define RuntimeEvent envelope and sequence rules.
- Write state mutation plus outbox entry in one transaction.
- Publish to NATS with retry and deduplication; consumers track durable cursors.
- Implement the RuntimeEvent envelope and event families of DOMAIN.md §9.1–§9.2; subject naming `q.<tenant>.<aggregate_type>.<type>`; Nats-Msg-Id dedup.

**Acceptance**
- No committed state transition lacks its corresponding event.
- Publisher restart cannot duplicate externally visible semantic events.

**Required tests/evidence**
- transaction rollback test
- outbox crash/replay test
- cursor resume test

### CORE-004 — Implement WorkGraph, AgentGraph and StateGraph stores
- **Owner:** Core Platform
- **Language:** Rust
- **Component:** `graphs`
- **Paths:** `crates/graph/`
- **Depends on:** CORE-001, CORE-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Represent work, participants and observed execution without parallel graph systems.

**Build**
- Implement versioned nodes/edges and graph reads for WorkGraph, AgentGraph and StateGraph.
- Keep definitions, delegation lineage and runtime observations distinct but linkable.
- Implement WorkNode/WorkEdge/AgentGraph edges/StateGraph links with the fields and kinds in DOMAIN.md §4–§5; enforce acyclicity for depends_on/parent_of.

**Acceptance**
- Graph reads are tenant-scoped and revision-aware.
- No feature-specific graph duplicates canonical work/agent/state ownership.

**Required tests/evidence**
- graph invariant tests
- revision conflict tests
- tenant isolation tests

### CORE-005 — Implement GraphTransaction
- **Owner:** Core Platform
- **Language:** Rust
- **Component:** `graphs`
- **Paths:** `crates/graph/transaction/`
- **Depends on:** CORE-003, CORE-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make semantic graph changes atomic and revision-safe.

**Build**
- Provide compare-and-set graph mutation with preconditions, emitted RuntimeEvents and outbox writes.
- Support atomic work/agent/state changes required by delegation and completion.

**Acceptance**
- Concurrent conflicting mutations deterministically accept one revision and reject stale writers.
- Events exactly match committed graph mutation.

**Required tests/evidence**
- concurrency CAS test
- transaction/event atomicity test

### CORE-006 — Implement durable protocol state and checkpoints
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `protocol-state`
- **Paths:** `crates/server/runtime/protocol_state/`, `crates/server/runtime/checkpoints/`
- **Depends on:** CORE-001, CORE-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make exact resume independent of semantic memory.

**Build**
- Persist tool/model calls, approvals, questions, waits, terminal/browser control, agent lifecycle and cancellation state.
- Define checkpoint metadata for workspace/browser/terminal snapshots and generation ownership.
- Persist ProtocolState with the fields in DOMAIN.md §5.7 and Checkpoint metadata per §5.8.

**Acceptance**
- Process crash can reconstruct the next safe protocol action without reading semantic memory.
- Unknown external-effect outcomes remain explicitly unsettled.

**Required tests/evidence**
- crash/restart replay test
- partial tool-call recovery test
- approval wait recovery test

### CORE-007 — Implement artifact and evidence storage
- **Owner:** Core Platform
- **Language:** Rust
- **Component:** `artifacts`
- **Paths:** `crates/server/artifacts/`
- **Depends on:** CORE-001, CORE-002, GOV-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Store durable user artifacts and immutable execution evidence.

**Build**
- Store metadata in PostgreSQL and bytes in S3-compatible object storage.
- Use content digests, immutable evidence descriptors, scoped grants and retention metadata.
- Implement Artifact/ArtifactVersion/Evidence per DOMAIN.md §10 with tenant-prefixed object keys, server-side encryption and multipart upload for attachments.

**Acceptance**
- Artifact bytes verify against recorded digest.
- Evidence cannot be silently overwritten; new versions create new identities.

**Required tests/evidence**
- digest corruption test
- grant scope test
- retention fixture

### CORE-008 — Implement scheduler, waits and durable timers
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `scheduler`
- **Paths:** `crates/server/scheduler/`
- **Depends on:** CORE-003, CORE-006
- **Real boundary:** not required
- **GA:** required
- **Goal:** Support routines and long-running work without polling loops hidden in features.

**Build**
- Implement durable timers, event waits, backoff, wakeups and cancellation.
- Timers publish canonical runtime events and are generation-fenced.
- Implement Routine firing semantics (cron/interval/event, timezone, absence policy) as timers per DOMAIN.md §13.1.

**Acceptance**
- Restart preserves due work without double firing.
- Cancelled or stale-generation timers cannot wake obsolete work.

**Required tests/evidence**
- restart timer test
- double-fire test
- cancel/fence test

### CORE-009 — Implement projections and resumable event streaming
- **Owner:** Core Platform
- **Language:** Rust
- **Component:** `projections`
- **Paths:** `crates/events/projections/`, `crates/server/api/stream/`
- **Depends on:** CORE-003, CORE-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Give clients read models without making UI caches authoritative.

**Build**
- Build projection consumers for run status, work graph summaries, notifications and activity timelines.
- Expose cursor-based replay semantics for WebSocket/SSE delivery.
- Implement the client stream frame protocol of DOMAIN.md §9.3 (durable event frames with cursor; transient live frames) and STREAM_BACKPRESSURE shedding.

**Acceptance**
- Client reconnect resumes from cursor without lost or reordered semantic events.
- Projection rebuild from event/store state is deterministic.
- Live frames are never persisted and clients render correctly when they are absent.

**Required tests/evidence**
- cursor reconnect test
- projection rebuild test
- slow-consumer backpressure test

## M2 — Runtime, policy, effects and recovery

**Exit criteria:** A Run can be created, advance through a bounded turn loop with validated tool calls, park on approval, survive a kill/restart, reserve/settle/reconcile effects, and be verified — all in Rust with no model provider required.

### RUN-001 — Implement authoritative Rust runtime state machine
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `runtime`
- **Paths:** `crates/server/runtime/state_machine/`
- **Depends on:** CORE-005, CORE-006, CORE-008
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make quansio-runtime the only authority that advances executable work.

**Build**
- Define Run, Step, Attempt and terminal/nonterminal states with typed transitions.
- Require transaction, generation and event emission for state changes.
- Implement the Run/Turn/Step/Attempt states and transitions exactly as DOMAIN.md §5.2–§5.5, including WAITING_* variants, SUSPENDED and BLOCKED_UNRECOVERABLE with typed reasons.

**Acceptance**
- Illegal transitions fail closed.
- Every accepted transition is durable, observable and replayable.

**Required tests/evidence**
- state transition matrix
- crash between transition/event test

### RUN-002 — Implement AgentThread, delegation and handoff lifecycle
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `agents`
- **Paths:** `crates/server/runtime/agents/`
- **Depends on:** RUN-001, CORE-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Support persistent teammates and scoped ephemeral workers on one runtime.

**Build**
- Implement AgentThread mailbox, parent/child lineage, capability narrowing, suspend/resume, handoff and join.
- Represent persistent and ephemeral agents through the same primitives with different lifecycle policies.
- Implement AgentThread states per DOMAIN.md §5.1; persistent teammates never JOIN, workers must.

**Acceptance**
- Child cannot widen authority.
- Handoff preserves conversation/work/evidence lineage and is recoverable.

**Required tests/evidence**
- capability narrowing test
- handoff recovery test
- fanout join test

### RUN-003 — Compile model plans into validated WorkGraph mutations
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `planning`
- **Paths:** `crates/server/runtime/planning/`
- **Depends on:** RUN-001, CORE-005
- **Real boundary:** not required
- **GA:** required
- **Goal:** Let models propose plans without giving them graph authority.

**Build**
- Define PlanProposal contract, validate bounded task structure, dependencies, acceptance and capability needs.
- Apply accepted proposals only through GraphTransaction.
- Implement PlanProposal validation per DOMAIN.md §4.5 including max_plan_nodes policy bound and CompletionContract inheritance.

**Acceptance**
- Malformed/cyclic/over-authorized plans are rejected.
- Accepted plans produce deterministic graph revisions and events.

**Required tests/evidence**
- cycle injection test
- invalid capability test
- plan replay fixture

### RUN-004 — Implement concurrency, fanout/fanin, cancellation and waits
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `orchestration`
- **Paths:** `crates/server/runtime/orchestration/`
- **Depends on:** RUN-001, RUN-002, RUN-003
- **Real boundary:** not required
- **GA:** required
- **Goal:** Execute independent work in parallel with bounded concurrency.

**Build**
- Implement ready-queue selection, dependency release, capacity tickets, cancellation propagation, joins and durable waits.
- Make ordering deterministic where semantics require it.

**Acceptance**
- No dependent work starts before prerequisites.
- Cancellation and retry do not produce duplicate consequential effects.

**Required tests/evidence**
- parallel DAG test
- cancel storm test
- retry/idempotency test

### RUN-005 — Implement Capability Projection
- **Owner:** Security
- **Language:** Rust
- **Component:** `capabilities`
- **Paths:** `crates/capability/`
- **Depends on:** CORE-002, RUN-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Derive the exact authority available to each run, agent and tool call.

**Build**
- Compose platform, organization, user, workspace, agent, skill, tool and execution-target constraints using only narrowing operations.
- Snapshot capability inputs for each consequential decision.
- Implement Grant, CapabilityProjection and the intersection/most-restrictive algebra with fixed layer order exactly as DOMAIN.md §6; log capability.widening_rejected.

**Acceptance**
- Delegation/skills/tools cannot widen authority.
- Missing or ambiguous security inputs fail closed.

**Required tests/evidence**
- narrowing property tests
- stale capability snapshot test

### RUN-006 — Implement policy, RBAC, privacy guards and approvals
- **Owner:** Security
- **Language:** Rust
- **Component:** `policy`
- **Paths:** `crates/server/policy/`
- **Depends on:** RUN-005, CORE-003
- **Real boundary:** not required
- **GA:** required
- **Goal:** Bind governance decisions to exact actions.

**Build**
- Evaluate RBAC/policy/privacy/sequence constraints.
- Issue ApprovalReceipt bound to actor, effect, parameters/digest, scope, expiry and generation.
- Support Ask/Always/Never style user rules where applicable.
- Implement Policy, UserRule, ApprovalRequest and ApprovalReceipt per DOMAIN.md §7.3 including consequence_preview typing, superseded-on-change and server signature.
- Integrate content-trust escalation hooks (INT-012 provides labels; policy enforces).

**Acceptance**
- Approval for one action cannot authorize a changed action.
- Denied/expired/stale approvals cannot execute.
- A UserRule cannot set `always` on a tier-4 effect class.

**Required tests/evidence**
- approval substitution test
- policy deny test
- privacy sequence test
- tier-4 always-rule rejection test

### RUN-007 — Implement Universal Effect Ledger
- **Owner:** Runtime/Security
- **Language:** Rust
- **Component:** `effects`
- **Paths:** `crates/server/effects/`
- **Depends on:** RUN-006, CORE-003
- **Real boundary:** not required
- **GA:** required
- **Goal:** Give every consequential external action one reservation/settlement path.

**Build**
- Classify semantic effects independent of low-level click/API mechanism.
- Implement reserve, dispatch token, settle, unknown outcome, reconcile and retry policy.
- Require idempotency/reconciliation strategy per effect class.
- Implement the effect class table, tiers, default policy and reconciliation strategies of DOMAIN.md §7.1 as configuration in config/effects.yaml validated against the taxonomy; EffectRecord states per §7.2.

**Acceptance**
- Browser clicks, connectors and tools that cause the same consequence share effect semantics.
- UNKNOWN effects are reconciled before retry.
- Every effect class in DOMAIN.md §7.1 has a registered tier, default decision and reconciliation strategy.

**Required tests/evidence**
- duplicate effect test
- unknown-outcome reconciliation test
- browser/API equivalence fixture

### RUN-008 — Implement CompletionContract verification
- **Owner:** Runtime/QA
- **Language:** Rust/Python
- **Component:** `verification`
- **Paths:** `crates/server/runtime/verification/`, `python/intelligence/evaluation/semantic_verifier/`
- **Depends on:** RUN-001, CORE-007, RUN-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Prevent model self-certification of task completion.

**Build**
- Define deterministic checks first, then optional independent semantic verification.
- Bind completion to expected artifacts, assertions, tests and effect settlement.
- Implement CompletionContract deterministic check kinds per DOMAIN.md §4.4 (artifact_exists, test_command, assertion, effects_settled, citations_valid) and the optional independent semantic verifier via the gateway.

**Acceptance**
- A model message alone cannot mark work succeeded.
- Failed required verification keeps work incomplete with actionable evidence.

**Required tests/evidence**
- false-done test
- missing artifact test
- semantic verifier disagreement test

### RUN-009 — Implement recovery, generation fencing and effect reconciliation
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `recovery`
- **Paths:** `crates/server/runtime/recovery/`
- **Depends on:** RUN-004, RUN-007, CORE-006
- **Real boundary:** not required
- **GA:** required
- **Goal:** Recover after process, worker and machine failures without corrupting state or repeating effects.

**Build**
- Rebuild from durable state/events/checkpoints.
- Fence stale workers and streams by generation/lease.
- Reconcile uncertain effects before resuming.
- Recovery must read only ProtocolState, RuntimeEvents, Checkpoints, Evidence and the Effect Ledger; add a test that fails if MemoryEntry tables are read during recovery.

**Acceptance**
- Killed runtime resumes to the same safe logical point.
- Stale worker output cannot mutate current run.

**Required tests/evidence**
- kill/restart matrix
- stale worker test
- uncertain effect recovery

### RUN-010 — Implement runtime budgets, quotas and capacity control
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `capacity`
- **Paths:** `crates/server/runtime/budgets/`
- **Depends on:** RUN-004, RUN-005
- **Real boundary:** not required
- **GA:** required
- **Goal:** Bound autonomous execution by explicit resource policy.

**Build**
- Enforce token, time, tool, concurrency, machine and cost budgets at run/agent/workspace scopes.
- Emit usage events and graceful budget-exhausted states.
- Implement Budget per DOMAIN.md §13.2 with child<=parent enforcement and usage.* events consumed by OPS-004.

**Acceptance**
- Budget exhaustion stops new work without corrupting active effects.
- Child budgets cannot exceed parent remaining allowance.

**Required tests/evidence**
- budget inheritance test
- capacity saturation test
- graceful stop test

### RUN-011 — Implement agent turn loop, Tool contract and Tool Registry
- **Owner:** Runtime
- **Language:** Rust
- **Component:** `turn-loop`
- **Paths:** `crates/tools/`, `crates/server/runtime/turn_loop/`
- **Depends on:** RUN-003, RUN-006, RUN-007, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make the model-driven loop that turns proposals into governed steps a single Rust-owned authority.

**Build**
- Implement the Turn loop exactly as DOMAIN.md §5.6: input -> ContextProjection request -> model call -> ModelProposal parse -> tool/plan/delegate/question/memory/completion handling -> bounded iteration.
- Define the Tool contract (DOMAIN.md §7.5) and a control-plane Tool Registry with versioned declarations, JSON Schema validation (additionalProperties=false), effect-class/resource derivation, idempotency-key derivation, host, output bounds and evidence capture.
- Implement the ToolCall protocol (DOMAIN.md §7.4) including parallel independent calls whose results are returned together, WAITING_* parking, dispatch tokens and OUTCOME_UNKNOWN handling.
- Implement the human question protocol (user.ask tool, Question entity, WAITING_QUESTION, expiry) and the agent-originated work.delegate / work.propose_plan / memory.propose tools as registry entries.
- Expose only capability-filtered tools to the model; record which tools/model/context were used per turn for UI inspection.

**Acceptance**
- A turn with a conformance-stub model (no provider) executes file/terminal tool proposals through capability, policy, approval wait and Effect Ledger, and produces durable Steps, Attempts and events.
- A tool proposal with unknown fields, an unregistered tool, or a tool outside the capability projection is rejected before any dispatch.
- A crash between dispatch and settlement resumes to OUTCOME_UNKNOWN reconciliation, never to a duplicate dispatch.
- Question and delegation waits are resumable after restart.

**Required tests/evidence**
- turn loop conformance suite with stub model
- tool schema rejection tests
- parallel tool result ordering test
- question/delegate wait recovery test
- crash-between-dispatch-and-settle test

## M3 — Model, context, search, memory and skills

**Exit criteria:** One chat turn reaches a live provider through the gateway with deterministic routing, trust-labelled bounded context, semantic retrieval, memory and skills resolved, and evaluations reproducible from pinned inputs.

### INT-001 — Create Python intelligence service and typed RPC boundary
- **Owner:** AI Platform
- **Language:** Python
- **Component:** `intelligence`
- **Paths:** `python/intelligence/`, `python/intelligence/contracts/`
- **Depends on:** GOV-004, CORE-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Isolate fast-moving AI logic from trusted Rust execution authority.

**Build**
- Create typed service for model gateway, context assembly, ranking, knowledge and capability compilation modules.
- Use generated Protobuf clients; no direct control-plane database mutation outside assigned read/derived stores.
- Use gRPC (tonic/grpcio) over Unix socket when co-located, mTLS when split; every RPC carries tenant/workspace scope, correlation_id, deadline and capability_projection_id.

**Acceptance**
- Service starts with health/version endpoints and generated contracts.
- Rust runtime can call it without shared in-process Python state.

**Required tests/evidence**
- RPC contract test
- service restart test
- forbidden DB mutation test

### INT-002 — Implement server-side model gateway
- **Owner:** AI Platform
- **Language:** Python
- **Component:** `model-gateway`
- **Paths:** `python/intelligence/model_gateway/`, `config/models.yaml`
- **Depends on:** INT-001, GOV-004
- **Real boundary:** required
- **GA:** required
- **Goal:** Route all model fulfillment through one trusted gateway.

**Build**
- Implement provider adapters, request normalization, streaming ModelEvent, cancellation, timeout and usage capture.
- Keep provider credentials in trusted server/local-service secret custody; never send them to renderer, normal worker or microVM guest.
- Ship adapters for Anthropic (primary), OpenAI, and an OpenAI-compatible generic endpoint; model ids and capabilities come from config/models.yaml only.
- Normalize per DOMAIN.md §11.1: streaming ModelEvent, strict-schema tool calling, operator-channel instructions, multimodal image/document input, prompt-cache hints (stable prefix first), usage incl. cache tokens, typed stop reasons incl. refusal, cancellation; disable provider-side compaction/context-editing.
- Provide a deterministic conformance-stub provider for offline tests; it never counts as real-boundary evidence.

**Acceptance**
- At least two provider adapters pass the same conformance suite.
- Cancellation closes stream and records terminal provider outcome.
- No provider-side compaction or context-editing feature is enabled; the conformance suite asserts the request shape.

**Required tests/evidence**
- live provider sandbox test
- stream/cancel test
- secret-leak scan

### INT-003 — Implement deterministic model selection, DLP and failover
- **Owner:** AI Platform
- **Language:** Python
- **Component:** `model-gateway`
- **Paths:** `python/intelligence/model_gateway/routing/`, `python/intelligence/model_gateway/dlp/`
- **Depends on:** INT-002, RUN-005
- **Real boundary:** required
- **GA:** required
- **Goal:** Select models without hidden prerequisite LLM calls and enforce data policy.

**Build**
- Resolve route from policy, capability demand, model availability, cost/latency and explicit user choice.
- Apply redaction/data-classification rules before provider call.
- Implement bounded failover with preserved request identity.
- Implement ModelRoute per DOMAIN.md §11.1 with request classes (chat, planning, tool_heavy, synthesis, verification, embedding, cheap_worker) and rule ids recorded on every route decision.

**Acceptance**
- Normal primary-model path performs zero mandatory auxiliary LLM calls before the selected model.
- Disallowed data/provider combinations fail before transmission.

**Required tests/evidence**
- route determinism test
- DLP denial test
- provider failover test

### INT-004 — Implement Rust indexer and SearchIndex API
- **Owner:** Context
- **Language:** Rust
- **Component:** `indexer`
- **Paths:** `crates/indexer/`
- **Depends on:** CORE-007, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide high-throughput exact, lexical, symbol and graph retrieval as a derived subsystem.

**Build**
- Index files, artifacts, knowledge sources, metadata, symbols and links with snapshot/index epochs.
- Use Tantivy/local indexes and server adapters; vectors remain derived/rebuildable.

**Acceptance**
- Index can rebuild from authoritative sources.
- Queries are snapshot/tenant scoped and return provenance.

**Required tests/evidence**
- incremental index test
- rebuild equivalence test
- tenant leakage test

### INT-005 — Implement ContextProjection and typed SearchProgram
- **Owner:** Context
- **Language:** Python/Rust
- **Component:** `context`
- **Paths:** `python/intelligence/context/`, `crates/server/runtime/context_bridge/`
- **Depends on:** INT-001, INT-004, RUN-005
- **Real boundary:** not required
- **GA:** required
- **Goal:** Build bounded model-visible context from canonical sources.

**Build**
- Define typed search AST with exact/lexical/semantic/graph/history/memory channels.
- Rank, dedupe and pack evidence under token budget with provenance and degradation reporting.
- Implement ContextProjection per DOMAIN.md §11.2 with trust_level on every segment (labels supplied per §12) and SearchProgram per §11.3; render stable prefix first for caching.

**Acceptance**
- No executable/arbitrary predicate strings are accepted.
- Context bundle records source, snapshot, policy and token ledger.
- Every segment carries a trust_level; a segment without one is rejected.

**Required tests/evidence**
- AST validation test
- budget packing test
- stale snapshot test

### INT-006 — Implement Knowledge Fabric
- **Owner:** Context
- **Language:** Python
- **Component:** `knowledge`
- **Paths:** `python/intelligence/knowledge/`
- **Depends on:** INT-005, CORE-007, INT-011
- **Real boundary:** not required
- **GA:** required
- **Goal:** Store durable evaluated knowledge separately from transcript and recovery state.

**Build**
- Define candidate, provenance, confidence, scope, version and lifecycle.
- Ingest approved sources and verified run outcomes; support supersession and deletion.
- Implement KnowledgeEntry per DOMAIN.md §11.4 with quarantine on source deletion; use INT-011 for the semantic channel.

**Acceptance**
- Knowledge entries are tenant/scoped and provenance-addressable.
- Removing source invalidates or marks affected derived knowledge.

**Required tests/evidence**
- provenance test
- tenant isolation test
- source deletion propagation

### INT-007 — Implement semantic memory with provenance
- **Owner:** Context
- **Language:** Python
- **Component:** `memory`
- **Paths:** `python/intelligence/memory/`
- **Depends on:** INT-006, RUN-005
- **Real boundary:** not required
- **GA:** required
- **Goal:** Remember useful user/workspace decisions without becoming a recovery mechanism.

**Build**
- Create memory candidates from explicit user direction and verified work.
- Gate writes by policy and provenance; retrieve only task-relevant memories through ContextProjection.
- Implement MemoryEntry per DOMAIN.md §11.4; writes only through the memory.propose tool gated by policy; scopes user/workspace/teammate.

**Acceptance**
- Runtime restart succeeds with memory disabled.
- Memory deletion prevents future retrieval after index refresh.

**Required tests/evidence**
- memory-off recovery test
- delete/retrieval test
- cross-tenant isolation

### INT-008 — Implement compaction epochs and bounded conversation projection
- **Owner:** Runtime/Context
- **Language:** Rust/Python
- **Component:** `compaction`
- **Paths:** `crates/server/runtime/compaction/`, `python/intelligence/context/compaction/`
- **Depends on:** CORE-006, INT-005
- **Real boundary:** not required
- **GA:** required
- **Goal:** Control long-running context size without losing protocol truth.

**Build**
- Create versioned compaction epochs tied to source event ranges.
- Reject stale async compaction after fork/revert and provide synchronous fallback.
- Keep tool/protocol state outside summaries.

**Acceptance**
- Fork/revert never installs compaction from abandoned history.
- Exact protocol replay does not depend on summary text.

**Required tests/evidence**
- stale compaction test
- fork/revert test
- fallback test

### INT-009 — Implement Skill Registry and task-scoped resolver
- **Owner:** AI Platform
- **Language:** Python/Rust
- **Component:** `skills`
- **Paths:** `python/intelligence/skills/`, `crates/server/control/skills/`
- **Depends on:** INT-005, RUN-005, CORE-001
- **Real boundary:** not required
- **GA:** required
- **Goal:** Treat skills as versioned procedural assets rather than autonomous subsystems.

**Build**
- Store skill metadata/provenance/evals in control plane.
- Resolve only relevant approved skills for a task and capability snapshot.
- Skills may guide procedure but cannot grant permissions or add infrastructure.
- Implement Skill/SkillVersion states per DOMAIN.md §11.5; only ACTIVE versions resolve.

**Acceptance**
- Unapproved skill version cannot enter production context.
- Skill resolution is bounded and deterministic for fixed inputs.

**Required tests/evidence**
- promotion-state test
- capability narrowing test
- resolver budget test

### INT-010 — Implement intelligence evaluation harness
- **Owner:** QA/AI Platform
- **Language:** Python
- **Component:** `evaluation`
- **Paths:** `python/intelligence/evaluation/`, `tests/evaluation/`
- **Depends on:** INT-002, INT-005, INT-009
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make model/context/skill changes measurable before promotion.

**Build**
- Define versioned datasets for route quality, retrieval, answer grounding, tool proposal validity and skill behavior.
- Record model/provider/index/skill versions and cost/latency.
- Encode the provisional thresholds from DOSSIER.md §21.3 as the initial protected gate configuration in tests/evaluation/thresholds.yaml.

**Acceptance**
- Evaluation runs are reproducible from pinned inputs.
- Protected safety/recovery regressions block promotion regardless of aggregate quality gain.

**Required tests/evidence**
- dataset digest check
- regression threshold test
- cost/latency capture

### INT-011 — Implement embedding pipeline and derived vector index
- **Owner:** Context
- **Language:** Python/SQL
- **Component:** `embeddings`
- **Paths:** `python/intelligence/embeddings/`, `migrations/derived/`
- **Depends on:** INT-002, INT-004, CORE-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide semantic retrieval as a derived, rebuildable index without a second knowledge store.

**Build**
- Implement chunking, embedding via the model gateway (embedding request class), and pgvector storage in the PostgreSQL `derived` schema keyed by tenant/workspace, source ref, snapshot and content digest.
- Support incremental re-embedding on source change, deletion propagation on source/knowledge/memory delete, and full rebuild from authoritative sources and object storage.
- Expose the semantic channel to SearchProgram execution with provenance and snapshot metadata.

**Acceptance**
- Index rebuild from authoritative sources produces equivalent retrieval results.
- Deleted sources disappear from semantic retrieval within the published SLO.
- Cross-tenant semantic retrieval is impossible by construction (tenant filter is mandatory in the query API).

**Required tests/evidence**
- rebuild equivalence test
- deletion propagation test
- tenant filter mandatory test
- embedding provider failover test

### INT-012 — Implement content trust labelling and injection defense
- **Owner:** Security/Context
- **Language:** Python/Rust
- **Component:** `trust`
- **Paths:** `python/intelligence/trust/`, `crates/server/policy/trust/`, `tests/security/injection/`
- **Depends on:** INT-005, RUN-006, RUN-011
- **Real boundary:** not required
- **GA:** required
- **Goal:** Treat web, email, document, file and tool content as data so injected instructions cannot become authorized actions.

**Build**
- Label every ContextProjection segment with a trust level (DOMAIN.md §12) from its source; render untrusted segments inside typed boundaries with a data-only instruction.
- Propagate derived_from_trust onto every ModelProposal element using the causal window of referenced segments.
- Implement policy-side escalation: untrusted-derived tier>=2 proposals escalate one tier, cannot use `always` rules, and require fresh approval with untrusted-origin preview at tier>=3.
- Implement the exfiltration guard: untrusted or protected-class content cannot be sent to a destination outside the current egress grant set without a data.upload.protected approval.
- Implement deterministic (non-LLM) injection heuristics that tag suspected segments, truncate them to evidence references in context and surface them in UI.
- Build and pin the injection corpus (web pages, emails, documents, tool outputs) used by QA-007.

**Acceptance**
- Zero unauthorized effect executions across the pinned injection corpus.
- An instruction embedded in a fetched page cannot change a tool call's target or bypass approval.
- Trust labels and escalation decisions are visible in the approval preview and evidence timeline.

**Required tests/evidence**
- injection corpus suite
- escalation policy test
- exfiltration guard test
- trust propagation unit tests

## M4 — Execution fabric, browser, computer and integrations

**Exit criteria:** A worker under lease executes file/terminal/browser/connector/git tools inside a real isolated target on at least one substrate, with deny-by-default egress and no credential reaching the guest.

### EXEC-001 — Implement Rust machine-control and execution-target lifecycle
- **Owner:** Execution
- **Language:** Rust
- **Component:** `machine-control`
- **Paths:** `crates/machine/control/`
- **Depends on:** RUN-005, CORE-003
- **Real boundary:** not required
- **GA:** required
- **Goal:** Own placement, generations, leases and lifecycle for all execution environments.

**Build**
- Implement ExecutionTarget, Lease, generation, desired/observed state and health.
- Support persistent workspace, isolated task runtime, local capsule, Windows VM and private worker classes behind one contract.
- Implement ExecutionTarget classes/substrates, states and Lease per DOMAIN.md §8.1–§8.3.

**Acceptance**
- Only machine-control mutates target/lease lifecycle.
- Lease expiry or generation change fences old controllers.

**Required tests/evidence**
- lease race test
- generation fence test
- lifecycle transition test

### EXEC-002 — Implement qworkerd typed endpoint/guest protocol
- **Owner:** Execution
- **Language:** Rust
- **Component:** `qworkerd`
- **Paths:** `crates/qworkerd/`, `crates/machine/gateway/`
- **Depends on:** EXEC-001, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide one safe worker daemon for local, microVM and private-worker execution.

**Build**
- Implement outbound authenticated control channel, heartbeat, lease validation, tool dispatch, stream output, checkpoint hooks and evidence upload.
- Do not allow direct control-plane database access.
- qworkerd exposes tool execution only for dispatch tokens issued by the runtime (RUN-011); it never evaluates policy itself.

**Acceptance**
- Disconnected worker resumes or is fenced without duplicate effect execution.
- Malformed/stale action envelopes fail before tool execution.

**Required tests/evidence**
- disconnect/reconnect test
- stale envelope test
- network ACL test

### EXEC-003 — Implement macOS local Linux microVM capsule
- **Owner:** Execution
- **Language:** Rust/Swift
- **Component:** `local-capsule`
- **Paths:** `native/macos/`, `crates/machine/substrates/macos_capsule/`
- **Depends on:** EXEC-001, EXEC-002
- **Real boundary:** required
- **GA:** required
- **Goal:** Provide isolated local execution on macOS.

**Build**
- Use Apple virtualization APIs through minimal native bridge, private guest channel and host-side networking.
- Launch qworkerd in guest; expose file/terminal/browser services only through typed protocol.
- Cookie/saved-login import requires explicit user permission.
- Ship or download a pinned, digest-verified Linux guest image (qworkerd, Chromium, base tooling) through the desktop update channel.

**Acceptance**
- Guest cannot reach host secrets or cloud metadata by default.
- Start/stop/reset and checkpoint flows are repeatable.

**Required tests/evidence**
- real macOS VM smoke test
- permission denial test
- reset isolation test

### EXEC-004 — Implement Windows local capsule and native broker
- **Owner:** Execution
- **Language:** Rust/Windows native
- **Component:** `windows-execution`
- **Paths:** `native/windows/`, `crates/machine/substrates/windows/`
- **Depends on:** EXEC-001, EXEC-002
- **Real boundary:** required
- **GA:** required
- **Goal:** Provide equivalent managed execution on Windows.

**Build**
- Use WSL/Linux capsule for generic workloads and narrow native broker for Windows-only app automation.
- Preserve same ExecutionTarget, capability, effect and evidence contracts.

**Acceptance**
- Windows path passes shared worker/tool conformance suite.
- Native broker exposes no arbitrary host command escape.

**Required tests/evidence**
- real Windows smoke test
- broker allowlist test
- lease fence test

### EXEC-005 — Implement cloud microVM execution fabric
- **Owner:** Execution/Platform
- **Language:** Rust/Infra
- **Component:** `cloud-execution`
- **Paths:** `crates/machine/substrates/cloud_microvm/`, `infra/images/`
- **Depends on:** EXEC-001, EXEC-002
- **Real boundary:** required
- **GA:** required
- **Goal:** Provide hardware-isolated on-demand persistent and disposable execution targets.

**Build**
- Implement immutable images, CoW/snapshot restore, warm pool, placement, host networking and guest credential isolation.
- Keep substrate replaceable behind ExecutionTarget/Lease contracts.

**Acceptance**
- Tenant workloads are isolated and do not receive host/cloud root credentials.
- Lost node results in fenced replacement and safe runtime recovery.

**Required tests/evidence**
- cloud microVM integration test
- node loss test
- tenant isolation test

### EXEC-006 — Implement file, terminal and process tool host
- **Owner:** Execution
- **Language:** Rust
- **Component:** `tools`
- **Paths:** `crates/qworkerd/tools/`
- **Depends on:** EXEC-002, RUN-007, RUN-011
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide typed local/guest tools with bounded outputs and evidence.

**Build**
- Implement file read/write/patch, directory operations, process spawn, PTY/terminal replay and command cancellation.
- Every mutating operation carries capability/effect/idempotency context.
- Implement fs.read/fs.write/fs.patch/terminal.exec/process.spawn as Tool Registry entries with effect classes fs.write.workspace / process.exec.sandboxed; TerminalSession per DOMAIN.md §8.5.

**Acceptance**
- Path traversal and unauthorized roots are denied.
- Terminal replay resumes from durable cursor without duplicating command.

**Required tests/evidence**
- path traversal test
- PTY reconnect test
- command cancellation test

### EXEC-007 — Implement secret broker and opaque credential handles
- **Owner:** Security/Execution
- **Language:** Rust
- **Component:** `secrets`
- **Paths:** `crates/machine/secrets/`
- **Depends on:** RUN-005, EXEC-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Keep raw credentials out of model context, renderer and normal guest payloads.

**Build**
- Resolve opaque credential handles only at approved execution boundary.
- Support short-lived scoped materialization, rotation/revocation and audit.
- Implement SecretHandle and credential.access effect (tier 4, always audited); KeyProvider abstraction with cloud KMS and local master-key implementations.

**Acceptance**
- Secret values are absent from model/tool logs and normal RPC payloads.
- Revoked handle cannot be reused from cached worker state.

**Required tests/evidence**
- secret scan
- revocation test
- scope substitution test

### EXEC-008 — Implement deny-by-default network and egress policy
- **Owner:** Security/Execution
- **Language:** Rust
- **Component:** `network`
- **Paths:** `crates/machine/egress/`
- **Depends on:** EXEC-001, RUN-006
- **Real boundary:** not required
- **GA:** required
- **Goal:** Control where execution targets and connectors can communicate.

**Build**
- Enforce DNS/domain/IP/port policy at host/network gateway.
- Bind egress grants to tenant, target, capability and lifetime; log decisions without leaking secrets.
- Implement network.egress.new_destination grants bound to tenant/target/capability/lifetime; webhook and connector egress use the same broker.

**Acceptance**
- Unknown destinations are denied by default.
- Policy changes fence or expire affected grants safely.

**Required tests/evidence**
- blocked-domain test
- DNS rebinding test
- grant expiry test

### EXEC-009 — Implement managed browser session with DOM/CDP-first control
- **Owner:** Execution
- **Language:** Rust + optional adapter
- **Component:** `browser`
- **Paths:** `crates/qworkerd/browser/`
- **Depends on:** EXEC-002, EXEC-008, RUN-007, RUN-011
- **Real boundary:** required
- **GA:** required
- **Goal:** Give agents a live browser that does not depend on screenshots for normal understanding.

**Build**
- Maintain one browser session exposed to agent and user.
- Use DOM, CDP, accessibility and network metadata first; screenshot/vision is fallback.
- Persist safe session/checkpoint metadata and return typed observations/evidence.
- Implement the Rust CDP driver inside qworkerd; BrowserSession per DOMAIN.md §8.4; CDP screencast frames as live frames; user input relay for takeover.
- Provide headless fetch/extract mode as the web.fetch tool (readable text + links + metadata, bounded output, trust UNTRUSTED_EXTERNAL); no separate scraper runtime.
- Register browser.navigate/click/type/extract/screenshot tools with effect derivation from page semantics (form submit that sends/buys/deletes -> corresponding tier 3/4 class).

**Acceptance**
- Research and action workflows use the same browser stack.
- Browser action that causes external consequence routes through Effect Ledger.
- web.fetch output is bounded, labelled UNTRUSTED_EXTERNAL and carries the source URL, retrieval time and content digest.

**Required tests/evidence**
- DOM action test
- screenshot fallback test
- effect classification test
- session resume test
- web.fetch bounds/labelling test

### EXEC-010 — Implement computer-use and human takeover
- **Owner:** Execution
- **Language:** Rust/Swift/Windows native
- **Component:** `computer-use`
- **Paths:** `crates/machine/computer_use/`, `native/macos/`, `native/windows/`
- **Depends on:** EXEC-009, RUN-006
- **Real boundary:** required
- **GA:** required
- **Goal:** Control native applications through a narrow privileged bridge while allowing user takeover.

**Build**
- Use app identity, accessibility tree and bounded screenshot input.
- Support read/click/type/clipboard/system-key capability tiers.
- Fence automation while user has control and resume only after explicit handback.
- Implement computer.click/type/clipboard/system-key tools with tiers via GrantComputerTier; app identity resolution via AX (macOS) and UIA (Windows).

**Acceptance**
- Unknown foreground app fails closed for privileged action.
- User takeover cannot race with agent input.

**Required tests/evidence**
- app identity test
- takeover race test
- permission tier test

### EXEC-011 — Implement connector and integration broker
- **Owner:** Integrations
- **Language:** Rust/Python adapters
- **Component:** `connectors`
- **Paths:** `crates/server/control/connectors/`, `python/intelligence/adapters/connectors/`
- **Depends on:** RUN-007, EXEC-007, EXEC-008, RUN-011
- **Real boundary:** required
- **GA:** required
- **Goal:** Connect external SaaS/APIs without bypassing canonical effects.

**Build**
- Maintain connector metadata and auth handles in control plane.
- Run adapters in restricted worker boundary; normalize reads/actions/events.
- All consequential writes reserve/settle EffectRecord.
- Ship GA connectors: GitHub, Google Workspace (Gmail read/send, Drive, Calendar), Slack, and one web-search provider adapter exposed as the web.search tool.
- Implement Connector states (connected/degraded/revoked) and OAuth begin/complete commands; tokens as secret handles only.

**Acceptance**
- Connector token never reaches renderer/model.
- Webhook/event redelivery is idempotent and tenant scoped.
- Each GA connector passes the shared connector conformance suite in its provider sandbox.

**Required tests/evidence**
- provider sandbox test
- token isolation test
- webhook replay test

### EXEC-012 — Implement source-control, PR and CI tools
- **Owner:** Execution
- **Language:** Rust/Python adapters
- **Component:** `developer-tools`
- **Paths:** `crates/qworkerd/tools/scm/`, `python/intelligence/adapters/connectors/github/`
- **Depends on:** EXEC-006, EXEC-011
- **Real boundary:** required
- **GA:** required
- **Goal:** Support coding work as ordinary governed tool capability.

**Build**
- Implement Git status/diff/branch/commit/worktree operations and provider adapters for PR/CI status/actions.
- Bind remote writes such as push/PR/comment/merge to effect/approval policy.
- Bind push-to-own-branch as scm.remote.write allow+log and PR/comment/merge as ask by default per DOMAIN.md §7.1.

**Acceptance**
- Local read operations work offline in execution target.
- Remote source-control writes are idempotent/effect-tracked.

**Required tests/evidence**
- git worktree test
- PR sandbox test
- merge approval test

## M5 — Product server, desktop/web/CLI and collaboration

**Exit criteria:** A new user signs in, creates a teammate, runs a chat objective end-to-end from desktop, approves an effect with exact preview, watches the browser, reviews artifacts, and the same journey works via web, CLI and SDK.

### APP-001 — Build Rust quansio-server API/control composition
- **Owner:** Server
- **Language:** Rust
- **Component:** `server`
- **Paths:** `crates/server/`, `crates/server/api/`
- **Depends on:** RUN-001, CORE-009, RUN-006, RUN-011, INT-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Expose canonical product commands without creating a second orchestration layer.

**Build**
- Compose API, control, runtime command facade, policy, schedule, artifact metadata and notification modules in one deployable by default.
- Expose REST/OpenAPI and WebSocket endpoints with auth, tenant scope, idempotency and typed errors.
- Expose the command catalog and read projections of DOMAIN.md §14 as OpenAPI-generated endpoints with the error taxonomy of §15; per-tenant API rate limits; feature flags from config/flags.yaml.
- Walking skeleton: a smoke test drives one chat turn end-to-end (HTTP command -> runtime -> gateway with conformance-stub or live provider -> tool proposal -> Effect Ledger -> projection -> WebSocket event).

**Acceptance**
- Every mutating endpoint calls the canonical owner rather than writing foreign tables.
- Generated clients pass contract tests.
- The walking-skeleton smoke test passes in CI with the conformance-stub provider.

**Required tests/evidence**
- API contract suite
- cross-owner mutation scan
- WebSocket reconnect test
- walking-skeleton smoke test

### APP-002 — Implement identity, onboarding, workspaces and settings
- **Owner:** Product/Server
- **Language:** Rust/TypeScript
- **Component:** `onboarding`
- **Paths:** `crates/server/control/identity/`, `apps/desktop/src/onboarding/`
- **Depends on:** APP-001, CORE-001
- **Real boundary:** not required
- **GA:** required
- **Goal:** Get a new user from sign-in to a usable governed workspace.

**Build**
- Implement account/session, workspace creation, model/provider setup, execution target choice, privacy/security defaults and layered settings.
- Keep secrets as handles; show effective policy/settings and restart/reindex effects.
- Baseline auth per OD-002 (email magic link + Google/Microsoft/GitHub OAuth); access JWT + rotating refresh in OS keychain; personal tenant auto-provisioned.

**Acceptance**
- Fresh user can complete onboarding without manual configuration files.
- Settings merge is deterministic and most-restrictive for security controls.

**Required tests/evidence**
- new-user E2E
- settings precedence test
- secret setup test

### APP-003 — Build Electron desktop shell and secure preload bridge
- **Owner:** Desktop
- **Language:** TypeScript
- **Component:** `desktop`
- **Paths:** `apps/desktop/`
- **Depends on:** GOV-003, APP-001
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide the primary desktop product without renderer privilege.

**Build**
- Implement Electron main, React renderer, secure preload, deep links, update hooks and typed server/local-runtime clients.
- No Node integration or arbitrary host API from renderer.
- Renderer receives model deltas, terminal bytes and browser frames only via the server stream client; externalize UI strings from day one.

**Acceptance**
- Renderer security baseline passes.
- Desktop reconnects to server/runtime without losing active run state.

**Required tests/evidence**
- CSP/preload test
- desktop reconnect E2E
- permission boundary test

### APP-004 — Implement chat, objective and thread experience
- **Owner:** Product/Desktop
- **Language:** TypeScript/Rust
- **Component:** `conversation`
- **Paths:** `apps/desktop/src/conversation/`, `crates/server/api/threads/`
- **Depends on:** APP-002, APP-003, INT-002, RUN-011
- **Real boundary:** not required
- **GA:** required
- **Goal:** Support fast assistant turns and durable long-running objectives through the same runtime.

**Build**
- Create chat/objective composer, streaming model output, tool/progress cards, thread persistence and cancel/resume.
- Map every objective to WorkGraph/Run rather than a UI-only workflow.
- Support file attachments (upload -> Artifact -> message), in-thread agent questions (answer via AnswerQuestion) and a per-turn inspector showing model route, context sources, trust labels and tools used.

**Acceptance**
- Chat and long objective survive client reload.
- User can inspect which model/context/tools were used.
- Attachments and answered questions survive reload and appear in the run timeline.

**Required tests/evidence**
- streaming UI test
- reload/resume test
- cancel test
- attachment/question E2E

### APP-005 — Implement plan, WorkGraph and agent control experience
- **Owner:** Product/Desktop
- **Language:** TypeScript
- **Component:** `work-monitor`
- **Paths:** `apps/desktop/src/work/`
- **Depends on:** APP-004, RUN-003, RUN-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make autonomous work inspectable and steerable.

**Build**
- Render plan/dependencies, active agents, handoffs, blockers and completion contracts.
- Support user edits through validated graph commands, not local mutation.

**Acceptance**
- Graph view matches canonical revision.
- Concurrent server update produces conflict/rebase UX rather than silent overwrite.

**Required tests/evidence**
- graph projection E2E
- revision conflict test
- agent handoff UI test

### APP-006 — Implement approvals, effects, timeline and evidence UI
- **Owner:** Product/Desktop
- **Language:** TypeScript
- **Component:** `trust-ui`
- **Paths:** `apps/desktop/src/trust/`
- **Depends on:** APP-005, RUN-006, RUN-007, CORE-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Show exactly what Quansio wants to do and why.

**Build**
- Display scoped approval request, effect semantics, target, data involved, expiry and consequences.
- Render run/effect/evidence timeline and verification status.
- Show untrusted-origin warnings and trust escalation reasons in approval previews; show OUTCOME_UNKNOWN and reconciliation state distinctly.

**Acceptance**
- Approval UI cannot approve changed payload after preview.
- Unknown/unsettled effects remain visibly unresolved.

**Required tests/evidence**
- approval binding E2E
- effect timeline E2E
- evidence digest error UI

### APP-007 — Implement live browser/computer surface
- **Owner:** Product/Desktop
- **Language:** TypeScript
- **Component:** `browser-ui`
- **Paths:** `apps/desktop/src/live/`
- **Depends on:** APP-003, EXEC-009, EXEC-010, EXEC-006
- **Real boundary:** required
- **GA:** required
- **Goal:** Let users observe and take over the same live session used by the agent.

**Build**
- Embed live browser/computer stream, navigation/session state, agent cursor state and takeover controls.
- Show when automation is paused by user control or policy.
- Include the live terminal view (PTY replay from durable cursor) alongside browser/computer views.

**Acceptance**
- No hidden replacement session is created on takeover.
- Agent input is fenced while user owns control.

**Required tests/evidence**
- single-session test
- takeover E2E
- reconnect test

### APP-008 — Implement artifact, file and document workspace
- **Owner:** Product/Desktop
- **Language:** TypeScript
- **Component:** `artifacts`
- **Paths:** `apps/desktop/src/artifacts/`
- **Depends on:** APP-003, CORE-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make generated/retrieved work first-class rather than buried in chat.

**Build**
- Provide artifact browser, version history, safe preview/download/open, diff where applicable and source/evidence links.
- Support document/spreadsheet/slide/media artifact metadata.
- Provide global search across threads, artifacts, knowledge and evidence via the typed /search endpoint.

**Acceptance**
- Artifacts remain available after chat/run closure.
- Preview cannot execute active content with host privileges.

**Required tests/evidence**
- artifact persistence E2E
- malicious preview test
- version history test

### APP-009 — Implement collaboration, group chat, reactions and handoffs
- **Owner:** Product
- **Language:** Rust/TypeScript
- **Component:** `collaboration`
- **Paths:** `crates/server/control/collaboration/`, `apps/desktop/src/collaboration/`
- **Depends on:** APP-001, RUN-002, CORE-009
- **Real boundary:** not required
- **GA:** required
- **Goal:** Support multi-participant human/agent collaboration on canonical threads.

**Build**
- Implement membership, mentions, reactions, message/event ordering, agent handoff and attention state.
- Enforce tenant/workspace permissions for every participant.

**Acceptance**
- Concurrent messages preserve stable order/IDs.
- Removed participant cannot receive new protected events.

**Required tests/evidence**
- multi-client ordering test
- membership revoke test
- handoff E2E

### APP-010 — Implement routines, schedules, events and notifications
- **Owner:** Product
- **Language:** Rust/TypeScript
- **Component:** `automation`
- **Paths:** `crates/server/control/routines/`, `crates/server/notify/`, `apps/desktop/src/automation/`
- **Depends on:** CORE-008, APP-001, RUN-001
- **Real boundary:** not required
- **GA:** required
- **Goal:** Allow recurring/event-driven work without separate automation runtime.

**Build**
- Create routine definitions that compile to WorkGraph triggers/waits.
- Implement event subscriptions, notification preferences, absence handling and retry/backoff.
- Notification channels: in-app, desktop native, email (OD-009); attention kinds per DOMAIN.md §13.3.

**Acceptance**
- Restart does not duplicate scheduled runs.
- Routine uses same policy/capability/effect path as interactive work.

**Required tests/evidence**
- schedule restart test
- event dedupe test
- notification preference E2E

### APP-011 — Implement memory, knowledge, skills and capability settings UI
- **Owner:** Product
- **Language:** TypeScript
- **Component:** `knowledge-ui`
- **Paths:** `apps/desktop/src/knowledge/`
- **Depends on:** APP-003, INT-006, INT-007, INT-009, APP-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Give users visibility and control over reusable intelligence.

**Build**
- Show knowledge/memory provenance, scope, delete/supersede controls, active skill versions and task-scoped capability resolution.
- Separate semantic memory from recovery/checkpoint information in UI.

**Acceptance**
- Deleted memory/knowledge disappears from future retrieval.
- Unapproved skill/capability cannot be activated through UI.

**Required tests/evidence**
- delete propagation E2E
- skill activation policy test

### APP-012 — Build web administration and review surface
- **Owner:** Web
- **Language:** TypeScript
- **Component:** `web-admin`
- **Paths:** `apps/web/`
- **Depends on:** APP-001, APP-002, APP-006
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide browser-based review, administration and remote monitoring.

**Build**
- Implement responsive React web app for work review, approvals, policies, connectors, users, capabilities, audit and artifacts.
- Do not expose local privileged desktop APIs.
- Serve the web app as static assets from quansio-server or a CDN; no host-privileged APIs.

**Acceptance**
- Web uses same public contracts and authorization rules as desktop.
- Admin-only pages enforce server RBAC.

**Required tests/evidence**
- web RBAC E2E
- approval E2E
- responsive smoke

### APP-013 — Build CLI and SDK surface
- **Owner:** Developer Experience
- **Language:** Rust/Generated SDKs
- **Component:** `cli-sdk`
- **Paths:** `crates/cli/`, `sdk/`
- **Depends on:** APP-001, GOV-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Allow automation and operators to use the same canonical APIs.

**Build**
- Implement Rust CLI for auth, runs, agents, artifacts, approvals, routines and diagnostics.
- Publish generated TypeScript/Python clients from OpenAPI/Protobuf where appropriate.
- CLI supports login, workspaces, threads/messages, objectives/runs, approvals, artifacts, routines, targets, diagnostics bundle and `--json` output for automation.

**Acceptance**
- CLI never bypasses API/runtime policy.
- SDK compatibility test passes against current server.

**Required tests/evidence**
- CLI E2E
- SDK contract fixture
- auth token scope test

### APP-014 — Implement teammate definitions, persona and roster
- **Owner:** Product/Server
- **Language:** Rust/TypeScript
- **Component:** `teammates`
- **Paths:** `crates/server/control/teammates/`, `apps/desktop/src/teammates/`
- **Depends on:** APP-004, RUN-002, RUN-005
- **Real boundary:** not required
- **GA:** required
- **Goal:** Let users create and govern persistent teammates as first-class, capability-bounded participants.

**Build**
- Implement Teammate (agent definition) with persona, standing instructions, default capability grants (narrowing only), memory scope, model route preference, default execution target and availability.
- Provision one persistent AgentThread per teammate per workspace; route direct/group thread messages to it through the canonical turn loop.
- Build the roster UI: create/edit/archive teammates, show active runs, attention state and effective capability.
- Ship default teammate templates (general assistant, researcher, engineer) as configuration, not code.

**Acceptance**
- A teammate's standing instructions cannot widen its capability projection.
- Archiving a teammate suspends its AgentThread without losing thread/work/evidence lineage.
- Teammate configuration changes are audited and take effect on the next turn without restart.

**Required tests/evidence**
- teammate capability narrowing test
- archive/suspend lineage test
- roster E2E

### APP-015 — Implement public event stream and outbound webhooks
- **Owner:** Developer Experience
- **Language:** Rust
- **Component:** `webhooks`
- **Paths:** `crates/server/api/webhooks/`, `crates/server/notify/webhooks/`
- **Depends on:** APP-001, CORE-009, RUN-007, EXEC-008
- **Real boundary:** not required
- **GA:** required
- **Goal:** Give integrators the same canonical events clients receive, delivered safely.

**Build**
- Implement webhook subscriptions (DOMAIN.md §13.5) with event-type filters, HMAC signing via secret handle, retry/backoff, dead-letter and delivery status projection.
- Deliver through the egress broker as network.egress effects with redaction applied to payloads.
- Document the public event stream (WS/SSE) and webhook payload schema in OpenAPI; generate SDK helpers for signature verification.

**Acceptance**
- Delivery is at-least-once with stable event ids so consumers can dedupe; no event is delivered to a subscription outside its tenant/workspace.
- Webhook destinations obey egress policy and can be disabled by kill switch.
- Secrets and protected data classes never appear in webhook payloads.

**Required tests/evidence**
- delivery/retry test
- tenant scope test
- redaction scan
- egress deny test

## M6 — Research, artifacts, Business Capabilities and skill evolution

**Exit criteria:** Research, artifact, WikiSkill, built-in packs, Capability Compiler and controlled skill evolution work through existing primitives with passing evals.

### CAP-001 — Implement evidence-first research workflow
- **Owner:** Product/AI
- **Language:** Rust/Python
- **Component:** `research`
- **Paths:** `python/intelligence/research/`, `packs/skills/research/`
- **Depends on:** INT-005, INT-002, RUN-003, CORE-007, EXEC-009, EXEC-011, INT-012
- **Real boundary:** required
- **GA:** required
- **Goal:** Deliver deep research through normal WorkGraph/context/evidence primitives.

**Build**
- Compile research intent to typed SearchProgram and bounded WorkGraph branches.
- Collect source evidence, uncertainty and answer artifacts; model synthesizes only from authorized context.
- Use web.search and web.fetch/browser tools under egress policy; all fetched content enters context as UNTRUSTED_EXTERNAL.

**Acceptance**
- Research creates no second orchestrator or memory store.
- Final claims can resolve to evidence descriptors.

**Required tests/evidence**
- multi-source research E2E
- unsupported claim test
- cancel/recover test

### CAP-002 — Implement citations, EvidenceBundle and research verification
- **Owner:** Product/AI
- **Language:** Python/Rust
- **Component:** `research-evidence`
- **Paths:** `python/intelligence/research/evidence/`, `crates/server/runtime/verification/citations/`
- **Depends on:** CAP-001, CORE-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make research outputs traceable and verifiable.

**Build**
- Create EvidenceBundle with source identity, excerpt locator/digest, retrieval time and claim links.
- Run deterministic citation existence/coverage checks plus optional semantic support scoring.

**Acceptance**
- Missing/invalid evidence prevents verified research completion.
- Evidence bundle survives provider/model changes.

**Required tests/evidence**
- broken citation test
- claim coverage test
- bundle reload test

### CAP-003 — Implement artifact creation and editing workflows
- **Owner:** Product/AI
- **Language:** Python/TypeScript
- **Component:** `artifact-workflows`
- **Paths:** `python/intelligence/artifacts/`, `packs/skills/artifacts/`
- **Depends on:** APP-008, RUN-008, EXEC-006
- **Real boundary:** not required
- **GA:** required
- **Goal:** Create polished documents, spreadsheets, slides, media and code artifacts through governed tools.

**Build**
- Represent artifact-edit tasks as skills/tools with versioned source files and preview/evidence.
- Require format validation and task-specific quality checks before completion.

**Acceptance**
- Artifacts are editable/versioned and not opaque chat blobs.
- Failed format validation blocks completion.

**Required tests/evidence**
- document/spreadsheet/slide fixtures
- round-trip edit test
- malformed artifact test

### CAP-004 — Implement Capability Compiler ingestion pipeline
- **Owner:** Business Platform
- **Language:** Python
- **Component:** `capability-compiler`
- **Paths:** `python/intelligence/capability_compiler/`
- **Depends on:** INT-006, INT-009, EXEC-011
- **Real boundary:** not required
- **GA:** required
- **Goal:** Turn authoritative enterprise material into candidate Business Capability Packs without creating a second runtime.

**Build**
- Ingest documents, SOPs, policies, APIs, permissions, workflows and verified historical work.
- Decompose knowledge/process/tool/policy/evaluation requirements and generate candidate pack artifacts with provenance.

**Acceptance**
- Compiler output is non-executable until qualification/promotion.
- Raw credentials and private cross-tenant data are excluded from generated packs.

**Required tests/evidence**
- ingestion provenance test
- credential leak test
- tenant isolation test

### CAP-005 — Implement Business Capability Pack lifecycle and runtime resolution
- **Owner:** Business Platform
- **Language:** Rust/Python
- **Component:** `capability-packs`
- **Paths:** `crates/server/control/packs/`, `python/intelligence/capability_compiler/lifecycle/`
- **Depends on:** CAP-004, RUN-005, RUN-008
- **Real boundary:** not required
- **GA:** required
- **Goal:** Govern, evaluate, version and execute enterprise capabilities through existing Quansio primitives.

**Build**
- Define pack schema combining knowledge, skills, tools, connectors, policies, RBAC, approvals, workflow templates and evals.
- Qualify/promote versions, then resolve execution into WorkGraph + Skills + Tools + Knowledge + Policy.

**Acceptance**
- Pack cannot widen caller authority.
- Rollback to prior qualified version is deterministic.

**Required tests/evidence**
- pack qualification test
- authority narrowing test
- rollback test

### CAP-006 — Ship WikiSkill and knowledge-navigation baseline
- **Owner:** AI/Product
- **Language:** Python
- **Component:** `skills`
- **Paths:** `packs/skills/wiki/`
- **Depends on:** INT-009, INT-010, INT-004
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide a strong built-in skill for navigating large knowledge/repository spaces without adding a subsystem.

**Build**
- Create approved WikiSkill package using existing SearchProgram, index and Knowledge Fabric.
- Add evals for navigation, source selection, summarization, link/provenance preservation and context budget.

**Acceptance**
- Skill passes evaluation thresholds on pinned corpora.
- Disabling the skill leaves core search/context functional.

**Required tests/evidence**
- skill eval campaign
- budget regression test
- disable/fallback test

### CAP-007 — Ship initial built-in skill and capability packs
- **Owner:** Product
- **Language:** Python/Markdown
- **Component:** `skills`
- **Paths:** `packs/skills/`, `packs/capabilities/`
- **Depends on:** INT-009, CAP-003, CAP-006
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make the final product useful on day one across core work domains.

**Build**
- Package governed skills for research, coding/source control, artifact creation, data analysis, cloud/DevOps and business operations.
- Each pack declares tools, capability needs, evaluation cases, examples and recovery guidance.

**Acceptance**
- Every built-in pack has owner, version, provenance and passing eval.
- No pack installs daemons or bypasses runtime/tool policy.

**Required tests/evidence**
- pack schema/eval checks
- capability scan
- tool availability test

### CAP-008 — Implement controlled skill evolution
- **Owner:** AI/Product
- **Language:** Python
- **Component:** `skill-evolution`
- **Paths:** `python/intelligence/skills/evolution/`
- **Depends on:** INT-010, CAP-007, CORE-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Improve procedures from verified experience without self-modifying production behavior.

**Build**
- Detect repeated verified success/failure/recovery patterns from evidence.
- Generate candidate skill patches, evaluate against regression/safety/cost suites and require governed promotion.

**Acceptance**
- Production skill cannot change directly from a task run.
- Candidate provenance links back to evidence and evaluation.

**Required tests/evidence**
- silent mutation negative test
- candidate-to-promotion test
- regression rejection test

## M7 — Enterprise security, operations and deployment

**Exit criteria:** Enterprise identity, audit/retention/deletion, observability, usage/quotas, DR drill, deployment topologies, supply-chain checks and kill switches are proven.

### OPS-001 — Implement enterprise identity and service principals
- **Owner:** Security/Platform
- **Language:** Rust
- **Component:** `identity`
- **Paths:** `crates/server/control/identity/enterprise/`
- **Depends on:** APP-001, APP-002
- **Real boundary:** required
- **GA:** required
- **Goal:** Support organization-grade authentication and lifecycle.

**Build**
- Add OIDC/SAML-compatible SSO boundary, SCIM provisioning, service identities, device/workload identity and session revocation.
- Map external groups to internal RBAC without trusting client claims.

**Acceptance**
- Deprovisioned identity loses access promptly.
- Service identity scopes are explicit and auditable.

**Required tests/evidence**
- SSO sandbox test
- SCIM deprovision test
- token revocation test

### OPS-002 — Implement audit, privacy, retention and user data controls
- **Owner:** Security/Platform
- **Language:** Rust
- **Component:** `governance`
- **Paths:** `crates/server/audit/`, `crates/server/control/data_lifecycle/`
- **Depends on:** CORE-003, INT-006, CORE-007, INT-007, INT-004, INT-011
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide durable auditability and controllable data lifecycle.

**Build**
- Record admin/access/effect/policy events with actor and provenance.
- Implement retention, export and deletion workflows across authoritative and derived stores.
- Deletion propagates to Tantivy indexes, pgvector, memory and caches; AuditEntry hash chain per DOMAIN.md §16.

**Acceptance**
- Deletion propagates to indexes/memory/derived data subject to explicit legal retention.
- Audit entries cannot be edited in place.

**Required tests/evidence**
- export/delete E2E
- derived deletion test
- audit immutability test

### OPS-003 — Implement observability and redacted diagnostics
- **Owner:** Platform
- **Language:** Rust/Python/TypeScript
- **Component:** `observability`
- **Paths:** `crates/server/observability/`, `python/intelligence/observability/`, `apps/desktop/src/diagnostics/`
- **Depends on:** APP-001, INT-001, EXEC-002
- **Real boundary:** not required
- **GA:** required
- **Goal:** Make production behavior diagnosable without leaking sensitive data.

**Build**
- Instrument traces, metrics and structured logs with correlation IDs across runtime, intelligence and workers.
- Apply secret/PII redaction and bounded diagnostic bundle export.
- OpenTelemetry traces/metrics/logs (OTLP), Prometheus-compatible metrics, structured JSON logs with correlation_id; secret canaries injected in CI.

**Acceptance**
- A run can be traced end-to-end across service boundaries.
- Secret canary values do not appear in logs/traces.

**Required tests/evidence**
- trace propagation test
- secret canary scan
- diagnostic bundle test

### OPS-004 — Implement usage, budget, quota and entitlement projections
- **Owner:** Platform/Product
- **Language:** Rust
- **Component:** `usage`
- **Paths:** `crates/server/usage/`
- **Depends on:** RUN-010, CORE-009
- **Real boundary:** not required
- **GA:** required
- **Goal:** Measure resources and enforce plan/organization limits without contaminating runtime truth.

**Build**
- Project model tokens/cost, machine time, storage, connector calls and concurrency.
- Enforce hard/soft quotas through runtime/capability policy; keep billing as projection.
- UsageRecord meters per DOMAIN.md §13.4.

**Acceptance**
- Usage rebuild from source events is deterministic.
- Quota failure does not partially commit a new effect.

**Required tests/evidence**
- usage rebuild test
- quota race test
- budget alert test

### OPS-005 — Implement backup, restore and disaster-recovery consistency
- **Owner:** Platform
- **Language:** Infra/Rust
- **Component:** `dr`
- **Paths:** `infra/dr/`, `crates/server/control/recovery_point/`
- **Depends on:** CORE-001, CORE-007, RUN-009
- **Real boundary:** required
- **GA:** required
- **Goal:** Restore canonical state, artifacts and effects to a consistent point.

**Build**
- Configure PostgreSQL PITR, object versioning/replication and event/outbox recovery.
- Define RecoveryConsistencyPoint spanning DB/event position, artifact manifest, target snapshots and Effect Ledger settlement watermark.

**Acceptance**
- DR drill restores a usable tenant and proves no settled effect becomes pending or repeats.
- RPO/RTO are measured rather than asserted.

**Required tests/evidence**
- automated restore drill
- consistency point validation
- effect settlement audit

### OPS-006 — Implement supported deployment topologies
- **Owner:** Platform
- **Language:** Terraform/Helm/Containers
- **Component:** `deployment`
- **Paths:** `infra/terraform/`, `infra/helm/`, `infra/images/`
- **Depends on:** APP-001, INT-001, EXEC-005
- **Real boundary:** required
- **GA:** required
- **Goal:** Run the same product in managed cloud, private cloud/VPC and developer environments.

**Build**
- Build containers and infrastructure modules for server, intelligence, indexer, machine control and backing stores.
- Support endpoint/local execution without exposing provider/control secrets.

**Acceptance**
- Topology changes do not change canonical contracts or ownership.
- Private deployment passes health and smoke tests.

**Required tests/evidence**
- managed deployment smoke
- private topology smoke
- upgrade fixture

### OPS-007 — Implement supply-chain and dependency security
- **Owner:** Security/Build
- **Language:** Mixed
- **Component:** `supply-chain`
- **Paths:** `scripts/ci/supply_chain/`, `deny.toml`
- **Depends on:** GOV-005, GOV-003
- **Real boundary:** not required
- **GA:** required
- **Goal:** Protect release inputs and third-party dependencies.

**Build**
- Pin dependencies, generate SBOM, scan vulnerabilities/licenses, verify container/base-image provenance and protect update channels.
- Quarantine imported skills/tools before promotion.

**Acceptance**
- Known critical dependency fixture blocks release.
- SBOM matches built artifacts.

**Required tests/evidence**
- dependency scan
- SBOM verification
- untrusted skill quarantine test

### OPS-008 — Implement operational runbooks and kill switches
- **Owner:** Platform/Security
- **Language:** Markdown/Rust
- **Component:** `operations`
- **Paths:** `crates/server/control/ops/`, `docs/runbooks/`
- **Depends on:** OPS-003, OPS-005, RUN-006
- **Real boundary:** not required
- **GA:** required
- **Goal:** Provide safe operator actions for incidents and degraded dependencies.

**Build**
- Implement provider disable, connector revoke, worker quarantine, machine drain, effect freeze and policy emergency controls.
- Document concise runbooks in DOSSIER.md release section rather than separate manuals.
- Implement the kill switches listed in DOSSIER.md §21.5 as runtime.control effects with audit; runbooks generated into docs/runbooks/.

**Acceptance**
- Kill switches are audited and recoverable.
- Effect freeze blocks new consequential actions without corrupting reads or active evidence.

**Required tests/evidence**
- kill-switch E2E
- worker quarantine test
- provider outage drill

## M8 — Qualification and end-to-end proof

**Exit criteria:** Every qualification suite executed on real boundaries where required; no P0/P1; SLOs and eval thresholds met or explicitly waived.

### QA-001 — Close unit, schema and contract verification
- **Owner:** QA
- **Language:** Mixed
- **Component:** `verification`
- **Paths:** `tests/contract/`, `scripts/ci/`
- **Depends on:** GOV-005, APP-013
- **Real boundary:** not required
- **GA:** required
- **Goal:** Ensure every canonical module has executable local correctness coverage.

**Build**
- Set coverage expectations for safety-critical paths, validate schemas/bindings and run public/internal contract suites.
- Ban skipped/xfail release-blocking tests.

**Acceptance**
- All required suites pass from clean checkout.
- Generated contract drift is zero.

**Required tests/evidence**
- CI unit suite
- schema suite
- contract compatibility suite

### QA-002 — Prove architecture and authority consistency
- **Owner:** QA/Architecture
- **Language:** Python/Mixed
- **Component:** `architecture`
- **Paths:** `tests/architecture/`
- **Depends on:** GOV-008, QA-001
- **Real boundary:** not required
- **GA:** required
- **Goal:** Verify there is exactly one authority per concern in the final build.

**Build**
- Scan dependencies, database access, network paths and task/manifest registries.
- Detect shadow runtimes, direct provider calls, worker DB access and alternate effect paths.

**Acceptance**
- All known negative fixtures are caught.
- V8.1 validator reports zero consistency errors.

**Required tests/evidence**
- architecture negative corpus
- manifest/DAG validation
- forbidden wiring scan

### QA-003 — Qualify runtime concurrency, crash recovery and replay
- **Owner:** QA/Runtime
- **Language:** Rust
- **Component:** `runtime`
- **Paths:** `tests/recovery/`
- **Depends on:** RUN-009, RUN-010, CORE-008
- **Real boundary:** required
- **GA:** required
- **Goal:** Demonstrate durable autonomous work under failure.

**Build**
- Inject runtime crashes, worker loss, stream loss, timer restart, cancellation races and concurrent graph updates.
- Verify protocol replay, generation fencing and effect reconciliation.

**Acceptance**
- No duplicate settled effects or impossible states occur.
- Recovered run either safely continues or ends with typed unrecoverable reason.

**Required tests/evidence**
- fault-injection matrix
- long-run soak
- race/property tests

### QA-004 — Qualify model, context, search, memory and skills
- **Owner:** QA/AI
- **Language:** Python/Rust
- **Component:** `intelligence`
- **Paths:** `tests/evaluation/`
- **Depends on:** INT-010, CAP-006, CAP-008
- **Real boundary:** required
- **GA:** required
- **Goal:** Prove intelligence quality and safety against pinned evaluation sets.

**Build**
- Run route, retrieval, grounding, context budget, memory isolation, skill regression and cost/latency evaluations.
- Compare against frozen V8.1 thresholds, not self-selected examples.
- Gate on the ratified thresholds (DOSSIER.md §21.3 / OD-011).

**Acceptance**
- Protected quality and safety thresholds pass.
- Cross-tenant or deleted-memory retrieval is zero in adversarial set.

**Required tests/evidence**
- evaluation campaign
- retrieval benchmark
- memory isolation suite

### QA-005 — Qualify real execution, browser and computer boundaries
- **Owner:** QA/Execution
- **Language:** Rust/TypeScript
- **Component:** `execution`
- **Paths:** `tests/integration/execution/`
- **Depends on:** EXEC-003, EXEC-004, EXEC-005, EXEC-009, EXEC-010
- **Real boundary:** required
- **GA:** required
- **Goal:** Verify execution on actual supported environments rather than mocks.

**Build**
- Run local macOS, Windows and cloud target suites where available.
- Test browser DOM/CDP, screenshot fallback, downloads/uploads, app automation, takeover and checkpoint recovery.

**Acceptance**
- Unavailable required environment is BLOCKED_REAL_BOUNDARY, never PASS.
- Consequential GUI actions produce correct EffectRecords.

**Required tests/evidence**
- real-device matrix
- browser recovery suite
- takeover race suite

### QA-006 — Qualify connectors and external effects
- **Owner:** QA/Integrations
- **Language:** Mixed
- **Component:** `integrations`
- **Paths:** `tests/integration/connectors/`
- **Depends on:** EXEC-011, RUN-007
- **Real boundary:** required
- **GA:** required
- **Goal:** Prove external actions are idempotent, scoped and reconcilable.

**Build**
- Exercise representative read/write connectors in provider sandboxes.
- Inject timeout after remote success, webhook duplicates, token revoke and approval expiry.

**Acceptance**
- No blind retry duplicates external consequence.
- Revoked connector stops future calls and surfaces attention state.

**Required tests/evidence**
- sandbox connector suite
- unknown outcome test
- token revoke test

### QA-007 — Run security and privacy adversarial qualification
- **Owner:** Security QA
- **Language:** Mixed
- **Component:** `security`
- **Paths:** `tests/security/`
- **Depends on:** OPS-001, OPS-002, OPS-007, EXEC-007, EXEC-008, INT-012
- **Real boundary:** required
- **GA:** required
- **Goal:** Attack the trust boundaries before release.

**Build**
- Test tenant breakout, capability escalation, approval substitution, secret exfiltration, SSRF/DNS rebinding, path traversal, malicious skill/content, unsafe preview and stale lease reuse.
- Include the INT-012 injection corpus and the threat table in DOSSIER.md §16 as the minimum attack list.

**Acceptance**
- No P0/P1 security finding remains open.
- Security-critical ambiguity fails closed.

**Required tests/evidence**
- adversarial suite
- secret canaries
- tenant isolation campaign

### QA-008 — Qualify desktop/web UX and accessibility
- **Owner:** QA/Product
- **Language:** TypeScript
- **Component:** `ui`
- **Paths:** `tests/e2e/`
- **Depends on:** APP-003, APP-012, APP-011, APP-014
- **Real boundary:** required
- **GA:** required
- **Goal:** Prove complete human workflows, not isolated screens.

**Build**
- Automate onboarding, chat/objective, plan steering, approval, browser takeover, artifact review, collaboration, routines and settings.
- Run keyboard/accessibility and degraded/offline-state checks.
- Run on the support matrix in DOSSIER.md §21.1.

**Acceptance**
- Critical journeys pass on supported desktop OS versions and major browsers.
- Accessibility blockers are resolved.

**Required tests/evidence**
- Playwright E2E
- desktop smoke
- accessibility scan

### QA-009 — Qualify performance, scale and cost controls
- **Owner:** QA/Platform
- **Language:** Mixed
- **Component:** `performance`
- **Paths:** `tests/performance/`
- **Depends on:** OPS-003, OPS-004, RUN-010
- **Real boundary:** required
- **GA:** required
- **Goal:** Ensure the product remains responsive and bounded under realistic load.

**Build**
- Measure API latency, event lag, runtime scheduling, index/query latency, model stream startup, browser control, worker density and cost/usage accuracy.
- Run overload/backpressure tests.
- Measure against the ratified SLOs (DOSSIER.md §21.2 / OD-011).

**Acceptance**
- Published V8.1 SLO thresholds pass or are explicitly waived before candidate creation.
- Overload degrades with bounded queues rather than unbounded memory growth.

**Required tests/evidence**
- load test
- soak test
- backpressure test
- usage accuracy check

### QA-010 — Run final end-to-end product and migration journeys
- **Owner:** QA/Product
- **Language:** Mixed
- **Component:** `e2e`
- **Paths:** `tests/e2e/journeys/`, `tests/integration/migration/`
- **Depends on:** QA-003, QA-004, QA-005, QA-006, QA-007, QA-008, QA-009, GOV-006
- **Real boundary:** required
- **GA:** required
- **Goal:** Prove the final product as users will experience it and prove migration from legacy state.

**Build**
- Execute representative persistent teammate, research, coding/tool, artifact, routine, collaboration and Business Capability journeys from UI/API to real effects/evidence.
- Migrate supported legacy fixtures and verify no shadow authority remains.

**Acceptance**
- All GA journeys end with verified completion/evidence and can be resumed after injected interruption.
- Migration preserves supported user data or emits explicit incompatibility report.

**Required tests/evidence**
- GA journey suite
- migration suite
- fresh-install suite

## M9 — Packaging, canary and production release

**Exit criteria:** Reproducible candidate, signed desktop packages, deployed infrastructure, canary + rollback exercised, GA sealed.

### REL-001 — Create immutable release candidate
- **Owner:** Release
- **Language:** Mixed
- **Component:** `release`
- **Paths:** `scripts/release/`, `infra/images/`
- **Depends on:** QA-010, OPS-006, OPS-007
- **Real boundary:** not required
- **GA:** required
- **Goal:** Build one candidate from a known commit and exact dependencies.

**Build**
- Freeze version, migrations, contracts, container/image digests, desktop assets, skill/capability versions and evaluation dataset digests.
- Generate CI attestation and checksum manifest; no custom per-task signing ceremony is required.

**Acceptance**
- Candidate is reproducible from tagged commit and build config.
- All artifacts resolve to recorded digests.

**Required tests/evidence**
- rebuild comparison
- manifest verification
- SBOM check

### REL-002 — Package and notarize macOS desktop
- **Owner:** Release/Desktop
- **Language:** TypeScript/Rust/Swift
- **Component:** `macos-release`
- **Paths:** `apps/desktop/build/macos/`, `scripts/release/macos/`
- **Depends on:** REL-001, APP-003, EXEC-003
- **Real boundary:** required
- **GA:** required
- **Goal:** Produce installable trusted macOS distribution.

**Build**
- Build universal/supported architecture bundle, hardened runtime, entitlements, code signing, notarization and auto-update metadata.
- Signing credentials remain operator/CI secrets outside repository.

**Acceptance**
- Signed/notarized package installs and updates on clean supported macOS.
- Unsigned developer build remains available when release credentials are unavailable.

**Required tests/evidence**
- clean-machine install
- update/rollback test
- Gatekeeper validation

### REL-003 — Package and sign Windows desktop
- **Owner:** Release/Desktop
- **Language:** TypeScript/Rust
- **Component:** `windows-release`
- **Paths:** `apps/desktop/build/windows/`, `scripts/release/windows/`
- **Depends on:** REL-001, APP-003, EXEC-004
- **Real boundary:** required
- **GA:** required
- **Goal:** Produce installable trusted Windows distribution.

**Build**
- Build installer, native helper registration, signing and update metadata.
- Keep signing certificate outside repository/agent context.

**Acceptance**
- Signed package installs/uninstalls/upgrades on clean supported Windows.
- WSL/native broker prerequisites are detected with actionable UI.

**Required tests/evidence**
- clean Windows install
- upgrade/rollback
- prerequisite detection

### REL-004 — Deploy server and execution infrastructure
- **Owner:** Release/Platform
- **Language:** Containers/Terraform/Helm
- **Component:** `cloud-release`
- **Paths:** `infra/terraform/`, `infra/helm/`
- **Depends on:** REL-001, OPS-006, EXEC-005
- **Real boundary:** required
- **GA:** required
- **Goal:** Deploy production control, intelligence and execution planes from immutable artifacts.

**Build**
- Apply database migrations, deploy services with workload identity, secrets, autoscaling, backups and observability.
- Register execution images/templates by immutable digest.

**Acceptance**
- Production-like environment passes health, migration and smoke gates.
- Rollback path is tested before canary.

**Required tests/evidence**
- staging deployment
- migration rollback/forward-fix
- execution image verification

### REL-005 — Run canary and rollback qualification
- **Owner:** Release/Platform
- **Language:** Mixed
- **Component:** `canary`
- **Paths:** `scripts/release/canary/`
- **Depends on:** REL-002, REL-003, REL-004
- **Real boundary:** required
- **GA:** required
- **Goal:** Expose candidate gradually while preserving fast rollback.

**Build**
- Canary server/desktop cohorts, monitor error/effect/recovery/SLO signals, test rollback and forward compatibility.
- Do not rebuild candidate during qualification.

**Acceptance**
- Canary thresholds pass and rollback has been exercised successfully.
- No unresolved P0/P1 incident or data/effect inconsistency remains.

**Required tests/evidence**
- canary telemetry review
- rollback drill
- compatibility test

### REL-006 — Seal production readiness and release V8.1
- **Owner:** Release
- **Language:** Governance
- **Component:** `ga`
- **Paths:** `scripts/release/seal.py`, `evidence/REL-006/`
- **Depends on:** REL-005
- **Real boundary:** required
- **GA:** required
- **Goal:** Make the final go/no-go decision from executed evidence.

**Build**
- Verify every GA task is PASS or explicitly non-GA/deferred by approved scope decision.
- Verify manifest, QA results, known issues, rollback, support/runbook readiness and release artifacts.
- Tag and publish the exact qualified candidate.

**Acceptance**
- No open release-blocking task or failed mandatory suite.
- Published artifacts are byte-identical to qualified candidate.

**Required tests/evidence**
- final readiness validator
- artifact digest verification
- post-release smoke

