/**
 * Desktop product surfaces (DOSSIER.md §15).
 *
 * Canonical owner: `apps/desktop`. This module is the single declarative list of
 * surfaces the desktop shell must make first-class; the shell renders them and the
 * surfaces test fails if one is dropped. It holds no runtime authority: every
 * surface is a projection plus a command surface against the Rust server.
 */
export type SurfaceId =
  | "conversation"
  | "objectives"
  | "teammates"
  | "plan-and-agents"
  | "run-status"
  | "approvals"
  | "evidence-timeline"
  | "live-browser-terminal"
  | "artifacts"
  | "knowledge-controls"
  | "collaboration"
  | "automations"
  | "connectors"
  | "capability-admin"
  | "model-usage";

export interface SurfaceDescriptor {
  readonly id: SurfaceId;
  /** Whether the surface shows a consequence preview that requires approval. */
  readonly requiresApproval: boolean;
  /** Whether the surface needs the local host (capsule/native bridge) rather than only the server. */
  readonly needsLocalHost: boolean;
}

export const DESKTOP_SURFACES: readonly SurfaceDescriptor[] = [
  { id: "conversation", requiresApproval: false, needsLocalHost: false },
  { id: "objectives", requiresApproval: false, needsLocalHost: false },
  { id: "teammates", requiresApproval: false, needsLocalHost: false },
  { id: "plan-and-agents", requiresApproval: false, needsLocalHost: false },
  { id: "run-status", requiresApproval: false, needsLocalHost: false },
  { id: "approvals", requiresApproval: true, needsLocalHost: false },
  { id: "evidence-timeline", requiresApproval: false, needsLocalHost: false },
  { id: "live-browser-terminal", requiresApproval: true, needsLocalHost: true },
  { id: "artifacts", requiresApproval: false, needsLocalHost: false },
  { id: "knowledge-controls", requiresApproval: false, needsLocalHost: false },
  { id: "collaboration", requiresApproval: false, needsLocalHost: false },
  { id: "automations", requiresApproval: false, needsLocalHost: false },
  { id: "connectors", requiresApproval: false, needsLocalHost: false },
  { id: "capability-admin", requiresApproval: true, needsLocalHost: false },
  { id: "model-usage", requiresApproval: false, needsLocalHost: false },
];

export function surface(id: SurfaceId): SurfaceDescriptor {
  const found = DESKTOP_SURFACES.find((s) => s.id === id);
  if (!found) {
    throw new Error(`unknown desktop surface: ${id}`);
  }
  return found;
}
