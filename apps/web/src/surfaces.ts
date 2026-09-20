/**
 * Web administration and review surfaces (DOSSIER.md §15, APP-012).
 *
 * Canonical owner: `apps/web`. Each surface is a projection plus a command surface over
 * the public API v1 and names the action family the server gates it by; the app offers a
 * surface when the session's roles allow that action and shows the server's refusal when
 * they do not. Every surface is `serverOnly`: no host-privileged API exists on the web.
 */

import { allows, type Action, type Roles } from "./rbac.js";

export type WebSurfaceId =
  | "sign-in"
  | "work-review"
  | "approvals"
  | "artifacts"
  | "policies"
  | "connectors"
  | "users"
  | "capabilities"
  | "audit-and-privacy"
  | "usage";

export interface WebSurfaceDescriptor {
  readonly id: WebSurfaceId;
  /** The action family the server gates the surface's projection by (`null`: signed-in read). */
  readonly action: Action | null;
  /** Requires an administrative role (RBAC) to render. */
  readonly adminOnly: boolean;
  /** No privileged local-host API may be used by this surface. */
  readonly serverOnly: true;
  /** The projections the surface reads. */
  readonly projections: readonly string[];
  /** The commands the surface may send. */
  readonly commands: readonly string[];
}

export const WEB_SURFACES: readonly WebSurfaceDescriptor[] = [
  { id: "sign-in", action: null, adminOnly: false, serverOnly: true, projections: ["/v1/me"], commands: [] },
  { id: "work-review", action: "read", adminOnly: false, serverOnly: true, projections: ["/v1/workgraph", "/v1/runs/{id}", "/v1/effects"], commands: [] },
  { id: "approvals", action: "read", adminOnly: false, serverOnly: true, projections: ["/v1/approvals", "/v1/effects"], commands: ["ApproveEffect", "DenyEffect"] },
  { id: "artifacts", action: "read", adminOnly: false, serverOnly: true, projections: ["/v1/artifacts", "/v1/artifacts/{id}/versions", "/v1/search?q="], commands: [] },
  { id: "policies", action: "manage_workspace", adminOnly: true, serverOnly: true, projections: ["/v1/policies"], commands: ["SetPolicy"] },
  { id: "connectors", action: "manage_workspace", adminOnly: true, serverOnly: true, projections: ["/v1/connectors"], commands: [] },
  { id: "users", action: "manage_workspace", adminOnly: true, serverOnly: true, projections: ["/v1/members"], commands: ["InviteMember", "UpdateMembership", "RemoveMember"] },
  { id: "capabilities", action: "manage_workspace", adminOnly: true, serverOnly: true, projections: ["/v1/policies", "/v1/skills"], commands: [] },
  { id: "audit-and-privacy", action: "manage_tenant", adminOnly: true, serverOnly: true, projections: ["/v1/audit"], commands: [] },
  { id: "usage", action: "view_usage", adminOnly: true, serverOnly: true, projections: ["/v1/usage"], commands: [] },
];

export function surface(id: WebSurfaceId): WebSurfaceDescriptor {
  const found = WEB_SURFACES.find((item) => item.id === id);
  if (!found) {
    throw new Error(`unknown web surface: ${id}`);
  }
  return found;
}

export function adminSurfaces(): readonly WebSurfaceId[] {
  return WEB_SURFACES.filter((item) => item.adminOnly).map((item) => item.id);
}

/** Whether the roles allow the surface — what the navigation offers, never what the server decides. */
export function canOpen(id: WebSurfaceId, roles: Roles): boolean {
  const descriptor = surface(id);
  return descriptor.action === null || allows(roles, descriptor.action);
}

/** The surfaces to list for a session. */
export function offered(roles: Roles): readonly WebSurfaceDescriptor[] {
  return WEB_SURFACES.filter((item) => item.id !== "sign-in" && canOpen(item.id, roles));
}
