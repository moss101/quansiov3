/**
 * APP-007 unit tests: the single-session rule (a takeover, a handback, a reconnect and a
 * control event for another session all leave the session id alone), the fence the page
 * shows, and the terminal replay from the durable cursor (in-order chunks, trimmed
 * overlap, reported gaps, resume position).
 */

import { describe, expect, it } from "vitest";

import {
  TerminalReplay,
  agentInputAllowed,
  applyLiveEvent,
  computerCommand,
  decodeBrowserFrame,
  decodeLiveSession,
  decodeTargets,
  decodeTerminalBytes,
  handback,
  handbackAllowed,
  isSameSession,
  latestFrame,
  pauseReasonOf,
  replayPosition,
  requestTakeover,
  takeoverAllowed,
} from "../src/index.js";

const SESSION = "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const OTHER = "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAB";
const TARGET = "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const TERMINAL = "tsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

function projection(overrides: Record<string, unknown> = {}, automation: Record<string, unknown> = {}): unknown {
  return {
    session: {
      id: SESSION,
      target_id: TARGET,
      run_id: RUN,
      control_holder: "agent",
      control_since: "2026-09-21T10:00:00Z",
      current_url: "https://example.com/",
      tabs: [{ target_id: "cdp-1", url: "https://example.com/", title: "Example Domain" }],
      screencast: { enabled: true, stream_ref: `${SESSION}:screencast` },
      status: "active",
      agent_may_drive: true,
      ...overrides,
    },
    target: { id: TARGET, workspace_id: "ws_x", substrate: "local_capsule_macos", status: "READY" },
    run: { id: RUN, status: "RUNNING", generation: 1, browser_control: null },
    terminal_sessions: [{ id: TERMINAL, target_id: TARGET, run_id: RUN, pty_ref: "pty:1", cursor: 128, status: "open", last_command_id: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }],
    computer_control: { target_id: TARGET, run_id: RUN, holder: "agent", generation: 1, takeover_pending: false, active_tool_call_id: null, agent_may_input: true },
    automation: { paused: false, reason: null, agent_input_fenced: false, run_status: "RUNNING", ...automation },
  };
}

const b64 = (text: string): string => Buffer.from(text, "utf8").toString("base64");

describe("live session (single-session rule)", () => {
  it("decodes the projection and offers only the legal control", () => {
    const session = decodeLiveSession(projection());
    expect(session.sessionId).toBe(SESSION);
    expect(session.controller).toBe("agent");
    expect(session.terminals[0]?.cursor).toBe(128);
    expect(session.computerControl?.agentMayInput).toBe(true);
    expect(agentInputAllowed(session)).toBe(true);
    expect(takeoverAllowed(session)).toBe(true);
    expect(handbackAllowed(session)).toBe(false);
    expect(pauseReasonOf(session)).toBeNull();
  });

  it("a takeover applied from the server keeps the id and fences the agent", () => {
    const before = decodeLiveSession(projection());
    const after = decodeLiveSession(projection({ control_holder: "user", status: "paused_takeover", agent_may_drive: false }, { paused: true, reason: "user_control", agent_input_fenced: true, run_status: "WAITING_TAKEOVER" }));
    expect(isSameSession(before, after)).toBe(true);
    expect(agentInputAllowed(after)).toBe(false);
    expect(takeoverAllowed(after)).toBe(false);
    expect(handbackAllowed(after)).toBe(true);
    expect(pauseReasonOf(after)).toBe("user_control");
  });

  it("a policy pause fences the agent and offers no handback", () => {
    const paused = decodeLiveSession(projection({ control_holder: "none", status: "paused_policy", agent_may_drive: false }, { paused: true, reason: "policy", agent_input_fenced: true }));
    expect(agentInputAllowed(paused)).toBe(false);
    expect(takeoverAllowed(paused)).toBe(false);
    expect(handbackAllowed(paused)).toBe(false);
    expect(pauseReasonOf(paused)).toBe("policy");
  });

  it("applies control events only for this session, so no replacement can slip in", () => {
    const session = decodeLiveSession(projection());
    const foreign = applyLiveEvent(session, { type: "browser.control_changed", payload: { session_id: OTHER, holder: "user", status: "paused_takeover" } });
    expect(foreign).toBe(session);
    const taken = applyLiveEvent(session, { type: "browser.control_changed", payload: { session_id: SESSION, holder: "user", status: "paused_takeover", since: "2026-09-21T10:01:00Z" } });
    expect(taken.sessionId).toBe(SESSION);
    expect(taken.controller).toBe("user");
    expect(agentInputAllowed(taken)).toBe(false);
    expect(handbackAllowed(taken)).toBe(true);
    const parked = applyLiveEvent(taken, { type: "run.waiting", payload: { run_id: RUN, from: "RUNNING", to: "WAITING_TAKEOVER", generation: 1 } });
    expect(parked.run?.status).toBe("WAITING_TAKEOVER");
    const back = applyLiveEvent(parked, { type: "browser.control_changed", payload: { session_id: SESSION, holder: "agent", status: "active" } });
    expect(back.sessionId).toBe(SESSION);
    expect(agentInputAllowed(back)).toBe(true);
    const resumed = applyLiveEvent(back, { type: "run.resumed", payload: { run_id: RUN, from: "WAITING_TAKEOVER", to: "RUNNING", generation: 1 } });
    expect(resumed.run?.status).toBe("RUNNING");
    // The native computer's control changes the control row, never the browser session.
    const computer = applyLiveEvent(resumed, { type: "target.control_changed", payload: { target_id: TARGET, holder: "user", generation: 2, takeover_pending: false } });
    expect(computer.computerControl?.holder).toBe("user");
    expect(computer.computerControl?.agentMayInput).toBe(false);
    expect(computer.controller).toBe("agent");
  });

  it("builds the two commands over a bsn_ session or a tgt_ target and refuses anything else", () => {
    expect(requestTakeover(SESSION)).toEqual({ name: "RequestTakeover", params: { session_id: SESSION } });
    expect(handback(SESSION)).toEqual({ name: "Handback", params: { session_id: SESSION } });
    expect(computerCommand("RequestTakeover", TARGET)).toEqual({ name: "RequestTakeover", params: { target_id: TARGET } });
    expect(() => requestTakeover(TARGET)).toThrow(/bsn_/);
    expect(() => computerCommand("Handback", SESSION)).toThrow(/tgt_/);
  });

  it("decodes the target list with its sessions", () => {
    const targets = decodeTargets({
      items: [{ id: TARGET, workspace_id: "ws_x", substrate: "local_capsule_macos", status: "READY", browser_sessions: [{ id: SESSION, status: "active", control_holder: "agent", current_url: "https://example.com/" }], terminal_sessions: [{ id: TERMINAL, cursor: "128", status: "open" }], computer_control: null }],
    });
    expect(targets[0]?.browserSessions[0]?.id).toBe(SESSION);
    expect(targets[0]?.terminals[0]?.cursor).toBe(128);
    expect(targets[0]?.computerControl).toBeNull();
  });
});

describe("browser frames", () => {
  it("accepts a frame for this session only and keeps the newest", () => {
    const frame = decodeBrowserFrame({ kind: "browser.frame", session_id: SESSION, data: b64("jpeg"), media_type: "image/jpeg", url: "https://example.com/a", cursor: { x: 10, y: 20, action: "click" }, sequence: 2 }, SESSION);
    expect(frame?.dataUrl.startsWith("data:image/jpeg;base64,")).toBe(true);
    expect(frame?.cursor).toEqual({ x: 10, y: 20, action: "click" });
    expect(decodeBrowserFrame({ kind: "browser.frame", session_id: OTHER, data: b64("x") }, SESSION)).toBeNull();
    expect(decodeBrowserFrame({ kind: "browser.frame", session_id: SESSION, data: "<script>" }, SESSION)).toBeNull();
    const older = decodeBrowserFrame({ kind: "browser.frame", session_id: SESSION, data: b64("old"), sequence: 1 }, SESSION);
    expect(frame && older ? latestFrame(frame, older) : null).toBe(frame);
    expect(frame && older ? latestFrame(older, frame) : null).toBe(frame);
  });
});

describe("terminal replay from the durable cursor", () => {
  it("renders in-order chunks, trims a re-sent overlap and refuses a gap", () => {
    const replay = new TerminalReplay(TERMINAL, 128);
    expect(replay.cursor).toBe(128);
    expect(replay.accept({ kind: "terminal.bytes", session_id: TERMINAL, from: 128, bytes: b64("$ ls\n") })).toEqual({ kind: "appended", bytes: 5, trimmed: 0 });
    expect(replay.cursor).toBe(133);
    // The conduit re-sends from 130 after a reconnect: the three bytes already shown are trimmed.
    expect(replay.accept({ kind: "terminal.bytes", session_id: TERMINAL, from: 130, bytes: b64("ls\nREADME\n") })).toEqual({ kind: "appended", bytes: 7, trimmed: 3 });
    expect(replay.text()).toBe("$ ls\nREADME\n");
    expect(replay.cursor).toBe(140);
    expect(replay.accept({ kind: "terminal.bytes", session_id: TERMINAL, from: 100, bytes: b64("old") })).toEqual({ kind: "duplicate" });
    expect(replay.accept({ kind: "terminal.bytes", session_id: TERMINAL, from: 150, bytes: b64("late") })).toEqual({ kind: "gap", expected: 140, received: 150 });
    expect(replay.gaps).toBe(1);
    expect(replay.text()).toBe("$ ls\nREADME\n");
    expect(replay.accept({ kind: "terminal.bytes", session_id: "tsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAB", from: 140, bytes: b64("x") })).toEqual({ kind: "other-session" });
    expect(replay.cursor).toBe(140);
  });

  it("resumes from the later of the local offset and the durable cursor", () => {
    expect(replayPosition(140, 128)).toEqual({ from: 140, lost: 0 });
    expect(replayPosition(140, 200)).toEqual({ from: 200, lost: 60 });
    const replay = new TerminalReplay(TERMINAL, 0);
    replay.accept({ kind: "terminal.bytes", session_id: TERMINAL, from: 0, bytes: b64("abc") });
    expect(replay.resume(2)).toEqual({ from: 3, lost: 0 });
    expect(replay.resume(10)).toEqual({ from: 10, lost: 7 });
    expect(replay.cursor).toBe(10);
    expect(replay.accept({ kind: "terminal.bytes", session_id: TERMINAL, from: 10, bytes: b64("d") }).kind).toBe("appended");
    expect(replay.text()).toBe("abcd");
  });

  it("decodes only well-formed terminal frames", () => {
    expect(decodeTerminalBytes({ kind: "terminal.bytes", session_id: TERMINAL, from: "12", bytes: "AA==" })).toEqual({ kind: "terminal.bytes", session_id: TERMINAL, from: 12, bytes: "AA==" });
    expect(decodeTerminalBytes({ kind: "terminal.bytes", session_id: TERMINAL, from: -1, bytes: "AA==" })).toBeNull();
    expect(decodeTerminalBytes({ kind: "model.delta", text: "x" })).toBeNull();
    expect(() => new TerminalReplay(SESSION, 0)).toThrow(/tsn_/);
  });
});
