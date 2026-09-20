# APP-005 — Plan, WorkGraph and agent control experience

## What was built (task branch `task/APP-005-work-graph-experience`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
a 49-line local "revision" object with no server projection and no command.

### Structural fix first: the deployable can now compose the WorkGraph

The graph-backed ports RUN-003 (`PlanWorkspacePort`), RUN-001 (`WorkGraphPort`) and
CORE-008 (`RunDispatch`) existed only inside `crates/graph`'s tests, because
`quansio-graph` depended on `quansio-server` for `control::schema` and the workspace gate
forbids a cycle — so nothing in the running server could apply a plan to the canonical
graph. `control::schema`'s implementation now lives in `quansio_events::schema` (below
every store that sets a tenant context; `crate::control::schema` stays the canonical path
as a re-export), `quansio-server` depends on `quansio-graph`, and the three ports are
server library code (`runtime::planning::GraphPlanWorkspace`,
`runtime::orchestration::GraphWorkGraph`, `scheduler::GraphDispatch`) with their suites
moved to `crates/server/tests/graph_{planning,orchestration,scheduler_dispatch}.rs`.
Workspace, architecture and authority gates are clean.

### Server (`crates/server/src/api/workgraph.rs`)

- `GET /v1/workgraph?workspace_id=` — the plan as the canonical revision (`graph_heads`)
  has it: nodes with their completion contract, origin, thread and current run; edges;
  agents; handoff edges (`agent_graph_edges`); typed blockers (an unmet prerequisite, a
  parked run with its open question, a blocked/waiting status).
- `ProposeGraphEdit` — the user's edit compiled and validated by RUN-003's planner
  exactly like a model's PlanProposal (bounds, acyclicity, contracts, capability narrowing
  against the workspace policy's allow rules) and committed by CORE-005's revision-checked
  transaction: a stale `base_revision` is `CONFLICT_REVISION` (409) and writes nothing; a
  cycle is refused; a widening capability need is `CAPABILITY_DENIED`; `work.plan_applied`
  / `work.plan_rejected` are on the durable log.
- `CreateObjective` — the objective's `objective` thread plus its root node, through the
  same validated path.
- The conformance seam gains `[[scenario:delegate]]` (RUN-002's port spawns a worker on
  the run's node and parks the run `WAITING_CHILD`); teammate threads are activated on
  creation, which delegation requires of the parent.

### Desktop (`apps/desktop/src/work/`)

- `graph.ts` — projection decoder and reducer keyed by revision; `work.*`, `agent.*`,
  `run.*`, `question.*` events for the workspace mark the view stale; `matchesCanonical`.
- `edits.ts` — a draft of typed intents (add node / add dependency / transition / remove
  edge) becomes `ProposeGraphEdit` params against the observed revision; because the
  planner refuses edges naming nodes of the same proposal, such drafts go as two
  proposals (nodes, then edges with the new canonical ids). On `CONFLICT_REVISION` the
  draft is rebased onto the server revision: intents that still apply are kept (expected
  revisions re-pointed), the rest are dropped and reported — the user chooses; nothing is
  overwritten.
- Renderer `Work.tsx` — plan tree with dependencies, blockers and contracts, agents and
  handoffs (delegation edges with joined/in-flight), node detail with transitions and
  add-node/add-dependency forms, the draft list, the conflict/rebase dialog; the shell's
  `objectives` surface.

## Acceptance

1. **Graph view matches canonical revision** — `work.e2e.test.ts`: after CreateObjective
   and a two-phase plan edit, `matchesCanonical(view, GET /v1/workgraph)` holds and the
   view's revision equals the command's returned revision; a `work.*` event makes the page
   re-read the projection. Rust `workgraph_surface.rs` asserts the projection's revision,
   nodes, edges and blockers after every command.
2. **Concurrent server update produces conflict/rebase UX rather than silent overwrite** —
   two pages load the same revision; Alice's edit applies first; Bob's is refused with
   `CONFLICT_REVISION`, the projection shows none of his changes, `rebase()` re-points his
   draft at the new revision and it applies; Alice's page learns of it through events.
   Rust: the same at the API, plus nothing written on refusal.

## Tests (task list)

| test | where |
|---|---|
| graph projection E2E | `work.e2e.test.ts` (part 1); `workgraph_surface.rs::the_projection_is_the_canonical_revision…` |
| revision conflict test | `work.e2e.test.ts` (part 2); `work.test.ts` "rebases a conflicting draft…"; Rust 409 + nothing written |
| agent handoff UI test | `work.e2e.test.ts` (part 3: worker, handoff edge, parked run, blocker); `work.test.ts` decoder/handoffsFor; `workgraph_surface.rs::a_delegation_is_a_handoff_edge…` |

## Limits, stated

- Model-proposed plans (the engine's `plan_proposal` seam) are still refused by the
  runtime engine as RUN-003's integration point; this task wires the planner for user
  edits and objectives. The port and the transaction path are the same ones a later
  engine wiring will use.
- User edits carry no capability needs beyond the workspace policy's allow rules; the
  transition buttons offer `ready`/`cancelled`, and the server validates every transition.
