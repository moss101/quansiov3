/**
 * APP-005: the plan view's projection decoding and reducer, and edits as validated
 * commands — a draft of intents becomes `ProposeGraphEdit` params, a stale revision is a
 * conflict the user rebases (never a silent overwrite) — pure, no server.
 */

import { describe, expect, it } from "vitest";

import type { StreamFrame } from "../src/client/stream.js";
import {
  childrenOf,
  decodeWorkGraph,
  describeIntent,
  handoffsFor,
  initialWorkState,
  matchesCanonical,
  needsTwoProposals,
  newDraft,
  prerequisitesOf,
  proposal,
  rebase,
  resolveLocalIds,
  withIntent,
  withoutIntent,
  workReducer,
  type WorkGraphView,
} from "../src/index.js";

const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const OBJ = "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA0";
const A = "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const B = "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";

function wire(revision: number, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    workspace_id: WORKSPACE,
    revision,
    nodes: [
      { id: OBJ, kind: "objective", title: "Ship", status: "in_progress", parent_id: null, created_by: { kind: "user", id: "usr_1" }, completion_contract: { deterministic_checks: [{ kind: "artifact_exists" }], semantic_verification: { required: true, rubric_id: "rub_1" }, human_signoff_required: true }, capability_needs: [], priority: 1, revision: 1, thread_id: "thr_1", origin: "user", run: null, open_questions: [] },
      { id: A, kind: "task", title: "gather", status: "done", parent_id: OBJ, created_by: {}, completion_contract: {}, priority: 1, revision: 2, origin: "plan_proposal", run: { id: "run_a", agent_thread_id: "ath_1", status: "SUCCEEDED", generation: 1 }, open_questions: [] },
      { id: B, kind: "task", title: "write", status: "ready", parent_id: OBJ, created_by: {}, completion_contract: {}, priority: 2, revision: 3, origin: "user", run: { id: "run_b", agent_thread_id: "ath_1", status: "WAITING_CHILD", generation: 1 }, open_questions: [] },
    ],
    edges: [{ id: "we_1", from_node_id: B, to_node_id: A, kind: "depends_on", revision: 3 }],
    agents: [
      { id: "ath_1", kind: "teammate", parent_id: null, work_node_id: OBJ, status: "ACTIVE" },
      { id: "ath_2", kind: "worker", parent_id: "ath_1", work_node_id: B, status: "ACTIVE" },
    ],
    handoffs: [{ id: "age_1", parent_agent_thread_id: "ath_1", child_agent_thread_id: "ath_2", work_node_id: B, delegated_at: "2026-09-20T00:00:00Z", joined_at: null }],
    blockers: [{ node_id: B, kind: "run", ref: "run_b", status: "WAITING_CHILD", question: null }],
    ...extra,
  };
}

function event(type: string, workspaceId: string | null = WORKSPACE): StreamFrame {
  return { kind: "event", cursor: "c", event: { type, workspace_id: workspaceId, payload: {} } };
}

describe("graph projection", () => {
  it("decodes nodes, edges, agents, handoffs and blockers and answers the plan questions", () => {
    const graph = decodeWorkGraph(wire(3));
    expect(graph.revision).toBe(3);
    expect(graph.nodes.map((node) => node.kind)).toEqual(["objective", "task", "task"]);
    expect(graph.nodes[0]?.contract).toEqual({ deterministicChecks: [{ kind: "artifact_exists" }], semanticVerification: { required: true, rubricId: "rub_1" }, humanSignoffRequired: true });
    expect(graph.nodes[2]?.run).toEqual({ id: "run_b", agentThreadId: "ath_1", status: "WAITING_CHILD", generation: 1 });
    expect(childrenOf(graph, OBJ).map((node) => node.id)).toEqual([A, B]);
    expect(prerequisitesOf(graph, B).map((node) => node.id)).toEqual([A]);
    expect(handoffsFor(graph, B)).toHaveLength(1);
    expect(graph.agents.find((agent) => agent.kind === "worker")?.parentId).toBe("ath_1");
    expect(graph.blockers[0]).toMatchObject({ nodeId: B, kind: "run", status: "WAITING_CHILD" });
    expect(() => decodeWorkGraph({ nodes: [] })).toThrow(/revision/);
  });

  it("renders the canonical revision and marks the view stale on work, agent and run events only", () => {
    let state = workReducer(initialWorkState(WORKSPACE), { type: "graph.loaded", graph: decodeWorkGraph(wire(3)) });
    expect(state.stale).toBe(false);
    expect(state.observedRevision).toBe(3);
    expect(matchesCanonical(state, decodeWorkGraph(wire(3)))).toBe(true);
    expect(matchesCanonical(state, decodeWorkGraph(wire(4)))).toBe(false);
    state = workReducer(state, { type: "frame", frame: event("thread.message_posted") });
    expect(state.stale).toBe(false);
    state = workReducer(state, { type: "frame", frame: event("work.plan_applied", "ws_other") });
    expect(state.stale).toBe(false);
    state = workReducer(state, { type: "frame", frame: event("work.plan_applied") });
    expect(state.stale).toBe(true);
    state = workReducer(state, { type: "graph.loaded", graph: decodeWorkGraph(wire(4)) });
    expect(state.stale).toBe(false);
    for (const type of ["agent.delegated", "run.waiting", "question.asked"]) {
      expect(workReducer(state, { type: "frame", frame: event(type) }).stale).toBe(true);
    }
    // A graph for another workspace is ignored; selection survives only while the node exists.
    expect(workReducer(state, { type: "graph.loaded", graph: { ...decodeWorkGraph(wire(9)), workspaceId: "ws_other" } })).toBe(state);
    state = workReducer(state, { type: "select", nodeId: B });
    state = workReducer(state, { type: "graph.loaded", graph: decodeWorkGraph(wire(5, { nodes: (wire(5)["nodes"] as unknown[]).slice(0, 2) })) });
    expect(state.selectedNodeId).toBeNull();
  });
});

describe("validated edits", () => {
  it("turns a draft into ProposeGraphEdit params against the observed revision, nodes before edges", () => {
    let draft = newDraft(WORKSPACE, 3, "plan the report");
    draft = withIntent(draft, { kind: "add_node", localId: "review", nodeKind: "task", title: " review ", description: "read it", parentId: OBJ, priority: 2, contract: null });
    draft = withIntent(draft, { kind: "add_edge", localId: "e1", fromNodeId: "review", toNodeId: B, edgeKind: "depends_on" });
    draft = withIntent(draft, { kind: "transition", nodeId: A, to: "cancelled", expectedRevision: 2 });
    draft = withIntent(draft, { kind: "remove_edge", edgeId: "we_1", expectedRevision: 3 });
    expect(needsTwoProposals(draft)).toBe(true);
    const nodes = proposal(draft, "nodes");
    expect(nodes.base_revision).toBe(3);
    expect(nodes.nodes_add).toEqual([{ id: "review", kind: "task", title: "review", status: "draft", completion_contract: { deterministic_checks: [], semantic_verification: { required: false }, human_signoff_required: true }, capability_needs: [], priority: 2, description: "read it", parent_id: OBJ }]);
    expect(nodes.edges_add).toEqual([]);
    expect(nodes.nodes_update).toEqual([]);
    const edges = proposal({ ...draft, baseRevision: 4 }, "edges", { review: "wn_new" });
    expect(edges.base_revision).toBe(4);
    expect(edges.nodes_add).toEqual([]);
    expect(edges.edges_add).toEqual([{ id: "e1", from_node_id: "wn_new", to_node_id: B, kind: "depends_on" }]);
    expect(edges.nodes_update).toEqual([{ node_id: A, expected_revision: 2, to: "cancelled" }]);
    expect(edges.edges_remove).toEqual([{ edge_id: "we_1", expected_revision: 3 }]);
    expect(edges.capability_needs).toEqual([]);
    expect(proposal(draft).nodes_add).toHaveLength(1);
    expect(withoutIntent(draft, 0).intents).toHaveLength(3);
    const first = draft.intents[0];
    if (!first) {
      throw new Error("an intent expected");
    }
    expect(describeIntent(first)).toBe(`add task "review" under ${OBJ}`);
    expect(() => withIntent(draft, { kind: "add_node", localId: "review", nodeKind: "task", title: "dup", description: null, parentId: null, priority: 1, contract: null })).toThrow(/already/);
    expect(() => withIntent(draft, { kind: "add_node", localId: "x", nodeKind: "task", title: "   ", description: null, parentId: null, priority: 1, contract: null })).toThrow(/title/);
    expect(() => withIntent(draft, { kind: "add_edge", localId: "loop", fromNodeId: A, toNodeId: A, edgeKind: "depends_on" })).toThrow(/itself/);
  });

  it("rebases a conflicting draft onto the server revision, keeping what still applies and reporting the rest", () => {
    let draft = newDraft(WORKSPACE, 3);
    draft = withIntent(draft, { kind: "add_node", localId: "n1", nodeKind: "task", title: "new", description: null, parentId: OBJ, priority: 1, contract: null });
    draft = withIntent(draft, { kind: "add_node", localId: "n2", nodeKind: "task", title: "orphan", description: null, parentId: A, priority: 1, contract: null });
    draft = withIntent(draft, { kind: "add_edge", localId: "e1", fromNodeId: B, toNodeId: A, edgeKind: "depends_on" });
    draft = withIntent(draft, { kind: "transition", nodeId: B, to: "ready", expectedRevision: 3 });
    draft = withIntent(draft, { kind: "remove_edge", edgeId: "we_gone", expectedRevision: 3 });
    // The server moved to revision 6: A was removed, B's revision advanced and it is already ready.
    const server: WorkGraphView = decodeWorkGraph(wire(6, {
      nodes: [
        { id: OBJ, kind: "objective", title: "Ship", status: "in_progress", parent_id: null, revision: 1 },
        { id: B, kind: "task", title: "write", status: "ready", parent_id: OBJ, revision: 6 },
      ],
      edges: [],
    }));
    const outcome = rebase(draft, server);
    expect(outcome.draft.baseRevision).toBe(6);
    expect(outcome.draft.intents.map(describeIntent)).toEqual([`add task "new" under ${OBJ}`]);
    expect(outcome.dropped.map((item) => item.reason)).toEqual([
      `parent ${A} no longer exists`,
      `node ${A} no longer exists`,
      `node ${B} is already ready`,
      "edge we_gone no longer exists",
    ]);
    // A transition whose node moved on is re-pointed at the current revision.
    const moved = rebase(withIntent(newDraft(WORKSPACE, 3), { kind: "transition", nodeId: B, to: "cancelled", expectedRevision: 3 }), server);
    expect(moved.draft.intents).toEqual([{ kind: "transition", nodeId: B, to: "cancelled", expectedRevision: 6 }]);
    expect(() => rebase(draft, decodeWorkGraph(wire(2)))).toThrow(/older/);
  });

  it("maps local ids to the canonical ids the projection gained", () => {
    const draft = withIntent(newDraft(WORKSPACE, 3), { kind: "add_node", localId: "n1", nodeKind: "task", title: "review", description: null, parentId: OBJ, priority: 1, contract: null });
    const before = decodeWorkGraph(wire(3));
    const after = decodeWorkGraph(wire(4, { nodes: [...(wire(4)["nodes"] as unknown[]), { id: "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA9", kind: "task", title: "review", status: "draft", parent_id: OBJ, revision: 4 }] }));
    expect(resolveLocalIds(draft, before, after)).toEqual({ n1: "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA9" });
    expect(resolveLocalIds(draft, before, before)).toEqual({});
  });
});
