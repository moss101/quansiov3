/**
 * Work review on the web (APP-012): the WorkGraph as `GET /v1/workgraph` projects it —
 * nodes with their status and run, edges, agents and typed blockers — read-only.
 */

import { list, number, record, text } from "./wire.js";

export interface WorkNodeView {
  readonly id: string;
  readonly kind: string;
  readonly title: string;
  readonly status: string;
  readonly parentId: string | null;
  readonly priority: number;
  readonly runId: string | null;
  readonly runStatus: string | null;
  readonly openQuestions: number;
  readonly updatedAt: string;
}

export interface WorkGraphView {
  readonly workspaceId: string;
  readonly revision: number;
  readonly nodes: readonly WorkNodeView[];
  readonly edges: readonly { readonly id: string; readonly from: string; readonly to: string; readonly kind: string }[];
  readonly agents: readonly { readonly id: string; readonly kind: string; readonly status: string; readonly workNodeId: string | null }[];
  readonly blockers: readonly { readonly nodeId: string; readonly kind: string; readonly detail: string }[];
}

/** Decode `GET /v1/workgraph`. */
export function decodeWorkGraph(payload: unknown): WorkGraphView {
  const root = record(payload);
  return {
    workspaceId: text(root["workspace_id"]) ?? "",
    revision: number(root["revision"]) ?? 0,
    nodes: list(root["nodes"]).map(record).flatMap((row) => {
      const id = text(row["id"]);
      if (!id) {
        return [];
      }
      const run = record(row["run"]);
      return [
        {
          id,
          kind: text(row["kind"]) ?? "",
          title: text(row["title"]) ?? id,
          status: text(row["status"]) ?? "",
          parentId: text(row["parent_id"]),
          priority: number(row["priority"]) ?? 0,
          runId: text(run["id"]),
          runStatus: text(run["status"]),
          openQuestions: list(row["open_questions"]).length,
          updatedAt: text(row["updated_at"]) ?? "",
        },
      ];
    }),
    edges: list(root["edges"]).map(record).flatMap((row) => {
      const id = text(row["id"]);
      const from = text(row["from_node_id"]);
      const to = text(row["to_node_id"]);
      return id && from && to ? [{ id, from, to, kind: text(row["kind"]) ?? "" }] : [];
    }),
    agents: list(root["agents"]).map(record).flatMap((row) => {
      const id = text(row["id"]);
      return id ? [{ id, kind: text(row["kind"]) ?? "", status: text(row["status"]) ?? "", workNodeId: text(row["work_node_id"]) }] : [];
    }),
    blockers: list(root["blockers"]).map(record).flatMap((row) => {
      const nodeId = text(row["node_id"]);
      return nodeId ? [{ nodeId, kind: text(row["kind"]) ?? "", detail: text(row["detail"]) ?? text(row["reason"]) ?? "" }] : [];
    }),
  };
}

/** Nodes needing a human: blocked, waiting, or with an open question. */
export function needsAttention(graph: WorkGraphView): readonly WorkNodeView[] {
  const blocked = new Set(graph.blockers.map((blocker) => blocker.nodeId));
  return graph.nodes.filter((node) => blocked.has(node.id) || node.openQuestions > 0 || (node.runStatus ?? "").startsWith("WAITING"));
}
