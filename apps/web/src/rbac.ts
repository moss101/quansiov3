/**
 * Roles and what they allow (APP-012, DOMAIN.md §2).
 *
 * The server decides; this module is the page's mirror of `crates/server/src/policy/rbac.rs`
 * used only to decide what to *offer* (which surfaces to list, which buttons to render).
 * A refusal the mirror missed still comes back from the server as `SCOPE_FORBIDDEN` with
 * `details.kind = "rbac"`, and the page shows it. The mirror can never widen anything: it
 * has no way to reach a projection or command that the server would not also gate.
 */

export type TenantRole = "owner" | "admin" | "billing" | "member";
export type WorkspaceRole = "admin" | "editor" | "approver" | "viewer";

export const TENANT_ROLES: readonly TenantRole[] = ["owner", "admin", "billing", "member"];
export const WORKSPACE_ROLES: readonly WorkspaceRole[] = ["admin", "editor", "approver", "viewer"];

/** The action families the server gates (`ActionFamily`, spelled as its refusals spell them). */
export type Action = "read" | "author_work" | "approve_effect" | "manage_workspace" | "admin_ops" | "manage_tenant" | "view_usage";

export interface Roles {
  readonly tenant: TenantRole | null;
  readonly workspace: WorkspaceRole | null;
}

export function tenantRole(value: unknown): TenantRole | null {
  return typeof value === "string" && (TENANT_ROLES as readonly string[]).includes(value) ? (value as TenantRole) : null;
}

export function workspaceRole(value: unknown): WorkspaceRole | null {
  return typeof value === "string" && (WORKSPACE_ROLES as readonly string[]).includes(value) ? (value as WorkspaceRole) : null;
}

/** Tenant owner/admin inherit workspace admin on every workspace (DOMAIN.md §2). */
export function effectiveWorkspaceRole(roles: Roles): WorkspaceRole | null {
  if (roles.tenant === "owner" || roles.tenant === "admin") {
    return "admin";
  }
  return roles.workspace;
}

/** Whether `roles` allow `action` — the same table as the server's `rbac::check`. */
export function allows(roles: Roles, action: Action): boolean {
  const workspace = effectiveWorkspaceRole(roles);
  switch (action) {
    case "manage_tenant":
      return roles.tenant === "owner" || roles.tenant === "admin";
    case "view_usage":
      return roles.tenant !== null && roles.tenant !== "member";
    case "read":
      return workspace !== null;
    case "author_work":
      return workspace === "admin" || workspace === "editor";
    case "approve_effect":
      return workspace === "admin" || workspace === "approver";
    case "manage_workspace":
    case "admin_ops":
      return workspace === "admin";
    default:
      return false;
  }
}

/** The roles `GET /v1/me` reports for the session's tenant and a chosen workspace. */
export function rolesFromMe(me: unknown, workspaceId: string | null): Roles {
  const record = me && typeof me === "object" ? (me as Record<string, unknown>) : {};
  const session = record["session"] && typeof record["session"] === "object" ? (record["session"] as Record<string, unknown>) : {};
  const tenantId = typeof session["tenant_id"] === "string" ? session["tenant_id"] : null;
  const tenants = Array.isArray(record["tenants"]) ? (record["tenants"] as Record<string, unknown>[]) : [];
  const workspaces = Array.isArray(record["workspaces"]) ? (record["workspaces"] as Record<string, unknown>[]) : [];
  const tenant = tenants.find((entry) => entry["id"] === tenantId);
  const workspace = workspaceId ? workspaces.find((entry) => entry["id"] === workspaceId) : undefined;
  return { tenant: tenantRole(tenant?.["role"]), workspace: workspaceRole(workspace?.["role"]) };
}

export const PUBLIC_COMMAND_PREFIX = "/v1/commands/";
export const PUBLIC_ERROR_SHAPE = ["code", "message", "correlation_id", "retryable"] as const;

/** Every path the web app addresses is a public contract. */
export function usesPublicContracts(path: string): boolean {
  return path.startsWith("/v1/");
}
