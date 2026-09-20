/**
 * APP-012: the page-side mirror of the server's RBAC offers exactly what DOMAIN.md §2
 * allows — never more — and reads roles from `/v1/me` as the server reports them.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { TENANT_ROLES, WORKSPACE_ROLES, WEB_SURFACES, adminSurfaces, allows, canOpen, effectiveWorkspaceRole, offered, rolesFromMe, surface, usesPublicContracts, type Action, type Roles } from "../src/index.js";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const ACTIONS: readonly Action[] = ["read", "author_work", "approve_effect", "manage_workspace", "admin_ops", "manage_tenant", "view_usage"];

describe("roles", () => {
  it("tenant owner/admin inherit workspace admin; billing sees usage only; a missing role denies", () => {
    for (const tenant of [...TENANT_ROLES, null]) {
      for (const workspace of [...WORKSPACE_ROLES, null]) {
        const roles: Roles = { tenant, workspace };
        const effective = effectiveWorkspaceRole(roles);
        const inherits = tenant === "owner" || tenant === "admin";
        expect(effective).toBe(inherits ? "admin" : workspace);
        expect(allows(roles, "manage_tenant")).toBe(inherits);
        expect(allows(roles, "view_usage")).toBe(tenant !== null && tenant !== "member");
        expect(allows(roles, "read")).toBe(effective !== null);
        expect(allows(roles, "author_work")).toBe(effective === "admin" || effective === "editor");
        expect(allows(roles, "approve_effect")).toBe(effective === "admin" || effective === "approver");
        expect(allows(roles, "manage_workspace")).toBe(effective === "admin");
        expect(allows(roles, "admin_ops")).toBe(effective === "admin");
      }
    }
    // Nothing is allowed to nobody.
    for (const action of ACTIONS) {
      expect(allows({ tenant: null, workspace: null }, action)).toBe(false);
    }
  });

  it("reads the session tenant's role and the chosen workspace's role from /v1/me", () => {
    const me = {
      session: { id: "ses_1", tenant_id: "tn_1" },
      tenants: [{ id: "tn_0", role: "owner" }, { id: "tn_1", role: "member" }],
      workspaces: [{ id: "ws_1", role: "approver" }, { id: "ws_2", role: "viewer" }],
    };
    expect(rolesFromMe(me, "ws_1")).toEqual({ tenant: "member", workspace: "approver" });
    expect(rolesFromMe(me, "ws_2")).toEqual({ tenant: "member", workspace: "viewer" });
    expect(rolesFromMe(me, "ws_9")).toEqual({ tenant: "member", workspace: null });
    expect(rolesFromMe(me, null)).toEqual({ tenant: "member", workspace: null });
    expect(rolesFromMe({ session: { tenant_id: "tn_1" }, tenants: [{ id: "tn_1", role: "emperor" }] }, null)).toEqual({ tenant: null, workspace: null });
    expect(rolesFromMe(null, null)).toEqual({ tenant: null, workspace: null });
  });
});

describe("surfaces", () => {
  it("are all server-only, name the action the server gates them by, and use only catalogued contracts", () => {
    const commands = readFileSync(join(REPO, "schemas", "catalog", "commands.yaml"), "utf8");
    const projections = readFileSync(join(REPO, "schemas", "catalog", "read-projections.yaml"), "utf8");
    for (const descriptor of WEB_SURFACES) {
      expect(descriptor.serverOnly).toBe(true);
      expect(descriptor.adminOnly).toBe(descriptor.action === "manage_workspace" || descriptor.action === "manage_tenant" || descriptor.action === "view_usage");
      for (const projection of descriptor.projections) {
        expect(usesPublicContracts(projection)).toBe(true);
        const catalogued = projection.replace(/\?.*$/, "?q=").replace(/\/v1\/runs\/\{id\}$/, "/v1/runs/{id}");
        expect(projections.includes(`- ${catalogued}`) || projections.includes(`- ${projection}`) || projection === "/v1/me", `${projection} is a catalogued projection`).toBe(true);
      }
      for (const command of descriptor.commands) {
        expect(commands.includes(`- ${command}\n`), `${command} is a catalogued command`).toBe(true);
      }
    }
    expect(adminSurfaces()).toEqual(["policies", "connectors", "users", "capabilities", "audit-and-privacy", "usage"]);
    expect(() => surface("nope" as never)).toThrow(/unknown web surface/);
  });

  it("offers a viewer the review surfaces only, an approver the same, an owner everything", () => {
    const viewer: Roles = { tenant: "member", workspace: "viewer" };
    const approver: Roles = { tenant: "member", workspace: "approver" };
    const billing: Roles = { tenant: "billing", workspace: null };
    const owner: Roles = { tenant: "owner", workspace: null };
    expect(offered(viewer).map((item) => item.id)).toEqual(["work-review", "approvals", "artifacts"]);
    expect(offered(approver).map((item) => item.id)).toEqual(["work-review", "approvals", "artifacts"]);
    expect(offered(billing).map((item) => item.id)).toEqual(["usage"]);
    expect(offered(owner).map((item) => item.id)).toEqual(["work-review", "approvals", "artifacts", "policies", "connectors", "users", "capabilities", "audit-and-privacy", "usage"]);
    expect(canOpen("audit-and-privacy", approver)).toBe(false);
    expect(canOpen("audit-and-privacy", { tenant: "admin", workspace: null })).toBe(true);
    expect(canOpen("sign-in", { tenant: null, workspace: null })).toBe(true);
  });
});
