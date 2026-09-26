/**
 * APP-012: the command palette's matching — every typed word against label and detail, order kept.
 */

import { describe, expect, it } from "vitest";

import { matchItems, type PaletteItem } from "../src/app/Palette.js";

describe("palette matching", () => {
  const item = (label: string, detail: string | null = null): PaletteItem => ({ key: label, group: "g", label, detail, icon: null, run: () => undefined });
  const items = [item("Work review"), item("Audit & privacy"), item("e2e seed", "admin"), item("Sign out")];

  it("keeps everything for an empty query and filters case-insensitively by every word", () => {
    expect(matchItems(items, "  ").map((entry) => entry.label)).toEqual(["Work review", "Audit & privacy", "e2e seed", "Sign out"]);
    expect(matchItems(items, "AUDIT").map((entry) => entry.label)).toEqual(["Audit & privacy"]);
    expect(matchItems(items, "seed admin").map((entry) => entry.label)).toEqual(["e2e seed"]);
    expect(matchItems(items, "seed viewer")).toEqual([]);
  });
});
