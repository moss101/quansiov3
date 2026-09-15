import { describe, expect, it } from "vitest";

import {
  addVersion,
  afterRunClosed,
  previewExecutesWithHostPrivileges,
  previewIsSafe,
  previewMode,
  type ArtifactRecord,
} from "../src/artifacts/workspace.js";

function sample(): ArtifactRecord {
  return {
    id: "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    title: "launch-plan",
    kind: "document",
    runId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    versions: [
      {
        id: "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        seq: 1,
        contentDigest: "abc",
        mediaType: "text/markdown",
      },
    ],
    sourceEvidenceIds: ["evd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"],
  };
}

describe("artifact workspace", () => {
  it("keeps artifacts after the producing run closes and records versions", () => {
    const closed = afterRunClosed(sample());
    expect(closed.id).toBe(sample().id);
    expect(closed.versions).toHaveLength(1);
    const v2 = addVersion(closed, {
      id: "artv_01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
      seq: 2,
      contentDigest: "def",
      mediaType: "text/markdown",
    });
    expect(v2.versions.map((v) => v.seq)).toEqual([1, 2]);
  });

  it("never previews HTML or JavaScript with host privileges", () => {
    expect(previewMode("text/html")).toBe("safe-text");
    expect(previewMode("application/javascript")).toBe("safe-text");
    expect(previewMode("image/svg+xml")).toBe("safe-text");
    expect(previewIsSafe("text/html")).toBe(true);
    expect(previewExecutesWithHostPrivileges("text/html")).toBe(false);
    expect(previewMode("image/png")).toBe("safe-image");
  });
});
