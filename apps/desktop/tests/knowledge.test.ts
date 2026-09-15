import { describe, expect, it } from "vitest";

import {
  activate,
  deleteEntry,
  isRecoveryField,
  retrievalIndex,
  type KnowledgeEntry,
} from "../src/knowledge/controls.js";

describe("knowledge controls", () => {
  it("removes deleted memory from future retrieval", () => {
    const entries: KnowledgeEntry[] = [
      { id: "mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", status: "active", kind: "memory" },
      { id: "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", status: "active", kind: "knowledge" },
    ];
    const after = deleteEntry(entries, "mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(retrievalIndex(after)).toEqual(["kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"]);
    expect(isRecoveryField("checkpoint_id")).toBe(true);
    expect(isRecoveryField("content")).toBe(false);
  });

  it("refuses to activate an unapproved skill", () => {
    expect(() => activate({ id: "sklv_1", status: "DRAFT" })).toThrow(/unapproved/);
    expect(activate({ id: "sklv_2", status: "APPROVED" }).status).toBe("ACTIVE");
  });
});
