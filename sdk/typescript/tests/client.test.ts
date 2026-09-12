import { describe, expect, it } from "vitest";

import { PUBLIC_API_VERSION, publicApiPath } from "../src/index.js";

describe("public API v1 paths", () => {
  it("version-prefixes every resource", () => {
    expect(PUBLIC_API_VERSION).toBe("v1");
    expect(publicApiPath("runs")).toBe("/v1/runs");
    expect(publicApiPath("workspaces/ws_1/artifacts")).toBe("/v1/workspaces/ws_1/artifacts");
  });

  it("fails closed on a malformed or unversioned resource", () => {
    for (const bad of ["", "/runs", "Runs", "runs?x=1", "a".repeat(129), "../admin"]) {
      expect(() => publicApiPath(bad)).toThrow(/invalid public API resource/);
    }
  });
});
