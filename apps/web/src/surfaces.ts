/**
 * Web administration and review surfaces (DOSSIER.md §15, APP-012).
 *
 * Canonical owner: `apps/web`. The web surface provides remote review, approval and
 * administration without privileged local-host APIs; it is a projection plus command
 * surface over the public API v1 and holds no runtime authority.
 */
export type WebSurfaceId =
  | "sign-in"
  | "work-review"
  | "approvals"
  | "artifacts"
  | "audit-and-privacy"
  | "usage"
  | "admin"
  | "capability-admin";

export interface WebSurfaceDescriptor {
  readonly id: WebSurfaceId;
  /** Requires an administrative role (RBAC) to render. */
  readonly adminOnly: boolean;
  /** No privileged local-host API may be used by this surface. */
  readonly serverOnly: true;
}

export const WEB_SURFACES: readonly WebSurfaceDescriptor[] = [
  { id: "sign-in", adminOnly: false, serverOnly: true },
  { id: "work-review", adminOnly: false, serverOnly: true },
  { id: "approvals", adminOnly: false, serverOnly: true },
  { id: "artifacts", adminOnly: false, serverOnly: true },
  { id: "audit-and-privacy", adminOnly: true, serverOnly: true },
  { id: "usage", adminOnly: true, serverOnly: true },
  { id: "admin", adminOnly: true, serverOnly: true },
  { id: "capability-admin", adminOnly: true, serverOnly: true },
];

export function adminSurfaces(): readonly WebSurfaceId[] {
  return WEB_SURFACES.filter((s) => s.adminOnly).map((s) => s.id);
}
