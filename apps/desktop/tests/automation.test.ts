import { describe, expect, it } from "vitest";

import { prefers } from "../src/automation/preferences.js";

describe("notification preferences", () => {
  it("honours channel selection", () => {
    expect(prefers({ channels: ["in_app", "email"] }, "desktop")).toBe(false);
    expect(prefers({ channels: ["in_app", "email"] }, "email")).toBe(true);
  });
});
