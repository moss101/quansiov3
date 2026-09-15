import { describe, expect, it } from "vitest";

import { matchesCanonical, project, proposeEdit, type GraphRevision } from "../src/work/monitor.js";

const base: GraphRevision = {
  revision: 3,
  nodes: ["wn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"],
  agents: ["ath_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"],
  handoffs: [],
  blockers: [],
  completionContract: "tests-green",
};

describe("work monitor", () => {
  it("projects the canonical revision", () => {
    const view = project(base);
    expect(matchesCanonical(view, base)).toBe(true);
  });

  it("refuses a concurrent overwrite with CONFLICT_REVISION", () => {
    const stale = { ...base, revision: 2 };
    const result = proposeEdit(stale, base, "wn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.code).toBe("CONFLICT_REVISION");
      expect(result.server.revision).toBe(3);
    }
    const fresh = proposeEdit(base, base, "wn_01J8Z3K6F1N8VQ2X5W9Y0BBBBB");
    expect(fresh.ok).toBe(true);
  });
});
