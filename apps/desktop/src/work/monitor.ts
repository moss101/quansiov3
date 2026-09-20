/**
 * Plan / WorkGraph / agent control (APP-005) — the module index.
 *
 * `graph.ts` projects the canonical revision; `edits.ts` turns user intents into
 * `ProposeGraphEdit` commands and rebases them on `CONFLICT_REVISION`; the renderer is
 * `renderer/Work.tsx`. Nothing here holds authority: the graph is what the server's
 * revision says it is.
 */

export * from "./graph.js";
export * from "./edits.js";

export const PROPOSE_COMMAND = "ProposeGraphEdit";
export const OBJECTIVE_COMMAND = "CreateObjective";
