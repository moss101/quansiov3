# Review: M5 "implementation complete" claims are placeholders (2026-09-18)

**Scope.** While flipping the tasks that INT-002's PASS had unblocked (INT-003, INT-011,
APP-001 — all sound), the next nine dependency-ready tasks were examined the same way: every
recorded suite re-run on `main` (803f2cd), then the code under each task's `paths` compared
with its build items and acceptance. Two hold; seven do not, and the pattern extends to the
rest of M5.

**Finding.** Fourteen M5 tasks and OPS-003 were recorded `BLOCKED_EXTERNAL` with
`implementation_complete: true` on 2026-09-15, blocked "only" on INT-002. Their recorded
tests pass — but the tests assert placeholder functions written to satisfy them, and the
build items are not implemented. Lines of code under each task's paths (`.rs/.py/.ts/.swift/
.sql/.yaml`, crate `src/` layout resolved):

| Task | Title | Code under `paths` | What is there |
|---|---|---|---|
| APP-002 | identity, onboarding, workspaces, settings | 465 rs + 35 ts | real `IdentityStore` (onboarding, layered settings) — but OAuth/magic-link "recorded as names only", no JWT/refresh/keychain, no client flow, and nothing outside the module calls it |
| APP-003 | Electron desktop shell and preload bridge | `apps/desktop` 1349 (all APP-* stubs + tests) | `devDependencies: {}` — no Electron; `main` exports a webPreferences/CSP object that the tests assert |
| APP-004 | chat, objective and thread experience | 103 ts; `api/threads.rs` 7 lines | |
| APP-005 | plan, WorkGraph and agent control | 49 ts | |
| APP-006 | approvals, effects, timeline, evidence UI | 47 ts | |
| APP-007 | live browser/computer surface | 36 ts | |
| APP-008 | artifact, file and document workspace | 88 ts | |
| APP-009 | collaboration, group chat, reactions, handoffs | 31 rs + 12 ts | `order_messages` (a sort), `can_receive(status == "active")` |
| APP-010 | routines, schedules, events, notifications | 20 rs + 87 rs (notify) + 13 ts | `fire_key` (string format), `should_fire` (`!seen.contains`), a constant |
| APP-011 | memory, knowledge, skills, capability settings UI | 52 ts | |
| APP-012 | web administration and review surface | `apps/web` 125 | |
| APP-013 | CLI and SDK surface | `crates/cli` 78 (no deps, no binary) + generated SDK types | verb → path string map; `sdk/python/` is a `.gitkeep`; the "compatibility test against current server" reads the OpenAPI file |
| APP-014 | teammate definitions, persona, roster | 55 rs + 18 ts | |
| APP-015 | public event stream and outbound webhooks | 87 rs (notify) ; `api/webhooks.rs` 5 lines | `may_deliver` predicate, `already_delivered` (`contains`), key-name `redact`; no subscriptions, HMAC, retry, dead-letter, delivery projection |
| OPS-003 | observability and redacted diagnostics | 384 rs + 249 py + 57 ts | real correlation/redaction/Prometheus-text/bundle code with tests in three planes — referenced by nothing outside its own module in any plane; no OTLP |

Wiring checks (all empty): `grep -rl "observability::" crates/server/src` outside the module;
`grep -rl intelligence.observability python/intelligence` outside the package;
`grep -rl "identity::\|IdentityStore" crates/server/src` outside the module.

**Why it matters.** DOSSIER.md §20 lets a task start only when its dependencies are PASS or
`BLOCKED_EXTERNAL` + `implementation_complete: true`. Eleven further tasks were started on the
strength of these flags — CAP-003, CAP-007, CAP-008, OPS-001, OPS-008, QA-001, QA-002, QA-004,
QA-007, QA-008, QA-009 — so an honest correction reaches 26 registry entries, and the product
surface that M5 names (desktop, web, CLI) does not exist yet. AGENTS.md invariant 10 ("no model
self-certification of completion") and the Definition of PASS both forbid recording these as
PASS; this review records the finding so the decision on the registry is the owner's.

**Sound in the same batch.** INT-006 (Knowledge Fabric, 1395 lines, 32 tests against real
PostgreSQL/pgvector) and INT-010 (evaluation harness, 2159 + 1793 lines, 61 tests) hold and
were recorded PASS at 803f2cd.

**Recommended correction.** For the 15 placeholders: `status: NOT_STARTED`, `coverage: PARTIAL`,
`implementation_complete: false`, claim and commit cleared, notes naming the placeholder
commit and the gap. For the 11 dependents: back to `NOT_STARTED` as well, keeping their notes
(their own implementation state is unchanged; only their readiness was). `--ready` then lists
the M5 tasks in dependency order and the loop resumes with real work: APP-002/APP-003 first.
