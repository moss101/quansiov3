import { describe, expect, it } from "vitest";

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { GENERATED_OPERATIONS, PUBLIC_API_VERSION, publicApiPath } from "../src/index.js";

describe("public API v1 paths", () => {
  it("version-prefixes every resource", () => {
    expect(PUBLIC_API_VERSION).toBe("v1");
    expect(publicApiPath("runs")).toBe("/v1/runs");
    expect(publicApiPath("workspaces/ws_1/artifacts")).toBe("/v1/workspaces/ws_1/artifacts");
  });

  it("exposes generated OpenAPI operation ids", () => {
    const openapi = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../../../schemas/openapi/public-api-v1.yaml"),
      "utf8",
    );
    for (const name of GENERATED_OPERATIONS) {
      expect(openapi).toContain(`operationId: ${name}`);
    }
  });

  it("fails closed on a malformed or unversioned resource", () => {
    for (const bad of ["", "/runs", "Runs", "runs?x=1", "a".repeat(129), "../admin"]) {
      expect(() => publicApiPath(bad)).toThrow(/invalid public API resource/);
    }
  });
});
