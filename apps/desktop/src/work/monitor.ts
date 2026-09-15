/**
 * Plan / WorkGraph / agent control (APP-005).
 *
 * The view is a projection of a canonical graph revision. User edits are
 * ProposeGraphEdit commands, never local mutation. A stale revision is a
 * conflict, not a silent overwrite.
 */

export interface GraphRevision {
  readonly revision: number;
  readonly nodes: readonly string[];
  readonly agents: readonly string[];
  readonly handoffs: readonly string[];
  readonly blockers: readonly string[];
  readonly completionContract: string;
}

export type EditResult =
  | { readonly ok: true; readonly revision: GraphRevision }
  | { readonly ok: false; readonly code: "CONFLICT_REVISION"; readonly server: GraphRevision };

export function project(revision: GraphRevision): GraphRevision {
  return { ...revision, nodes: [...revision.nodes] };
}

export function proposeEdit(
  local: GraphRevision,
  server: GraphRevision,
  addNode: string,
): EditResult {
  if (local.revision !== server.revision) {
    return { ok: false, code: "CONFLICT_REVISION", server };
  }
  return {
    ok: true,
    revision: {
      ...server,
      revision: server.revision + 1,
      nodes: [...server.nodes, addNode],
    },
  };
}

export function matchesCanonical(view: GraphRevision, canonical: GraphRevision): boolean {
  return (
    view.revision === canonical.revision &&
    view.nodes.join(",") === canonical.nodes.join(",")
  );
}
