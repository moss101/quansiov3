/**
 * Plan / WorkGraph view (APP-005): a projection of the canonical graph revision.
 *
 * Everything the page shows comes from `GET /v1/workgraph` — nodes with their completion
 * contracts and current run, dependency edges, agents, handoff (delegation) edges and
 * typed blockers — keyed by the revision the server reported. Durable `work.*`, `agent.*`
 * and `run.*` events mark the view stale; the page then re-reads the projection. The
 * reducer never mutates the graph itself: edits are `ProposeGraphEdit` commands
 * (`edits.ts`), and a stale revision is a conflict the user resolves, not an overwrite.
 */

import type { StreamFrame } from "../client/stream.js";

export type NodeKind = "objective" | "task" | "subtask" | "wait" | "milestone";
export type NodeStatus = "draft" | "ready" | "blocked" | "in_progress" | "waiting" | "verifying" | "done" | "failed" | "cancelled";
export type EdgeKind = "depends_on" | "parent_of" | "produces_artifact" | "verified_by" | "blocked_by";

export interface RunRef {
  readonly id: string;
  readonly agentThreadId: string;
  readonly status: string;
  readonly generation: number;
}

export interface CompletionContract {
  readonly deterministicChecks: readonly Record<string, unknown>[];
  readonly semanticVerification: { readonly required: boolean; readonly rubricId: string | null };
  readonly humanSignoffRequired: boolean;
}

export interface WorkNodeView {
  readonly id: string;
  readonly kind: NodeKind;
  readonly title: string;
  readonly description: string | null;
  readonly parentId: string | null;
  readonly status: NodeStatus;
  readonly ownerAgentThreadId: string | null;
  readonly createdBy: { readonly kind: string; readonly id: string };
  readonly contract: CompletionContract;
  readonly capabilityNeeds: readonly unknown[];
  readonly priority: number;
  readonly revision: number;
  readonly threadId: string | null;
  readonly origin: string;
  readonly run: RunRef | null;
  readonly openQuestions: readonly { readonly questionId: string; readonly prompt: string }[];
}

export interface WorkEdgeView {
  readonly id: string;
  readonly fromNodeId: string;
  readonly toNodeId: string;
  readonly kind: EdgeKind;
  readonly revision: number;
}

export interface AgentView {
  readonly id: string;
  readonly kind: "teammate" | "worker";
  readonly parentId: string | null;
  readonly workNodeId: string | null;
  readonly status: string;
  readonly suspendedReason: string | null;
  readonly handoffToAgentThreadId: string | null;
}

export interface HandoffView {
  readonly id: string;
  readonly parentAgentThreadId: string;
  readonly childAgentThreadId: string;
  readonly workNodeId: string | null;
  readonly delegatedAt: string;
  readonly joinedAt: string | null;
}

export type BlockerKind = "prerequisite" | "run" | "status";

export interface BlockerView {
  readonly nodeId: string;
  readonly kind: BlockerKind;
  /** The prerequisite node or the parked run, depending on `kind`. */
  readonly ref: string | null;
  readonly status: string | null;
  readonly question: { readonly questionId: string; readonly prompt: string } | null;
}

export interface WorkGraphView {
  readonly workspaceId: string;
  readonly revision: number;
  readonly nodes: readonly WorkNodeView[];
  readonly edges: readonly WorkEdgeView[];
  readonly agents: readonly AgentView[];
  readonly handoffs: readonly HandoffView[];
  readonly blockers: readonly BlockerView[];
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function number(value: unknown): number | null {
  return typeof value === "number" ? value : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

const NODE_KINDS: readonly NodeKind[] = ["objective", "task", "subtask", "wait", "milestone"];
const NODE_STATUSES: readonly NodeStatus[] = ["draft", "ready", "blocked", "in_progress", "waiting", "verifying", "done", "failed", "cancelled"];
const EDGE_KINDS: readonly EdgeKind[] = ["depends_on", "parent_of", "produces_artifact", "verified_by", "blocked_by"];

function contract(value: unknown): CompletionContract {
  const raw = record(value);
  const semantic = record(raw["semantic_verification"]);
  return {
    deterministicChecks: list(raw["deterministic_checks"]).map(record),
    semanticVerification: { required: semantic["required"] === true, rubricId: text(semantic["rubric_id"]) },
    humanSignoffRequired: raw["human_signoff_required"] === true,
  };
}

/** Decode `GET /v1/workgraph`; throws when the payload has no revision. */
export function decodeWorkGraph(payload: unknown): WorkGraphView {
  const root = record(payload);
  const revision = number(root["revision"]);
  const workspaceId = text(root["workspace_id"]);
  if (revision === null || !workspaceId) {
    throw new Error("workgraph projection has no revision");
  }
  const nodes: WorkNodeView[] = [];
  for (const raw of list(root["nodes"])) {
    const node = record(raw);
    const id = text(node["id"]);
    if (!id) {
      continue;
    }
    const kind = text(node["kind"]);
    const status = text(node["status"]);
    const run = record(node["run"]);
    const createdBy = record(node["created_by"]);
    nodes.push({
      id,
      kind: kind && (NODE_KINDS as readonly string[]).includes(kind) ? (kind as NodeKind) : "task",
      title: text(node["title"]) ?? id,
      description: text(node["description"]),
      parentId: text(node["parent_id"]),
      status: status && (NODE_STATUSES as readonly string[]).includes(status) ? (status as NodeStatus) : "draft",
      ownerAgentThreadId: text(node["owner_agent_thread_id"]),
      createdBy: { kind: text(createdBy["kind"]) ?? "unknown", id: text(createdBy["id"]) ?? "" },
      contract: contract(node["completion_contract"]),
      capabilityNeeds: list(node["capability_needs"]),
      priority: number(node["priority"]) ?? 0,
      revision: number(node["revision"]) ?? 1,
      threadId: text(node["thread_id"]),
      origin: text(node["origin"]) ?? "user",
      run: text(run["id"])
        ? { id: text(run["id"]) ?? "", agentThreadId: text(run["agent_thread_id"]) ?? "", status: text(run["status"]) ?? "", generation: number(run["generation"]) ?? 1 }
        : null,
      openQuestions: list(node["open_questions"]).map(record).flatMap((question) => {
        const questionId = text(question["question_id"]);
        return questionId ? [{ questionId, prompt: text(question["prompt"]) ?? "" }] : [];
      }),
    });
  }
  const edges: WorkEdgeView[] = [];
  for (const raw of list(root["edges"])) {
    const edge = record(raw);
    const id = text(edge["id"]);
    const from = text(edge["from_node_id"]);
    const to = text(edge["to_node_id"]);
    const kind = text(edge["kind"]);
    if (!id || !from || !to || !kind || !(EDGE_KINDS as readonly string[]).includes(kind)) {
      continue;
    }
    edges.push({ id, fromNodeId: from, toNodeId: to, kind: kind as EdgeKind, revision: number(edge["revision"]) ?? 1 });
  }
  const agents: AgentView[] = [];
  for (const raw of list(root["agents"])) {
    const agent = record(raw);
    const id = text(agent["id"]);
    if (!id) {
      continue;
    }
    agents.push({
      id,
      kind: text(agent["kind"]) === "worker" ? "worker" : "teammate",
      parentId: text(agent["parent_id"]),
      workNodeId: text(agent["work_node_id"]),
      status: text(agent["status"]) ?? "",
      suspendedReason: text(agent["suspended_reason"]),
      handoffToAgentThreadId: text(agent["handoff_to_agent_thread_id"]),
    });
  }
  const handoffs: HandoffView[] = [];
  for (const raw of list(root["handoffs"])) {
    const handoff = record(raw);
    const id = text(handoff["id"]);
    const parent = text(handoff["parent_agent_thread_id"]);
    const child = text(handoff["child_agent_thread_id"]);
    if (!id || !parent || !child) {
      continue;
    }
    handoffs.push({
      id,
      parentAgentThreadId: parent,
      childAgentThreadId: child,
      workNodeId: text(handoff["work_node_id"]),
      delegatedAt: text(handoff["delegated_at"]) ?? "",
      joinedAt: text(handoff["joined_at"]),
    });
  }
  const blockers: BlockerView[] = [];
  for (const raw of list(root["blockers"])) {
    const blocker = record(raw);
    const nodeId = text(blocker["node_id"]);
    const kind = text(blocker["kind"]);
    if (!nodeId || (kind !== "prerequisite" && kind !== "run" && kind !== "status")) {
      continue;
    }
    const question = record(blocker["question"]);
    blockers.push({
      nodeId,
      kind,
      ref: text(blocker["ref"]),
      status: text(blocker["status"]),
      question: text(question["question_id"]) ? { questionId: text(question["question_id"]) ?? "", prompt: text(question["prompt"]) ?? "" } : null,
    });
  }
  return { workspaceId, revision, nodes, edges, agents, handoffs, blockers };
}

// ---- view state ----

export interface WorkState {
  readonly workspaceId: string;
  readonly graph: WorkGraphView | null;
  /** The revision the page last rendered; edits are proposed against it. */
  readonly observedRevision: number | null;
  readonly selectedNodeId: string | null;
  readonly stale: boolean;
  readonly notice: string | null;
}

export type WorkAction =
  | { readonly type: "graph.loaded"; readonly graph: WorkGraphView }
  | { readonly type: "frame"; readonly frame: StreamFrame }
  | { readonly type: "select"; readonly nodeId: string | null }
  | { readonly type: "stale" }
  | { readonly type: "notice"; readonly notice: string | null };

export function initialWorkState(workspaceId: string): WorkState {
  return { workspaceId, graph: null, observedRevision: null, selectedNodeId: null, stale: true, notice: null };
}

/** Event families that change what the plan view shows. */
const GRAPH_EVENT_PREFIXES = ["work.", "agent.", "run.", "question."];

export function workReducer(state: WorkState, action: WorkAction): WorkState {
  switch (action.type) {
    case "graph.loaded":
      if (action.graph.workspaceId !== state.workspaceId) {
        return state;
      }
      return {
        ...state,
        graph: action.graph,
        observedRevision: action.graph.revision,
        selectedNodeId: state.selectedNodeId && action.graph.nodes.some((node) => node.id === state.selectedNodeId) ? state.selectedNodeId : null,
        stale: false,
      };
    case "frame": {
      if (action.frame.kind !== "event") {
        return state;
      }
      const event = action.frame.event;
      const type = text(event["type"]) ?? "";
      const workspaceId = text(event["workspace_id"]);
      if (workspaceId && workspaceId !== state.workspaceId) {
        return state;
      }
      return GRAPH_EVENT_PREFIXES.some((prefix) => type.startsWith(prefix)) ? { ...state, stale: true } : state;
    }
    case "select":
      return { ...state, selectedNodeId: action.nodeId };
    case "stale":
      return { ...state, stale: true };
    case "notice":
      return { ...state, notice: action.notice };
    default:
      return state;
  }
}

/** True when the rendered view is the canonical revision it claims to be. */
export function matchesCanonical(state: WorkState, canonical: WorkGraphView): boolean {
  return state.graph !== null && state.observedRevision === canonical.revision && state.graph.revision === canonical.revision
    && state.graph.nodes.length === canonical.nodes.length
    && state.graph.nodes.every((node, index) => {
      const other = canonical.nodes[index];
      return other !== undefined && node.id === other.id && node.revision === other.revision;
    });
}

/** The children of a node (by `parent_id`), in projection order. */
export function childrenOf(graph: WorkGraphView, nodeId: string | null): readonly WorkNodeView[] {
  return graph.nodes.filter((node) => node.parentId === nodeId);
}

/** The nodes `nodeId` depends on (`from depends_on to`). */
export function prerequisitesOf(graph: WorkGraphView, nodeId: string): readonly WorkNodeView[] {
  const ids = graph.edges.filter((edge) => (edge.kind === "depends_on" || edge.kind === "blocked_by") && edge.fromNodeId === nodeId).map((edge) => edge.toNodeId);
  return graph.nodes.filter((node) => ids.includes(node.id));
}

/** The handoffs that concern a node: delegations of work on it. */
export function handoffsFor(graph: WorkGraphView, nodeId: string): readonly HandoffView[] {
  return graph.handoffs.filter((handoff) => handoff.workNodeId === nodeId);
}
