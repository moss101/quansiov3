/**
 * Web RBAC gate (APP-012).
 *
 * Admin-only pages are decided by the server-issued role, not by hiding a link.
 * The web app uses the same public command/error contracts as desktop and never
 * exposes host-privileged APIs.
 */

import { WEB_SURFACES, type WebSurfaceId } from "./surfaces.js";

export type ServerRole = "owner" | "admin" | "billing" | "member" | "viewer";

const ADMIN_ROLES: readonly ServerRole[] = ["owner", "admin"];

export function canOpen(surface: WebSurfaceId, role: ServerRole): boolean {
  const descriptor = WEB_SURFACES.find((item) => item.id === surface);
  if (!descriptor) {
    return false;
  }
  if (!descriptor.adminOnly) {
    return true;
  }
  return ADMIN_ROLES.includes(role);
}

export const PUBLIC_COMMAND_PREFIX = "/v1/commands/";
export const PUBLIC_ERROR_SHAPE = ["code", "message", "correlation_id", "retryable"] as const;

export function usesPublicContracts(path: string): boolean {
  return path.startsWith("/v1/");
}
