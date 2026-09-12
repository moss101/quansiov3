import { describe, expect, it } from "vitest";

import { DESKTOP_SURFACES, surface } from "../src/surfaces.js";

// DOSSIER.md §15: the Desktop product MUST make these first-class.
const REQUIRED = [
  "conversation",
  "objectives",
  "teammates",
  "plan-and-agents",
  "run-status",
  "approvals",
  "evidence-timeline",
  "live-browser-terminal",
  "artifacts",
  "knowledge-controls",
  "collaboration",
  "automations",
  "connectors",
  "capability-admin",
  "model-usage",
] as const;

describe("desktop surfaces", () => {
  it("declares every first-class surface exactly once", () => {
    const ids = DESKTOP_SURFACES.map((s) => s.id);
    expect(ids).toEqual([...REQUIRED]);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("marks consequential surfaces as requiring approval", () => {
    expect(surface("approvals").requiresApproval).toBe(true);
    expect(surface("live-browser-terminal").requiresApproval).toBe(true);
    expect(surface("conversation").requiresApproval).toBe(false);
  });

  it("rejects unknown surfaces", () => {
    // @ts-expect-error surface() is typed to known ids; runtime guard must still reject.
    expect(() => surface("nope")).toThrow(/unknown desktop surface/);
  });
});
