# APP-005 reconciliation (2026-09-20, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `apps/desktop/src/work/monitor.ts` (49 lines of a
local "revision" object) with no server projection and no command
(docs/review/2026-09-18-m5-placeholder-audit.md).

- **Canonical owners:** `apps/desktop/src/work/` (the surface, the task's `paths`) over
  CORE-005's WorkGraph (`crates/graph`: `GraphStore`, `GraphTransaction::apply_plan_with`)
  and RUN-003's planner (`crates/server/src/runtime/planning`: compile/validate a
  proposal, commit through `PlanWorkspacePort`). The HTTP projection and the two §14 work
  commands live in `crates/server/src/api/workgraph.rs`. No second store: the graph view
  is `GET /v1/workgraph`, a projection of `work_nodes`, `work_edges`, `graph_heads`,
  `agent_threads`, `agent_graph_edges` and `runs`; every edit is a command.
- **Existing code, coverage PARTIAL, kept:** the planner and its typed rejections
  (`CONFLICT_REVISION` for a stale `base_revision`, cycle, bounds, missing contract,
  capability widening), the graph store and transaction, RUN-002's delegation port
  (`AgentDelegationPort`: child worker thread + `agent_graph_edges` row, run parks
  `WAITING_CHILD`).
- **Gap found and fixed first (structural):** the graph-backed `PlanWorkspacePort`,
  `WorkGraphPort` and `RunDispatch` existed only inside `crates/graph`'s tests because
  `quansio-graph` depended on `quansio-server` for `control::schema`, so the deployable
  could not compose the WorkGraph at all — `ProposeGraphEdit` had nowhere to go.
  `control::schema`'s implementation now lives in `quansio_events::schema` (below every
  store that sets a tenant context; `crate::control::schema` remains the canonical path as
  a re-export), `quansio-server` depends on `quansio-graph`, and the three ports are server
  library code (`runtime::planning::GraphPlanWorkspace`,
  `runtime::orchestration::GraphWorkGraph`, `scheduler::GraphDispatch`) with their suites in
  `crates/server/tests/graph_*.rs`. Workspace, architecture and authority gates stay clean.
- **Persistent state touched:** none new.
- **Contracts (DOMAIN.md §14, catalogued):** `GET /v1/workgraph?workspace_id=`; commands
  `CreateObjective` (root objective node + its `objective` thread, through the planner) and
  `ProposeGraphEdit` (`{workspace_id, base_revision, nodes_add[], nodes_update[],
  edges_add[], edges_remove[], rationale}`, validated like a PlanProposal with the user as
  proposer; a stale `base_revision` is `CONFLICT_REVISION` and writes nothing).
- **External effects:** none. **Capabilities:** user edits carry `capability_needs: []`
  unless they narrow the workspace policy's allow rules (the proposer projection);
  widening is `CAPABILITY_DENIED`. **Migrations:** none. **Real boundaries:** none.
- **Handoffs:** the conformance seam gains `[[scenario:delegate]]` (RUN-002's port spawns a
  worker on the run's node; the run parks `WAITING_CHILD`) so the agent/handoff view is
  exercisable without a live model; the teammate thread is activated when created, which
  the delegation port requires.

## Plan

1. Server: `api/workgraph.rs` — projection (revision, nodes with contracts and their
   current run, edges, agents, handoffs, blockers with typed reasons), `CreateObjective`,
   `ProposeGraphEdit`; the planner composed over `GraphPlanWorkspace`.
2. Desktop `work/`: `graph.ts` (projection decoder + reducer keyed by revision; `work.*`
   and `agent.*` events mark the view stale), `edits.ts` (an edit draft of typed intents
   → a `ProposeGraphEdit` against the observed revision; on `CONFLICT_REVISION` the draft
   is rebased onto the server revision — intents that still apply are kept, the rest are
   reported — never silently overwritten), renderer `Work.tsx` (plan tree with
   dependencies, agents and handoffs, blockers, completion contracts, edit form,
   conflict/rebase UI), bridge channels `work.graph`, `work.propose`, `work.objective`.
3. Tests: graph projection E2E (real server: objective → edits → projection equals the
   canonical revision, events refresh), revision conflict test (two pages edit the same
   revision: the second gets `CONFLICT_REVISION`, rebases, succeeds; Rust integration for
   the same at the API), agent handoff UI test (delegate scenario → handoff edge and
   parked run in the view; reducer unit test over the projection).
