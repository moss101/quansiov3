import { describe, expect, it } from "vitest";

import { WEB_SURFACES, adminSurfaces } from "../src/surfaces.js";

describe("web surfaces", () => {
  it("never uses a privileged local-host API", () => {
    for (const s of WEB_SURFACES) {
      expect(s.serverOnly).toBe(true);
    }
  });

  it("keeps governance surfaces admin-only", () => {
    expect(adminSurfaces()).toEqual(["audit-and-privacy", "usage", "admin", "capability-admin"]);
  });

  it("exposes approvals to non-admin reviewers", () => {
    expect(WEB_SURFACES.find((s) => s.id === "approvals")?.adminOnly).toBe(false);
  });
});
