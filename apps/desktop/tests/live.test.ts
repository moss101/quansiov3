import { describe, expect, it } from "vitest";

import {
  agentInputAllowed,
  isSameSession,
  reconnect,
  takeover,
  type LiveSession,
} from "../src/live/session.js";

const session: LiveSession = {
  sessionId: "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
  kind: "browser",
  controller: "agent",
  paused: false,
  terminalCursor: "0",
};

describe("live session", () => {
  it("does not replace the session on takeover and fences agent input", () => {
    const owned = takeover(session);
    expect(isSameSession(session, owned)).toBe(true);
    expect(owned.controller).toBe("user");
    expect(agentInputAllowed(owned)).toBe(false);
    const restored = reconnect(owned);
    expect(restored.sessionId).toBe(session.sessionId);
    expect(agentInputAllowed(restored)).toBe(false);
  });
});
