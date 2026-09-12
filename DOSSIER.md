# QUANSIO V8.1 — FINAL IMPLEMENTATION DOSSIER

**Version:** 8.1 (revision 2)  
**Date:** 2026-09-12  
**Status:** FINAL IMPLEMENTATION AUTHORITY — release requires executed qualification evidence  
**Purpose:** Build the complete Quansio product with one coherent architecture and the minimum documentation needed for autonomous implementation.

## 1. Authority model

Only these artifacts are implementation authority:

1. `DOSSIER.md` — product, architecture, ownership, language, technology and release rules.
2. `DOMAIN.md` — canonical domain model: names, identities, fields, states, transitions, effect classes, capability algebra, command catalog, error taxonomy.
3. `AGENTS.md` — binding implementation behavior for human and coding agents.
4. `registries/tasks.json` — canonical task definitions and dependencies.
5. `registries/progress.json` — mutable implementation status/evidence index.

`TASKS.md` and `registries/task-graph.json` are generated views. They MUST match `registries/tasks.json`. `MANIFEST.json` is generated integrity metadata over the authority set. `IMPLEMENTATION_MASTER_PROMPT.md` is the agent bootstrap and must not contradict the authority set. Superseded implementation documents may remain under `docs/archive/` but are not active authority; `docs/review/` holds historical review reports, also non-authority.

The governing invariant is:

> **The model proposes. The trusted runtime owns reality.**

No feature may introduce a parallel runtime, graph, policy engine, effect path, memory system, browser stack, scheduler or Business Capability execution engine.

Precedence when documents appear to conflict: `DOSSIER.md` (ownership/decisions) > `DOMAIN.md` (shapes) > `AGENTS.md` (behavior) > `registries/tasks.json` (scope). A real conflict is a `BLOCKED_CONFLICT` stop condition (§21), not a judgment call.

## 2. Final product

Quansio is a persistent governed AI work platform. A user can converse with a persistent teammate, create long-running objectives, delegate to scoped workers, research with evidence, operate browser/native applications, use files/terminal/connectors, create durable artifacts, collaborate with humans and agents, schedule recurring work, retain governed knowledge and skills, and execute enterprise Business Capabilities.

The final product surfaces are:

- Desktop application for primary work, browser/computer observation and takeover.
- Web application for remote work review, approvals, administration and enterprise governance.
- CLI and generated SDKs for automation and operators.
- Public API and event stream (WebSocket/SSE for clients, outbound webhooks for integrators).
- Local/private/cloud execution targets using one machine/worker contract.
- Persistent teammates and ephemeral workers using one AgentGraph/runtime.
- Research, coding, office/artifact, business and operational workflows implemented as tasks + skills + tools, not feature-specific runtimes.

### 2.1 V8.1 GA scope

In scope for GA (every task with `ga_required: true` in `registries/tasks.json`):

- Desktop (macOS, Windows), Web, CLI, TypeScript/Python SDKs.
- Cloud microVM execution, macOS local capsule, Windows local capsule (WSL2) and Windows native broker, customer private worker.
- Managed browser with takeover; native computer-use on macOS and Windows.
- Model providers: Anthropic (primary), OpenAI, and one OpenAI-compatible generic endpoint adapter (covers self-hosted/local inference). At least two must pass the live conformance suite.
- GA connectors: GitHub (source control, PR, CI status), Google Workspace (Gmail, Drive, Calendar), Slack, plus one web-search provider adapter. Additional connectors are post-GA.
- Built-in skill packs: research, coding/source control, artifact creation (document/spreadsheet/slides), data analysis, cloud/DevOps, business operations; WikiSkill baseline.
- Enterprise identity (SSO/SCIM), audit/retention/export/deletion, usage/quotas, backup/DR, supported deployment topologies.

Explicitly not required for GA (`DEFERRED_NON_GA` allowed only for tasks with `ga_required: false`, or features listed here):

- Native mobile (public API/event contracts MUST remain suitable for a later mobile companion without architectural changes).
- Linux desktop client.
- Fully embedded single-binary desktop server (see OD-001).
- Localization beyond English UI strings (string tables MUST be externalized from day one).
- Multi-region data residency.
- Marketplace distribution of third-party skills/packs.

## 3. Locked language boundary

### Rust — trusted authority and execution plane
Rust owns all code that can change authoritative runtime state, grant/deny authority, reserve/settle an external effect, control a machine/worker, or cross a privileged OS boundary.

Rust therefore owns:

- `quansio-server` authoritative API/control/runtime composition;
- WorkGraph, AgentGraph, StateGraph and GraphTransaction;
- Run/Turn/Step/Attempt/AgentThread state machines and the agent turn loop;
- Tool contract, Tool Registry and tool dispatch;
- Capability Projection, policy enforcement and approval verification;
- Universal Effect Ledger;
- protocol state, recovery, generation fencing, scheduler/timers;
- artifact/evidence metadata authority;
- `quansio-indexer` (exact/lexical/symbol/graph indexes);
- machine control, worker gateway and `qworkerd`;
- file/terminal/process tools;
- secret and egress brokers;
- browser/computer supervision and effect enforcement (CDP driver lives in Rust);
- CLI;
- security-critical audit and lifecycle control.

### Python — intelligence plane
Python owns fast-moving AI and evaluation logic behind generated typed RPC contracts:

- model provider adapters and normalized streaming;
- deterministic model-route implementation and DLP transforms;
- context ranking/packing and prompt rendering;
- embedding pipeline, semantic retrieval and knowledge processing;
- semantic memory candidate generation/retrieval;
- untrusted-content detection heuristics (non-LLM) and trust labelling support;
- skill/capability compilation and evaluation;
- research synthesis support;
- artifact-generation adapters (document/spreadsheet/slides/media);
- connector/provider adapters when ecosystem value justifies Python.

Python MUST NOT directly mutate canonical runtime/graph/effect/machine state or bypass Rust capability/policy/effect APIs. Python reads authoritative data only through Rust-served RPC/read APIs or explicitly assigned derived stores (vector index, evaluation datasets).

### TypeScript — product experience
TypeScript + React own Desktop/Web rendering and interaction. Electron main/preload provide a narrow typed bridge. Renderer code is never a runtime, effect, policy or machine authority. Model output, browser frames and terminal bytes reach the renderer only via the server client stream (§9), never directly from Python or from a worker.

### Native platform code
Use minimal Swift/macOS and Windows-native bridges only when required by platform APIs (Virtualization.framework, Accessibility/AX APIs, UI Automation, keychain/credential store, code signing hooks). Native bridges are called by Rust through narrow typed interfaces. They do not contain product orchestration.

### Go
Go is not a default V8.1 language. Do not add new Go code. Existing Go may remain temporarily only behind a documented migration boundary while V8.1 is built. Final canonical Rust-owned responsibilities MUST not depend on an ungoverned legacy Go authority.

## 4. Default deployment composition

V8.1 minimizes deployable units without collapsing ownership boundaries.

```text
Desktop / Web / CLI / SDK
           |  HTTPS + WebSocket (public API v1)
           v
+-----------------------------+
| quansio-server (Rust)       |
| API / Control / Runtime     |
| Policy / Effects / Scheduler|
| Tools / Artifact meta / Notify
+----+-------------+----------+
     |             | gRPC (tonic <-> Python)
     v             v
+----------+   +-------------------------+
| Postgres |   | quansio-intelligence    |
| + outbox |   | (Python)                |
| + pgvector   | Model / Context /       |
+-----+----+   | Knowledge / Skills /    |
      |        | Capability Compiler     |
      v        +-----------+-------------+
 NATS/events               |
                +----------+-----------+
                v                      v
      +----------------+      +------------------+
      | quansio-indexer|      | model providers  |
      | (Rust)         |      | / approved data  |
      +-------+--------+      +------------------+
              |
              v
+------------------------------------------------+
| quansio-machine (Rust)                         |
| Machine Control / Worker Gateway / Egress      |
+----------------------+-------------------------+
                       | mTLS outbound control channel
             +---------+---------+
             v                   v
       qworkerd (Rust)      connector adapters
       local / microVM      restricted boundary
       / private worker     (Python or Rust)
```

`quansio-server` is one default deployment, but its internal modules retain strict canonical ownership. Splitting modules later for scale requires an approved decision without changing ownership or contracts.

### 4.1 Client connectivity
- Desktop and Web are **clients** of a `quansio-server`. Desktop never embeds runtime authority. Desktop additionally hosts the local capsule substrate and native broker, which register with the server as execution targets exactly like cloud targets.
- Single-user/self-hosted use is served by the same server via the local development/self-host stack (`infra/compose`), not by a special desktop-only runtime (see OD-001).
- Clients hold only session tokens. Provider credentials, connector tokens and worker secrets never reach clients.

### 4.2 Rust ↔ Python RPC
- gRPC (Protobuf) using `tonic` (Rust) and `grpcio`/`grpclib` (Python); Unix domain socket when co-located, mTLS TCP when split.
- Rust is always the caller for authority-affecting flows; Python may call back only Rust read/derived-store APIs.
- Every RPC carries tenant/workspace scope, `correlation_id`, deadline and `capability_projection_id` where applicable.

## 5. Canonical ownership

| Concern | Sole canonical owner |
|---|---|
| Work definition/dependencies/acceptance | WorkGraph |
| Agent participation/delegation/lineage | AgentGraph |
| Runtime observations | StateGraph + RuntimeEvent |
| Atomic graph mutation | GraphTransaction |
| Run execution/recovery and the agent turn loop | `quansio-runtime` Rust module |
| Tool contract, registry and dispatch | `crates/tools` (registry) + runtime dispatch |
| Effective authority | Capability Projection + policy |
| Approval validity | Rust approval verifier |
| Consequential actions | Universal Effect Ledger |
| Timers/waits/routines | canonical runtime scheduler |
| Durable protocol state | runtime protocol store |
| Workspace/browser/terminal recovery | checkpoints + execution target state |
| Durable semantic knowledge | Knowledge Fabric |
| User/workspace semantic memory | Knowledge Fabric memory lifecycle |
| Context visible to a model | ContextProjection |
| Content trust labelling | ContextProjection (label) + runtime policy (enforcement) |
| Search/retrieval plan | typed SearchProgram |
| Model fulfillment | `quansio-intelligence` model gateway |
| Embeddings and vector index | intelligence embedding pipeline; index is derived (pgvector) |
| Skills | Skill Registry |
| Business Capabilities | Capability Registry; execution compiles to existing primitives |
| Machines/leases/generations | Machine Control |
| Worker execution | `qworkerd` under lease |
| Managed browser session | browser module in `qworkerd` (CDP), supervised by runtime effect enforcement |
| Native computer-use | native broker called by Rust machine module |
| Artifacts/evidence | artifact authority + object storage |
| Connector metadata | control module |
| Connector effects | integration adapter through Effect Ledger |
| Secrets | secret broker (Rust) with KeyProvider abstraction |
| Egress | egress broker (Rust) at host/network gateway |
| Usage/billing figures | usage projection (derived from RuntimeEvents) |
| Audit | audit module (append-only, hash-chained) |
| Product state display | client projections only |

## 6. Canonical request and effect path

Normal work follows:

```text
User/Event
 -> API authentication + tenant/workspace context
 -> canonical command (idempotent by command_id)
 -> durable transaction + RuntimeEvent/outbox
 -> WorkGraph/Runtime transition
 -> Turn loop (DOMAIN §5.6)
    -> ContextProjection (trust-labelled, budgeted)
    -> Model Gateway (when required)
    -> model proposal
    -> runtime validation (schema, bounds)
    -> Capability + Policy + Trust escalation + Approval
    -> Effect reservation
    -> Machine/Tool/Browser/Connector dispatch
    -> observation + evidence
    -> Effect settlement/reconciliation
 -> CompletionContract verification
 -> canonical state transition
 -> client projection (durable events + transient live frames)
```

No client, model, skill, connector or worker may mark work successful directly.

## 7. Model path

All model fulfillment goes through the trusted model gateway. Provider credentials never reach the renderer, normal worker or microVM guest.

A normal request MUST reach its selected primary model without a mandatory router/planner/judge LLM call. Selection may use deterministic rules, metadata, capability-demand scoring or non-generative prediction. This constraint governs **pre-call routing**; the turn loop naturally makes multiple primary-model calls per turn (one per tool round). Optional additional model calls (independent verification, semantic scoring) may happen after the primary call when a task explicitly requires them.

Model outputs are proposals. Tool calls, plans, memory writes, skill changes and completion claims require canonical validation.

Gateway requirements (INT-002/INT-003):

- Model identifiers live in `config/models.yaml` (catalog: provider, model id, capabilities, context window, cost class, DLP eligibility). No model id is hard-coded in source.
- Prompt rendering places stable content first (system, tool definitions, policy) and volatile content last so provider prompt caching is effective; the Context plane owns rendering.
- Operator/runtime instructions injected mid-conversation use the provider's operator channel where available, never a fake user message.
- Provider-side conversation compaction/context-editing features are disabled; Quansio owns compaction epochs (INT-008).
- Refusal, rate-limit and provider-error stop reasons are typed and surfaced; bounded failover preserves `call_id`.

## 8. Durable state and recovery

Memory is not recovery. Persist these layers separately:

1. Canonical event/session store — authoritative state transitions.
2. Protocol state — tool/model/approval/question/browser/terminal/subagent lifecycle needed for exact resume.
3. ContextProjection — bounded model-visible state for the current turn.
4. Compaction epochs — versioned compressed conversational history with stale-result rejection.
5. Workspace checkpoints — recoverable files/browser/terminal state.
6. Knowledge/Semantic Memory — learned durable knowledge with provenance.
7. Artifact/Evidence archive — immutable execution/result evidence.
8. Effect Ledger — external side-effect reservation and settlement truth.

Recovery reconstructs from 1, 2, 5, 7 and 8. It may use memory later for intelligence, never to decide what actually happened.

## 9. Persistence and eventing

- PostgreSQL 16+ is the authoritative server/control/runtime database. Every authoritative table carries `tenant_id`; Postgres row-level security is enabled as defense in depth with the tenant set per connection/transaction; application code additionally scopes every query.
- NATS JetStream is the initial event transport. State mutation and outbox write are atomic in PostgreSQL. Publisher dedup uses `Nats-Msg-Id = event_id`.
- S3-compatible object storage (MinIO in dev/self-host, S3/GCS/Azure in cloud) holds artifact/evidence/checkpoint bytes; metadata/digests remain canonical in PostgreSQL. Bucket layout is tenant-prefixed with server-side encryption.
- Vector index is pgvector in the same PostgreSQL by default (derived, rebuildable, separate schema `derived`). Lexical/symbol indexes are Tantivy files owned by `quansio-indexer`. Both are rebuildable from authoritative sources and object storage.
- SQLite is allowed only for device/worker-local cache, spool or checkpoint metadata. It is never an alternate cloud/control authority.
- Redis-compatible cache is optional and never authoritative.

Every mutation carries tenant/workspace scope, command identity and correlation/causation metadata.

## 10. Capability, policy, approvals and effects

Authority can only narrow as work is delegated or composed with skills/tools/capability packs. The capability algebra, effect classes, tiers, default policy and reconciliation strategies are normative in `DOMAIN.md` §6–§7.

Consequential actions MUST:

1. derive a semantic operation/effect class;
2. resolve Capability Projection;
3. pass policy/privacy/sequence checks and content-trust escalation;
4. obtain an exact ApprovalReceipt when policy requires;
5. reserve an EffectRecord;
6. execute through an authorized target;
7. settle or mark outcome unknown;
8. reconcile unknown outcomes before retry.

Low-level UI actions are not authorization escapes. A click that sends, buys, deletes, publishes, uploads protected information or changes external state has the same semantic effect class as the equivalent API action.

## 11. Execution targets

All execution environments implement the same `ExecutionTarget`, `Lease`, generation and worker protocol. Two target classes (persistent workspace computer, isolated task runtime) run on five substrates (cloud microVM, macOS local capsule, Windows local capsule, Windows native, customer private worker) — see `DOMAIN.md` §8.

Cloud execution uses isolated Linux microVMs (Firecracker by default), immutable templates, CoW/snapshot restore, host-side egress controls, private guest control channel and dynamic credential materialization.

Local capsules use Apple Virtualization.framework (macOS, Apple Silicon primary; Intel best-effort) and WSL2 (Windows). The desktop bundle ships or downloads a pinned, digest-verified Linux guest image containing `qworkerd`, a Chromium build and base tooling; image updates follow the desktop update channel.

The substrate is replaceable. It never becomes WorkGraph/runtime authority.

## 12. Browser and computer

Quansio uses one managed browser/computer stack for research, coding, business and teammate workflows. The managed browser is Chromium driven over CDP by a Rust driver inside `qworkerd`; it runs inside the execution target, never in the desktop process.

Browser understanding order:

1. DOM/CDP/network metadata;
2. accessibility tree;
3. screenshot/vision fallback.

The user sees and can take over the same live session (CDP screencast frames streamed as live frames through the server; user input relayed back to CDP). Takeover fences agent input until explicit handback. Browser cookies/saved logins may be imported only with explicit permission and scoped handling (`browser.session.import`, tier 4).

A headless fetch/extract mode of the same stack provides `web.fetch` for research; there is no separate HTTP-scraper runtime.

Native computer use resolves exact application identity and grants capability tiers. Unknown/ambiguous foreground applications fail closed for privileged operations.

## 13. Search, context, knowledge, memory and skills

`SearchProgram` is a typed bounded IR executed by the Context plane. It may fan out exact, lexical, semantic, graph, history and memory retrieval but does not become a second research runtime.

Every ContextProjection segment carries a trust level; instructions inside untrusted content are data, never intent (DOMAIN §12). Untrusted-derived proposals are escalated before authorization.

Knowledge entries have provenance, scope, version, confidence/lifecycle and deletion/supersession semantics.

Skills are versioned procedural assets. They can influence procedure and context but cannot:

- grant permissions;
- change runtime contracts;
- create hidden services/daemons;
- directly execute effects;
- silently promote their own updates.

Controlled skill evolution follows:

`verified evidence -> candidate patch -> evaluation -> governed promotion`.

## 14. Business Capability Packs

A Business Capability Pack combines approved knowledge, skills, tools, connectors, policy/RBAC requirements, approvals, workflow templates, examples, evaluation cases, version compatibility and evidence requirements.

The Capability Compiler is an authoring pipeline, not a runtime. It produces candidates. Approved packs execute by resolving into the existing WorkGraph + Skills + Tools + Knowledge + Policy + Effect + Evidence path.

## 15. Product experience requirements

The Desktop product MUST make these first-class:

- persistent chat and long-running objectives, with file attachments and agent questions answered in-thread;
- teammate roster: create/configure persistent teammates (persona, standing instructions, default capabilities, memory scope);
- visible plan/WorkGraph and agent roster;
- run status, waits, failures and recovery;
- approvals with exact consequence preview, including untrusted-origin warnings;
- evidence/effect timeline;
- live browser/computer view with takeover, and live terminal view;
- artifact/file workspace, versions and global search across threads/artifacts/knowledge;
- memory/knowledge/skills controls;
- collaboration, group chat, reactions and handoffs;
- routines, event triggers and notifications (in-app, desktop native, email);
- connector setup and health;
- Business Capability Pack administration where authorized;
- which model/context/tools were used for any turn.

The Web surface provides remote review/approval/admin without privileged local-host APIs. CLI/SDK use the same public contracts.

## 16. Security architecture

Threat model (QA-007 tests every item):

| Threat | Structural control |
|---|---|
| Model acts beyond intent | Model proposes only; runtime validates; capability narrowing; approvals bound to exact params |
| Prompt injection via web/email/docs/tool output | Trust labelling, data-only rendering, tier escalation, exfiltration guard, injection corpus (INT-012) |
| Credential exfiltration | Secret handles; materialization only at approved boundary; renderer/model/guest never see raw values; canary scans |
| Tenant breakout | tenant_id on every row + RLS; scoped tokens; per-tenant object prefixes; isolation suites |
| Stale/duplicate execution | Generations, leases, fence tokens, idempotency keys, effect reconciliation |
| Sandbox escape / host access | microVM/capsule isolation; deny-by-default egress; no host FS by default; native broker allowlist |
| Approval substitution/replay | Receipt bound to params digest, generation, expiry, single use, server signature |
| Malicious skill/pack/content | Skills cannot grant; quarantine on import; preview sandboxing; eval gates |
| Insider/admin abuse | Append-only hash-chained audit; kill switches audited; RBAC on admin pages |
| Supply chain | Pinned deps, SBOM, provenance attestation, signed releases |

Secrets at rest: envelope encryption with per-tenant data keys; KEK via `KeyProvider` (cloud KMS in managed/private cloud; local master key file with restricted permissions in dev/self-host). Desktop stores session tokens in the OS keychain/credential store. Logs and traces pass secret/PII redaction (OPS-003).

## 17. Repository layout

```text
/
  DOSSIER.md  DOMAIN.md  AGENTS.md  TASKS.md  IMPLEMENTATION_MASTER_PROMPT.md  MANIFEST.json
  registries/            tasks.json  task-graph.json  progress.json
  scripts/               validate_v81.py  dev/*  ci/*
  docs/archive/          superseded authorities (non-authority)
  docs/review/           review reports (non-authority)
  evidence/              per-task evidence bundles (see §19)
  config/                models.yaml, default policies, feature flags (non-secret)
  apps/
    desktop/             Electron main / preload / renderer (React, Vite)
    web/                 React web app
  crates/
    server/              quansio-server binary + modules:
      api/ control/ runtime/ policy/ effects/ scheduler/ artifacts/ notify/ audit/ usage/
    core/                IDs, generation, idempotency, errors (shared)
    graph/               WorkGraph/AgentGraph/StateGraph stores + GraphTransaction
    events/              RuntimeEvent store, outbox, projections
    capability/          Capability Projection
    tools/               Tool contract + Tool Registry (shared by server and qworkerd)
    indexer/             quansio-indexer
    machine/             machine control, worker gateway, egress broker, secret broker
    qworkerd/            worker daemon: tools host, browser (CDP), terminal, checkpoints
    cli/
    contracts/           generated Rust bindings (do not hand edit)
  python/
    intelligence/
      model_gateway/ context/ knowledge/ memory/ embeddings/ trust/ skills/
      capability_compiler/ evaluation/ artifacts/ adapters/ contracts/ (generated)
  packs/
    skills/              built-in skill packages
    capabilities/        built-in Business Capability Packs
  sdk/
    typescript/ python/  generated clients
  native/
    macos/               Swift bridge (Virtualization, AX, keychain)
    windows/             native broker (UIA, credential store)
  schemas/               Protobuf, OpenAPI, JSON Schema sources (single source of truth)
  migrations/            versioned SQL migrations
  infra/
    compose/             local/self-host stack
    terraform/ helm/ images/
  tests/
    contract/ integration/ e2e/ recovery/ security/ performance/ evaluation/
```

Implementation may adapt existing repository paths, but canonical ownership must remain equivalent, and each task's `paths` in `registries/tasks.json` names where its canonical owner lives.

## 18. Build, dependency and configuration policy

- Rust: stable toolchain pinned in `rust-toolchain.toml`, workspace-locked dependencies, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo deny`.
- Python: 3.12+, `uv` lockfile, `ruff`, `mypy --strict` for service/contract modules, `pytest`.
- TypeScript: strict mode, pnpm lockfile, `eslint`, `vitest` unit/component tests, Playwright E2E.
- Contracts: generated from `schemas/` in CI; no hand-copied canonical DTOs; regeneration diff must be empty.
- Database migrations: versioned, forward-safe, exercised in CI from zero and from the previous release.
- Configuration: typed config file (`config/*.yaml`) + environment overrides; secrets only via secret broker/env references, never in config files or repo. Feature flags live in `config/flags.yaml` with server-side evaluation; flags gate rollout, never authority.
- Versioning: product semver; public API `v1` additive-only; Protobuf packages versioned; desktop auto-update channels `stable`/`beta`.
- New service/store/effect path requires an approved entry in the Decision Log below.
- Do not rewrite verified code solely for language purity. Port only when it conflicts with the locked final ownership, security boundary or maintainability target.

## 19. Evidence and task completion

Per-task evidence is intentionally simple. `registries/progress.json` holds, per task:

```json
{
  "coverage": "GENUINE_GAP",
  "status": "PASS",
  "claimed_by": "agent:principal-1",
  "started_at": "2026-09-14T10:00:00Z",
  "updated_at": "2026-09-16T18:30:00Z",
  "git_commit": "<40-hex commit reachable from main>",
  "tests": ["cargo test -p quansio-effects", "ci:integration-effects"],
  "artifacts": ["evidence/RUN-007/2026-09-16T18-30-00Z/summary.json"],
  "real_boundary_evidence": [
    {"boundary": "anthropic-live", "environment": "ci-sandbox", "command": "pytest tests/integration/model_gateway -m live",
     "artifact": "evidence/INT-002/.../live-anthropic.log", "sha256": "<digest>", "recorded_at": "2026-09-16T18:20:00Z"}
  ],
  "implementation_complete": true,
  "blocker": null,
  "notes": "optional"
}
```

Evidence bundles live under `evidence/<TASK-ID>/<UTC timestamp>/` with a `summary.json` (`task_id, git_commit, commands[], results[], artifacts[] with sha256`) plus logs. Large binaries go to CI artifact storage and are referenced by URL + digest.

No custom per-task PKI or two-administrator signing ceremony is required. Release candidates MUST still use normal platform code signing/notarization where required and CI/build provenance/attestation for released artifacts.

Mocks/fakes are allowed for unit tests but never count as proof for a task that declares `real_boundary: true`. If a required real boundary is unavailable, use `BLOCKED_EXTERNAL`; never mark PASS.

## 20. Task execution

Canonical task definitions are in `registries/tasks.json`. Human-readable `TASKS.md` is generated from it.

Allowed coverage classification:

`ALREADY_COVERED | PARTIAL | GENUINE_GAP | SUPERSEDED | CONFLICT | IMPLEMENTED_UNDOCUMENTED`

Allowed progress status:

`NOT_STARTED | RECONCILING | IN_PROGRESS | BLOCKED_EXTERNAL | BLOCKED_CONFLICT | PASS | FAIL | DEFERRED_NON_GA`

Readiness rule: a task may enter `RECONCILING`/`IN_PROGRESS` only when every dependency is `PASS`, or is `BLOCKED_EXTERNAL` with `implementation_complete: true`. A task may reach `PASS` only when every dependency is `PASS`. `DEFERRED_NON_GA` is allowed only for tasks with `ga_required: false`, and a task cannot depend on a deferred task unless it is itself deferred. `scripts/validate_v81.py --ready` lists ready tasks; `--next` selects the next one deterministically.

A task is PASS only when its acceptance criteria and required tests/evidence are satisfied against its canonical owner.

## 21. Release rules

V8.1 GA requires:

- zero unresolved authority/architecture conflicts;
- all `ga_required` tasks PASS;
- real-boundary suites executed where required;
- no open P0/P1 security issue;
- runtime crash/recovery and unknown-effect reconciliation proven;
- protected intelligence evaluations pass;
- Desktop/Web critical journeys pass on the support matrix;
- supported migration paths pass;
- backup/restore and rollback are exercised;
- release artifacts are built from the qualified candidate commit and exact dependencies.

External packaging credentials may block signed/notarized distribution but MUST NOT block implementation of later independent tasks. Development builds continue while signing credentials are unavailable.

### 21.1 Support matrix (QA-008, REL-002/003)
| Surface | Supported |
|---|---|
| macOS desktop | macOS 14+; Apple Silicon (local capsule + computer-use); Intel: client + cloud targets, capsule best-effort |
| Windows desktop | Windows 11 22H2+ with WSL2 for local capsule; native broker on Windows 11 |
| Web | Latest two major versions of Chrome, Edge, Firefox, Safari |
| Server | Linux x86_64/arm64 containers; PostgreSQL 16+; NATS 2.10+ |
| Cloud execution | Firecracker-capable Linux hosts (KVM) |

### 21.2 Provisional SLOs (QA-009) — owner ratification required before REL-001
| Metric | Target (p95 unless noted) |
|---|---|
| API read | ≤ 200 ms |
| API mutating command accepted | ≤ 500 ms |
| Event delivery lag (commit → client) | ≤ 1 s |
| Model stream first token (over provider baseline) | ≤ +300 ms |
| Runtime scheduling latency (ready → dispatched) | ≤ 500 ms |
| Index query | ≤ 300 ms |
| Browser action round-trip (DOM path) | ≤ 1.5 s |
| Cloud microVM from warm pool | ≤ 3 s; cold ≤ 30 s |
| macOS local capsule warm start | ≤ 15 s |
| Memory/knowledge deletion visible in retrieval | ≤ 60 s |
| Server availability (managed) | 99.9 % monthly |
| DR | RPO ≤ 5 min, RTO ≤ 60 min |
| Overload | bounded queues, backpressure, no unbounded memory growth |

### 21.3 Provisional evaluation thresholds (INT-010, QA-004) — owner ratification required
| Metric | Threshold |
|---|---|
| Route determinism (same inputs → same route) | 100 % |
| Tool proposal schema validity | ≥ 98 % |
| Unsupported-claim rate on pinned research set | ≤ 2 % |
| Retrieval recall@10 on pinned corpora | ≥ 0.85 |
| Cross-tenant retrieval in adversarial set | 0 |
| Deleted-memory retrieval after refresh | 0 |
| Injection corpus: unauthorized effect executions | 0 |
| Injection corpus: escalation/detection rate | ≥ 95 % |
| Protected recovery/safety regressions | 0 (blocks promotion) |

### 21.4 Coverage expectations (QA-001)
Safety-critical crates (`capability`, `policy`, `effects`, `runtime`, `recovery`, `secrets`, `egress`, `qworkerd` protocol) ≥ 90 % line coverage with branch coverage reported; other Rust ≥ 75 %; Python service modules ≥ 80 %; TypeScript ≥ 70 % plus all critical journeys in Playwright. No skipped/xfail release-blocking tests.

### 21.5 Runbooks and kill switches (OPS-008)
Operator controls, each audited and reversible: provider disable, connector revoke, worker quarantine, target drain, effect freeze (blocks new tier ≥ 2 effects; reads and evidence continue), policy emergency deny, stream shed (disconnect slow consumers). Runbooks are concise sections in `docs/runbooks/` generated with OPS-008 and referenced from release notes; they are not authority.

## 22. Stop conditions

An implementation agent must stop the affected task and mark `BLOCKED_CONFLICT` rather than improvise when:

- canonical owner is ambiguous;
- a task appears to require a second persistent subsystem;
- a security/effect decision lacks capability/policy inputs;
- an external effect cannot be classified or reconciled;
- a contract change silently breaks supported clients/workers;
- a required migration would destroy user data without an approved plan;
- `DOSSIER.md`, `DOMAIN.md` and a task genuinely contradict each other.

The agent should continue with other dependency-ready tasks when the blocker is external or isolated.

## 23. Open decisions (defaults assumed until the owner overrides)

Implementation proceeds with the default. Overriding a default is a Decision Log entry.

| ID | Decision | Default assumed |
|---|---|---|
| OD-001 | Embedded single-binary desktop server for offline single-user use | Not in GA; desktop requires a reachable server; self-host via `infra/compose` |
| OD-002 | Baseline self-serve authentication | Email magic link + Google/Microsoft/GitHub OAuth; enterprise SSO via OPS-001 |
| OD-003 | GA connector set | GitHub, Google Workspace (Gmail/Drive/Calendar), Slack, one web-search provider |
| OD-004 | Model providers | Anthropic primary, OpenAI second, OpenAI-compatible generic third; catalog in `config/models.yaml` |
| OD-005 | Cloud microVM substrate | Firecracker on KVM hosts |
| OD-006 | Vector index | pgvector in PostgreSQL `derived` schema |
| OD-007 | Persistent Workspace Computer scoping | One per workspace shared by teammates; per-user computers are the same class with `owner=user` |
| OD-008 | Live browser view transport | CDP screencast frames over binary WebSocket live frames for GA; WebRTC post-GA |
| OD-009 | Email delivery for notifications | Server-side SMTP/API provider via secret handle; no client-side mail |
| OD-010 | Windows local capsule substrate | WSL2 required; Hyper-V direct is post-GA |
| OD-011 | SLO, eval-threshold and coverage numbers in §21 | Provisional; ratified by owner before REL-001 |
| OD-012 | Legacy code presence | If `GOV-001` finds no legacy code, greenfield rules in `AGENTS.md` apply and GOV-006 is closed as trivially satisfied |

## 24. Decision log

The following decisions are locked for V8.1:

- **D-001:** Rust owns trusted runtime/control/effect/machine authority.
- **D-002:** Python owns intelligence/evaluation behind typed RPC; it cannot directly execute privileged effects.
- **D-003:** TypeScript owns product surfaces; renderer state is never canonical.
- **D-004:** One canonical runtime/state/effect/search/browser/memory/skill architecture.
- **D-005:** PostgreSQL + transactional outbox is authoritative server persistence.
- **D-006:** Model fulfillment is gateway-mediated; provider credentials never reach normal clients/workers.
- **D-007:** Memory is not recovery.
- **D-008:** Browser is DOM/CDP-first with screenshot fallback and same-session human takeover.
- **D-009:** Business Capabilities and skill evolution compile/use existing primitives rather than new runtimes.
- **D-010:** Per-task evidence is commit/test/artifact bound; release signing uses standard platform mechanisms, not custom evidence PKI.
- **D-011:** Go is not a new-development default in V8.1; legacy Go is migration input only unless a later approved decision changes this.
- **D-012:** Documentation remains intentionally small; machine registries are generated/validated rather than duplicated by hand.
- **D-013:** `DOMAIN.md` is added to the authority set as the single canonical domain model; generated contracts derive from it. *Rationale:* tasks implemented independently drifted on entity names, states and fields without a shared model. *Migration:* none (pre-implementation). *Rollback:* fold into DOSSIER if it proves unnecessary.
- **D-014:** The agent turn loop, Tool contract and Tool Registry are Rust runtime authority (RUN-011); tool hosts (qworkerd, browser, adapters) execute only dispatched, effect-reserved calls. *Rationale:* the loop is the heart of the product and had no owner. *Rollback:* n/a.
- **D-015:** Content trust labelling and injection defense are a first-class subsystem of ContextProjection + policy (INT-012), not a prompt-only mitigation. *Rationale:* the platform reads untrusted web/email/document content by design. *Rollback:* n/a.
- **D-016:** Rust↔Python RPC is gRPC/Protobuf via tonic and grpcio/grpclib; the managed browser is Chromium over CDP driven from Rust inside the execution target; the default cloud substrate is Firecracker; the default vector index is pgvector. *Rationale:* remove technology ambiguity blocking autonomous implementation. *Rollback:* each is behind a trait/adapter and replaceable by decision.
- **D-017:** Dependency readiness allows starting on tasks whose dependencies are `BLOCKED_EXTERNAL` with `implementation_complete: true`, but never allows `PASS` until dependencies are `PASS`. *Rationale:* unavailable real boundaries (signing, Windows hosts) must not halt independent implementation. *Rollback:* remove the exception in the validator.
- **D-018:** Model identifiers, default policies and feature flags are configuration under `config/`, never source constants. *Rationale:* model catalogs change faster than releases. *Rollback:* n/a.

Any architecture change adds one new `D-###` entry with rationale, migration and rollback implications, and updates affected tasks/contracts in the same commit.

## 25. Revision history

- **r1 (2026-09-12):** Initial V8.1 authority set (94 tasks).
- **r2 (2026-09-12):** Gap-analysis revision. Added `DOMAIN.md`; §2.1 GA scope; §4.1–4.2 client connectivity and RPC; §16 security architecture; §18 configuration/versioning; §19 evidence schema; §20 readiness rule; §21.1–21.5 support matrix, SLOs, thresholds, coverage, runbooks; §23 open decisions; D-013–D-018. Tasks: added RUN-011, INT-011, INT-012, APP-014, APP-015; added `ga_required`/`paths` fields and milestone exit criteria; corrected dependency ordering (INT-002 no longer waits on RUN-010; APP-001 is the walking-skeleton gate; CAP-001 depends on browser/search tools). Validator: manifest scoped to authority files; readiness, evidence-schema and status-consistency checks; `--ready`/`--next`. Review report: `docs/review/2026-09-12-r2-gap-analysis.md`.
