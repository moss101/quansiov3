import { describe, expect, it } from "vitest";

import { approve, isUnresolved, preview } from "../src/trust/approvals.js";

describe("approval and effect timeline", () => {
  it("cannot approve a payload that changed after preview", () => {
    const shown = preview("apr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", { action: "delete", id: "1" });
    expect(approve(shown, { action: "delete", id: "1" }).ok).toBe(true);
    const changed = approve(shown, { action: "delete", id: "2" });
    expect(changed.ok).toBe(false);
    if (!changed.ok) {
      expect(changed.code).toBe("APPROVAL_SUPERSEDED");
    }
  });

  it("keeps unknown effects visibly unresolved", () => {
    expect(isUnresolved({ id: "eff_1", status: "OUTCOME_UNKNOWN" })).toBe(true);
    expect(isUnresolved({ id: "eff_2", status: "RECONCILING" })).toBe(true);
    expect(isUnresolved({ id: "eff_3", status: "SETTLED_SUCCESS" })).toBe(false);
  });
});
