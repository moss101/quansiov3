import { describe, expect, it } from "vitest";

import { archiveInRoster, type RosterEntry } from "../src/teammates/roster.js";

describe("teammate roster", () => {
  it("archives without dropping the teammate identity", () => {
    const roster: RosterEntry[] = [
      {
        id: "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        name: "Ada",
        status: "active",
        activeRunId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      },
    ];
    const after = archiveInRoster(roster, "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(after[0]?.id).toBe("agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(after[0]?.status).toBe("archived");
  });
});
