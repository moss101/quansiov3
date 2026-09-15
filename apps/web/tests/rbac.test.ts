import { describe, expect, it } from "vitest";

import { canOpen, usesPublicContracts } from "../src/rbac.js";

describe("web RBAC", () => {
  it("enforces admin-only pages from the server role", () => {
    expect(canOpen("admin", "member")).toBe(false);
    expect(canOpen("admin", "admin")).toBe(true);
    expect(canOpen("approvals", "member")).toBe(true);
    expect(canOpen("capability-admin", "viewer")).toBe(false);
  });

  it("uses the same public /v1 contracts as desktop", () => {
    expect(usesPublicContracts("/v1/commands/ApproveEffect")).toBe(true);
    expect(usesPublicContracts("/host/privileged")).toBe(false);
  });
});
