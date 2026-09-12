# Quansio V8.1 dossier — gap analysis and revision 2 changes

**Date:** 2026-09-12
**Scope reviewed:** DOSSIER.md, AGENTS.md, IMPLEMENTATION_MASTER_PROMPT.md, registries/tasks.json (94 tasks), registries/progress.json, scripts/validate_v81.py, generated views.
**Status of this document:** historical review report. Not implementation authority.

## 1. Summary

The r1 dossier is architecturally strong: the ownership model, invariants, language boundary and release discipline are clear and internally consistent, and the validator/registry mechanism is a sound way to keep an autonomous agent honest. Its weakness is that it stops at *who owns what* and never defines *what the things are*. An implementation agent working task-by-task would have had to invent the domain model (~40 named concepts with no fields, states or transitions), the technology choices, the API surface, the effect taxonomy, and the development workflow — independently, per task, with no mechanism to keep those inventions consistent. Several product-level commitments (research, public event stream, teammates, attachments, prompt-injection defense) also had no task that built them.

Revision 2 closes those gaps without changing any r1 decision. 94 → 99 tasks; one new authority file; ~70 build/acceptance items added; 15 dependency corrections; validator hardened.

## 2. Gaps found

### 2.1 Design gaps (named but unspecified)
| # | Gap | Impact | Resolution |
|---|---|---|---|
| G1 | No domain model: WorkGraph, Run, Step, Attempt, AgentThread, EffectRecord, ApprovalReceipt, CapabilityProjection, ContextProjection, SearchProgram, ExecutionTarget, Lease, Skill, Pack, etc. had no fields, identities or relationships | Every task would define its own shapes; contracts drift; RUN-001's "state transition matrix" test had nothing to test | **DOMAIN.md** (new, authority) §0–§17 |
| G2 | No state machines for Run/Step/Attempt/AgentThread/Effect/Approval/Lease/Target/Skill/Knowledge/Pack/Routine | "Illegal transitions fail closed" untestable | DOMAIN §5, §7, §8, §11 |
| G3 | No effect-class taxonomy, tiers, default approval policy or reconciliation strategy per class | RUN-007 "classify semantic effects" unimplementable consistently; approval UX undefined | DOMAIN §7.1 (23 classes, 5 tiers) |
| G4 | No capability algebra (what a grant is, how layers compose, what "narrowing" means operationally) | RUN-005 property tests undefined | DOMAIN §6 |
| G5 | No agent turn loop owner and no Tool contract/registry — the core agentic loop (model → proposal → validate → dispatch → observe → repeat) was implied across RUN-001/003/008 but never specified | The heart of the product had no task | DOMAIN §5.6, §7.4–7.5; **RUN-011** (new); D-014 |
| G6 | No API surface: "REST/OpenAPI and WebSocket endpoints" with no command list | APP-001 unbounded | DOMAIN §14 command catalog |
| G7 | No RuntimeEvent taxonomy or client stream protocol (durable vs transient frames) | CORE-003/009 shapes undefined; risk of Python→renderer streaming shortcut | DOMAIN §9; DOSSIER §3 (TS) explicit |
| G8 | No ID scheme, tenancy hierarchy, roles or baseline auth | CORE-001/002, APP-002 undefined | DOMAIN §1–§2; OD-002 |
| G9 | Technology ambiguities: Rust↔Python RPC mechanism, browser engine/driver, live-view transport, cloud microVM substrate, vector store, local capsule guest image | Agent would pick per task | DOSSIER §4.2, §9, §11, §12; D-016; OD-005/006/008 |
| G10 | Desktop connectivity model undefined (does desktop embed a server? does it need Postgres?) | Fundamental deployment question | DOSSIER §4.1; OD-001 |
| G11 | No prompt-injection / content-trust design, despite the product reading web pages, emails and documents by design | Highest-risk security gap; QA-007 tested nothing specific | DOMAIN §12; **INT-012** (new); D-015; DOSSIER §16 |
| G12 | No error taxonomy despite "typed errors" | APP-001/APP-013 SDKs inconsistent | DOMAIN §15 |
| G13 | No embedding pipeline / vector index task despite "semantic retrieval" | INT-005/006 depended on something nobody built | **INT-011** (new) |
| G14 | No threat model or secrets-at-rest / tenant-isolation mechanism | QA-007 attack list unbounded | DOSSIER §16 |
| G15 | Section 11 conflated target classes with substrates | EXEC-001 contract ambiguous | DOMAIN §8.1 (2 classes × 5 substrates) |

### 2.2 Missing product scope (promised in §2/§15, built by no task)
| # | Gap | Resolution |
|---|---|---|
| P1 | Persistent teammate definition, persona, standing instructions, roster | **APP-014** (new) |
| P2 | Public event stream / outbound webhooks for integrators | **APP-015** (new) |
| P3 | Research needs web search/fetch; no task provided them | EXEC-009 (headless fetch mode → `web.fetch`), EXEC-011 (search provider → `web.search`); CAP-001 deps corrected |
| P4 | File attachments in chat | APP-004, CORE-007 |
| P5 | Agent-asks-user question protocol | RUN-011, APP-004 |
| P6 | Live terminal view | APP-007 |
| P7 | Global search | APP-008 |
| P8 | Per-turn "which model/context/tools" inspector | RUN-011, APP-004 |
| P9 | Notification channels, email delivery | APP-010; OD-009 |
| P10 | Feature flags, API rate limits, config management | DOSSIER §18; APP-001; D-018 |
| P11 | GA scope vs. explicitly non-GA list | DOSSIER §2.1 |

### 2.3 Untestable qualification criteria
| # | Gap | Resolution |
|---|---|---|
| Q1 | QA-009 referenced "published SLO thresholds" — none existed | DOSSIER §21.2 provisional SLOs; OD-011 |
| Q2 | QA-004/INT-010 referenced "frozen thresholds" — none existed | DOSSIER §21.3 |
| Q3 | QA-001 "coverage expectations" — no numbers | DOSSIER §21.4 |
| Q4 | QA-008/REL-002/003 "supported OS versions and browsers" — undefined | DOSSIER §21.1 |
| Q5 | "release-blocking tasks" — no flag on tasks | `ga_required` field; validator enforces DEFERRED_NON_GA only when false |
| Q6 | Milestones had no exit criteria | `exit_criteria` on every milestone |
| Q7 | `real_boundary_evidence` had no schema | DOSSIER §19; validator enforces |

### 2.4 Process gaps for an autonomous agent
| # | Gap | Resolution |
|---|---|---|
| W1 | Not a git repository; no branch/commit/merge protocol despite "reachable git commit" | AGENTS.md git protocol; bootstrap step |
| W2 | Greenfield vs. brownfield ambiguity ("legacy Go", "supported legacy state", "archived dossiers") with no legacy code present | OD-012; greenfield rule in AGENTS.md; GOV-001/002/006 build items |
| W3 | No task-selection algorithm or claiming protocol for parallel agents | `--ready` / `--next`; `claimed_by`; CLAIM step |
| W4 | Real-boundary credentials: no convention for where they come from or how absence is handled | `QUANSIO_TEST_*` convention; automatic BLOCKED_EXTERNAL |
| W5 | BLOCKED_EXTERNAL on one real boundary (e.g., Windows host) would freeze all downstream implementation | D-017 readiness rule + `implementation_complete` |
| W6 | No regression rule for a PASS task later found broken | AGENTS.md "Regressions" |
| W7 | No evidence directory convention | `evidence/<TASK>/<ts>/summary.json` |
| W8 | No canonical path per task — "canonical owner ambiguous" is a stop condition but tasks didn't say where the owner lives | `paths` field on every task |
| W9 | Waterfall risk: no UI or end-to-end proof until M5 | APP-001 acceptance now includes a walking-skeleton smoke test with the conformance-stub provider |
| W10 | Model IDs and policies would be hard-coded | D-018; `config/` |

### 2.5 Validator defects
| # | Defect | Fix |
|---|---|---|
| V1 | `manifest_files()` walked the whole repository — MANIFEST.json would include every source file, `target/`, `node_modules/`, `.git/` once code exists; CI would fail on every commit | Manifest scoped to the authority set only |
| V2 | A task could be PASS while its dependencies were NOT_STARTED | Readiness + PASS-requires-deps-PASS checks |
| V3 | No schema for progress fields; BLOCKED without blocker, IN_PROGRESS without claim, PASS without artifacts all accepted | Full progress-field validation |
| V4 | No check that DOMAIN.md exists or that AGENTS/master prompt reference it | Added |
| V5 | Decision log could have gaps/duplicates | Sequential D-### check |
| V6 | No help for choosing work | `--ready`, `--next` |

All negative cases were exercised on a scratch copy (PASS-without-deps, BLOCKED-without-blocker, cycle, hand-edited TASKS.md, decision-log gap, DEFERRED on GA task) and each fails validation; the clean set passes.

## 3. Dependency corrections
| Task | Change | Reason |
|---|---|---|
| INT-002 | `RUN-010` → removed; deps now `INT-001, GOV-004` | Gateway only needs the usage *contract*; waiting on the full runtime pushed all model work behind M2 |
| APP-001 | + `RUN-011, INT-002` | Server composition is the walking-skeleton gate |
| APP-004 | + `RUN-011` | Chat runs on the turn loop |
| EXEC-006/009/011 | + `RUN-011` | Tool hosts execute registry-dispatched calls |
| RUN-008 | + `RUN-007` | Completion binds to effect settlement |
| INT-006 | + `INT-011` | Knowledge retrieval needs embeddings |
| CAP-001 | + `EXEC-009, EXEC-011, INT-012` | Research needs web tools and trust labelling |
| OPS-002 | + `INT-007, INT-004, INT-011` | Deletion must propagate to memory, lexical and vector indexes |
| QA-007 | + `INT-012` | Injection corpus |
| QA-008 | + `APP-014` | Teammate journeys |
| APP-007 | + `EXEC-006` | Terminal view |
| APP-011 | + `APP-004` | UI shell prerequisite |

Critical path after revision: 26 tasks (GOV-001 → … → RUN-011 → APP-001 → APP-003 → APP-008 → CAP-003 → CAP-007 → CAP-008 → QA-004 → QA-010 → REL-001 → REL-002 → REL-005 → REL-006).

## 4. Decisions made vs. decisions left to the owner

Made (recorded as D-013–D-018, all consistent with r1 invariants): DOMAIN.md as authority; turn loop/tool registry in Rust; trust labelling as a subsystem; gRPC/tonic, Chromium/CDP-in-Rust, Firecracker, pgvector as defaults; readiness exception; config-not-code for model ids/policies/flags.

Left to the owner with a stated default (DOSSIER §23, OD-001–OD-012): embedded desktop server, baseline auth, GA connector set, provider set, substrate defaults, workspace-computer scoping, live-view transport, email delivery, Windows substrate, SLO/threshold/coverage numbers, greenfield handling. Implementation proceeds on the defaults; overriding one is a Decision Log entry.

`ga_required` is `true` on all 99 tasks. If the owner wants to descope (candidates: EXEC-004/REL-003 Windows, EXEC-010 computer-use, OPS-001 SSO/SCIM for a first GA), flip the flag in `registries/tasks.json` and run `--write`; the validator then permits `DEFERRED_NON_GA` on those and their dependents.

## 5. What was deliberately not done
- No code, scaffolding, git initialization or CI files were created — the request was documentation only.
- Task count was kept near r1 (99) rather than exploding into fine-grained tickets; tasks remain coarse and agents split them into branch-local checklists (AGENTS.md PLAN step).
- No task was removed or narrowed.
