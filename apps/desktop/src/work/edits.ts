/**
 * Graph edits (APP-005): user changes to the plan are `ProposeGraphEdit` commands
 * validated by the server like any PlanProposal, never local mutation.
 *
 * The page keeps a draft of typed intents (add a node, add a dependency, transition a
 * node, remove an edge) and the revision it observed. `proposal()` turns the draft into
 * the command's params. When the server answers `CONFLICT_REVISION`, `rebase()` replays
 * the intents onto the newer revision: an intent that still applies is kept, one that no
 * longer can (its node or edge is gone, or its expected revision moved) is dropped and
 * reported, so the user decides — the older view is never written over the newer one.
 *
 * The planner refuses an edge that names a node created in the same proposal, so a draft
 * with both is sent as two proposals: nodes first, then edges against the new revision.
 */

import type { EdgeKind, NodeKind, NodeStatus, WorkGraphView } from "./graph.js";

export type EditIntent =
  | {
      readonly kind: "add_node";
      readonly localId: string;
      readonly nodeKind: NodeKind;
      readonly title: string;
      readonly description: string | null;
      readonly parentId: string | null;
      readonly priority: number;
      readonly contract: Record<string, unknown> | null;
    }
  | { readonly kind: "add_edge"; readonly localId: string; readonly fromNodeId: string; readonly toNodeId: string; readonly edgeKind: EdgeKind }
  | { readonly kind: "transition"; readonly nodeId: string; readonly to: NodeStatus; readonly expectedRevision: number }
  | { readonly kind: "remove_edge"; readonly edgeId: string; readonly expectedRevision: number };

export interface EditDraft {
  readonly workspaceId: string;
  readonly baseRevision: number;
  readonly intents: readonly EditIntent[];
  readonly rationale: string;
}

export interface ProposalParams {
  readonly workspace_id: string;
  readonly base_revision: number;
  readonly nodes_add: readonly Record<string, unknown>[];
  readonly nodes_update: readonly Record<string, unknown>[];
  readonly edges_add: readonly Record<string, unknown>[];
  readonly edges_remove: readonly Record<string, unknown>[];
  readonly rationale: string;
  readonly capability_needs: readonly unknown[];
}

export const MAX_TITLE_CHARS = 200;

export function newDraft(workspaceId: string, baseRevision: number, rationale = "user edit"): EditDraft {
  return { workspaceId, baseRevision, intents: [], rationale };
}

export function withIntent(draft: EditDraft, intent: EditIntent): EditDraft {
  if (intent.kind === "add_node") {
    const title = intent.title.trim();
    if (!title || title.length > MAX_TITLE_CHARS) {
      throw new Error(`a node title is 1..=${String(MAX_TITLE_CHARS)} characters`);
    }
    if (draft.intents.some((existing) => existing.kind === "add_node" && existing.localId === intent.localId)) {
      throw new Error(`local id ${intent.localId} is already in the draft`);
    }
  }
  if (intent.kind === "add_edge" && intent.fromNodeId === intent.toNodeId) {
    throw new Error("an edge cannot join a node to itself");
  }
  return { ...draft, intents: [...draft.intents, intent] };
}

export function withoutIntent(draft: EditDraft, index: number): EditDraft {
  return { ...draft, intents: draft.intents.filter((_, at) => at !== index) };
}

/** Whether the draft must be sent as two proposals (nodes, then edges). */
export function needsTwoProposals(draft: EditDraft): boolean {
  const localIds = new Set(draft.intents.flatMap((intent) => (intent.kind === "add_node" ? [intent.localId] : [])));
  return draft.intents.some((intent) => intent.kind === "add_edge" && (localIds.has(intent.fromNodeId) || localIds.has(intent.toNodeId)));
}

/** The default contract for a node the user adds without one: a human signs it off. */
export const DEFAULT_CONTRACT: Record<string, unknown> = {
  deterministic_checks: [],
  semantic_verification: { required: false },
  human_signoff_required: true,
};

/**
 * The `ProposeGraphEdit` params for the draft's intents. With `phase: "nodes"` only node
 * intents are sent; with `"edges"` only the rest, with `localIds` resolved to canonical
 * ids (from the projection after the first phase, by title).
 */
export function proposal(draft: EditDraft, phase: "all" | "nodes" | "edges" = "all", localIds: Readonly<Record<string, string>> = {}): ProposalParams {
  const resolve = (id: string): string => localIds[id] ?? id;
  const nodesAdd: Record<string, unknown>[] = [];
  const nodesUpdate: Record<string, unknown>[] = [];
  const edgesAdd: Record<string, unknown>[] = [];
  const edgesRemove: Record<string, unknown>[] = [];
  for (const intent of draft.intents) {
    switch (intent.kind) {
      case "add_node":
        if (phase !== "edges") {
          const node: Record<string, unknown> = {
            id: intent.localId,
            kind: intent.nodeKind,
            title: intent.title.trim(),
            status: "draft",
            completion_contract: intent.contract ?? DEFAULT_CONTRACT,
            capability_needs: [],
            priority: intent.priority,
          };
          if (intent.description) {
            node["description"] = intent.description;
          }
          if (intent.parentId) {
            node["parent_id"] = resolve(intent.parentId);
          }
          nodesAdd.push(node);
        }
        break;
      case "add_edge":
        if (phase !== "nodes") {
          edgesAdd.push({ id: intent.localId, from_node_id: resolve(intent.fromNodeId), to_node_id: resolve(intent.toNodeId), kind: intent.edgeKind });
        }
        break;
      case "transition":
        if (phase !== "nodes") {
          nodesUpdate.push({ node_id: intent.nodeId, expected_revision: intent.expectedRevision, to: intent.to });
        }
        break;
      case "remove_edge":
        if (phase !== "nodes") {
          edgesRemove.push({ edge_id: intent.edgeId, expected_revision: intent.expectedRevision });
        }
        break;
      default:
        break;
    }
  }
  return {
    workspace_id: draft.workspaceId,
    base_revision: draft.baseRevision,
    nodes_add: nodesAdd,
    nodes_update: nodesUpdate,
    edges_add: edgesAdd,
    edges_remove: edgesRemove,
    rationale: draft.rationale,
    capability_needs: [],
  };
}

export interface RebaseOutcome {
  readonly draft: EditDraft;
  /** Intents that no longer apply on the newer revision, with why. */
  readonly dropped: readonly { readonly intent: EditIntent; readonly reason: string }[];
}

/**
 * Replay the draft onto a newer revision of the graph. Node additions always carry over;
 * an edge or transition survives only if its nodes/edge still exist, and a transition or
 * removal whose expected revision moved is re-pointed at the current one (the server
 * still validates the transition itself).
 */
export function rebase(draft: EditDraft, current: WorkGraphView): RebaseOutcome {
  if (current.revision < draft.baseRevision) {
    throw new Error("cannot rebase onto an older revision");
  }
  const nodeIds = new Set(current.nodes.map((node) => node.id));
  const localIds = new Set(draft.intents.flatMap((intent) => (intent.kind === "add_node" ? [intent.localId] : [])));
  const nodeRevision = new Map(current.nodes.map((node) => [node.id, node.revision]));
  const edgeRevision = new Map(current.edges.map((edge) => [edge.id, edge.revision]));
  const kept: EditIntent[] = [];
  const dropped: { intent: EditIntent; reason: string }[] = [];
  for (const intent of draft.intents) {
    switch (intent.kind) {
      case "add_node":
        if (intent.parentId && !nodeIds.has(intent.parentId) && !localIds.has(intent.parentId)) {
          dropped.push({ intent, reason: `parent ${intent.parentId} no longer exists` });
        } else {
          kept.push(intent);
        }
        break;
      case "add_edge": {
        const missing = [intent.fromNodeId, intent.toNodeId].find((id) => !nodeIds.has(id) && !localIds.has(id));
        if (missing) {
          dropped.push({ intent, reason: `node ${missing} no longer exists` });
        } else if (current.edges.some((edge) => edge.kind === intent.edgeKind && edge.fromNodeId === intent.fromNodeId && edge.toNodeId === intent.toNodeId)) {
          dropped.push({ intent, reason: "the edge already exists on the current revision" });
        } else {
          kept.push(intent);
        }
        break;
      }
      case "transition": {
        const revision = nodeRevision.get(intent.nodeId);
        if (revision === undefined) {
          dropped.push({ intent, reason: `node ${intent.nodeId} no longer exists` });
        } else if (current.nodes.find((node) => node.id === intent.nodeId)?.status === intent.to) {
          dropped.push({ intent, reason: `node ${intent.nodeId} is already ${intent.to}` });
        } else {
          kept.push({ ...intent, expectedRevision: revision });
        }
        break;
      }
      case "remove_edge": {
        const revision = edgeRevision.get(intent.edgeId);
        if (revision === undefined) {
          dropped.push({ intent, reason: `edge ${intent.edgeId} no longer exists` });
        } else {
          kept.push({ ...intent, expectedRevision: revision });
        }
        break;
      }
      default:
        break;
    }
  }
  return { draft: { ...draft, baseRevision: current.revision, intents: kept }, dropped };
}

/** Map the draft's local node ids to the canonical ids the projection now holds, by title under the same parent. */
export function resolveLocalIds(draft: EditDraft, before: WorkGraphView | null, after: WorkGraphView): Record<string, string> {
  const known = new Set((before?.nodes ?? []).map((node) => node.id));
  const created = after.nodes.filter((node) => !known.has(node.id));
  const resolved: Record<string, string> = {};
  for (const intent of draft.intents) {
    if (intent.kind !== "add_node") {
      continue;
    }
    const match = created.find((node) => node.title === intent.title.trim() && (node.parentId ?? null) === (intent.parentId ? (resolved[intent.parentId] ?? intent.parentId) : null));
    if (match) {
      resolved[intent.localId] = match.id;
    }
  }
  return resolved;
}

/** A summary line per intent, for the draft list. */
export function describeIntent(intent: EditIntent): string {
  switch (intent.kind) {
    case "add_node":
      return `add ${intent.nodeKind} "${intent.title.trim()}"${intent.parentId ? ` under ${intent.parentId}` : ""}`;
    case "add_edge":
      return `${intent.fromNodeId} ${intent.edgeKind} ${intent.toNodeId}`;
    case "transition":
      return `${intent.nodeId} → ${intent.to}`;
    case "remove_edge":
      return `remove ${intent.edgeId}`;
    default:
      return "";
  }
}
