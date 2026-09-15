import { describe, expect, it } from "vitest";

import { receivesProtected } from "../src/collaboration/participants.js";

describe("collaboration", () => {
  it("hides protected events from removed participants", () => {
    expect(receivesProtected({ id: "usr_1", status: "active" })).toBe(true);
    expect(receivesProtected({ id: "usr_2", status: "removed" })).toBe(false);
  });
});
