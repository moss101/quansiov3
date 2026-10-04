/**
 * Skill recording on the desktop (ADP-003, DOMAIN.md §11.7): the page's decoders and refusals, the bridge's
 * recording channels, and the places a recording could be finished by something other than the person.
 */

import { mkdtempSync, readFileSync, readdirSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

import {
  IDLE_RECORDER,
  PRELOAD_ALLOWLIST,
  RECORDING_COMMANDS,
  canBeConstant,
  canCompile,
  canFinish,
  decodeRecorder,
  decodeRecordings,
  finishRequest,
  isCapturing,
  submission,
  undecided,
} from "../src/index.js";
import type { ProposalView, RecordingView } from "../src/index.js";
import { DesktopBridge } from "../src/main/ipc.js";
import { SessionFile } from "../src/main/session-file.js";
import { UpdateHooks } from "../src/main/updates.js";

const REC = "rec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

function proposal(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    schema_version: "v1",
    trust: { source: "recording", level: "UNTRUSTED_EXTERNAL", recording_id: REC, injection_suspected: [] },
    manifest: {
      instructions: "Follow procedure.",
      tool_needs: ["computer.click", "computer.read", "computer.type"],
      capability_needs: ["read.internal", "computer.input.privileged"],
      procedure: {
        schema_version: "v1",
        steps: [
          { id: "s1", intent: 'Press "New Note" in Notes', path: "ui", ui: { bundle_id: "com.apple.Notes", action: "press" } },
          { id: "s2", intent: "Enter the note body", path: "ui", needs_review: true, ui: { bundle_id: "com.apple.Notes", action: "set_value", slot_id: "slot_1" } },
        ],
        preconditions: [{ kind: "app_installed" }],
        postconditions: [{ kind: "evidence_required" }],
        recovery: [{ on: "any", action: "reobserve_then_ask" }],
      },
    },
    slots: [
      { slot_id: "slot_1", name: "note_body", description: "Value entered in Note body", type: "text", example: "Quarterly report", classification: null, control: { ax_role: "AXTextArea", ax_title: "Note body" } },
      { slot_id: "slot_2", name: "password", description: "x", type: "text", example: "hunter2", classification: null, control: { ax_role: "AXTextField", ax_title: "Password" } },
    ],
    questions: [{ slot_id: "slot_1", question: "constant or input?" }],
    ...overrides,
  };
}

function wire(status = "compiled", withProposal = true): unknown {
  return {
    id: REC,
    workspace_id: WORKSPACE,
    status,
    started_at: "2026-10-04T10:00:00Z",
    expires_at: "2026-10-04T10:30:00Z",
    expired: false,
    redaction: { rule_set: "v1", counts: { api_key: 2, bearer_token: 0, card_number: 1, verification_code: 0, password_field: 3 }, screenshots_withheld: 1 },
    stats: { actions: 4, screenshots: 2, transcript_segments: 0, paused_ms: 4000 },
    has_proposal: withProposal,
    skill_version_id: null,
    abort_reason: null,
    proposal: withProposal ? proposal() : null,
  };
}

function proposalOf(recording: RecordingView): ProposalView {
  if (!recording.proposal) {
    throw new Error("the fixture has no proposal");
  }
  return recording.proposal;
}

function view(status = "compiled"): RecordingView {
  const decoded = decodeRecordings({ recordings: [wire(status)] })[0];
  if (!decoded) {
    throw new Error("the fixture did not decode");
  }
  return decoded;
}

describe("decoding what the server says", () => {
  it("decodes the redaction summary as counts and the proposal with its untrusted origin", () => {
    const recording = view();
    expect(recording.redaction).toEqual({
      ruleSet: "v1",
      counts: { api_key: 2, bearer_token: 0, card_number: 1, verification_code: 0, password_field: 3 },
      screenshotsWithheld: 1,
      total: 7,
    });
    expect(recording.actions).toBe(4);
    expect(recording.proposal?.slots.map((slot) => slot.slotId)).toEqual(["slot_1", "slot_2"]);
    expect(recording.proposal?.steps.find((step) => step.id === "s2")?.needsReview).toBe(true);
    expect(recording.proposal?.slots.map((slot) => slot.classification)).toEqual([null, null]);
  });

  it("refuses a malformed row, an unknown status and a proposal that does not carry its origin", () => {
    expect(decodeRecordings({ recordings: [{ id: "run_1", status: "recording" }, { id: REC, status: "done" }, null, 5] })).toEqual([]);
    const forged = decodeRecordings({ recordings: [{ ...(wire() as object), proposal: proposal({ trust: { source: "model", level: "TRUSTED_USER" } }) }] });
    expect(forged[0]?.proposal).toBeNull();
    expect(decodeRecordings({}).length).toBe(0);
  });

  it("decodes the recorder's own state, and an unknown state is idle", () => {
    expect(decodeRecorder({ state: "recording", recordingId: REC, actions: 3, screenshots: 1, counts: { api_key: 1 } })).toMatchObject({ state: "recording", actions: 3 });
    expect(decodeRecorder({ state: "bogus" })).toEqual(IDLE_RECORDER);
    expect(decodeRecorder(null)).toEqual(IDLE_RECORDER);
  });
});

describe("the indicator and the finish", () => {
  it("shows while the recorder is starting, recording or saving, and never while idle", () => {
    expect(isCapturing(IDLE_RECORDER)).toBe(false);
    for (const state of ["starting", "recording", "finishing"] as const) {
      expect(isCapturing({ ...IDLE_RECORDER, state })).toBe(true);
    }
  });

  it("builds a finish request only from a genuine click", () => {
    expect(finishRequest({ isTrusted: true })).toEqual({ gesture: true });
    expect(finishRequest({ isTrusted: false })).toEqual({ gesture: false });
    expect(canFinish({ ...IDLE_RECORDER, state: "recording" })).toBe(true);
    expect(canFinish({ ...IDLE_RECORDER, state: "idle" })).toBe(false);
  });

  it("offers compile only for a finished or compiled recording", () => {
    expect(canCompile(view("finished"))).toBe(true);
    expect(canCompile(view("compiled"))).toBe(true);
    for (const status of ["recording", "submitted", "cancelled", "aborted"]) {
      expect(canCompile(view(status))).toBe(false);
    }
  });
});

describe("submission: every slot is the person's decision", () => {
  it("refuses until every slot is classified, naming what is open", () => {
    const recording = view();
    const open = submission(recording, "make-a-note", "1.0.0", { slot_1: "input" });
    expect(open).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(open.ok ? "" : open.reason).toContain("password");
    expect(undecided(proposalOf(recording), {}).map((slot) => slot.slotId)).toEqual(["slot_1", "slot_2"]);
  });

  it("refuses a bad name or version and a recording that is not compiled", () => {
    const decisions = { slot_1: "input", slot_2: "input" } as const;
    expect(submission(view(), "Not A Name", "1.0.0", decisions)).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(submission(view(), "ok-name", "1.0", decisions)).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(submission(view("finished"), "ok-name", "1.0.0", decisions)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
  });

  it("never lets a redacted value or a sensitive control become a constant", () => {
    const recording = view();
    const note = proposalOf(recording).slots.find((slot) => slot.slotId === "slot_1");
    const password = proposalOf(recording).slots.find((slot) => slot.slotId === "slot_2");
    if (!note || !password) {
      throw new Error("the fixture lost its slots");
    }
    expect(canBeConstant(note)).toBe(true);
    expect(canBeConstant(password)).toBe(false);
    expect(canBeConstant({ ...note, example: "[REDACTED:verification_code]" })).toBe(false);
    expect(submission(recording, "ok-name", "1.0.0", { slot_1: "constant", slot_2: "constant" })).toMatchObject({ ok: false });
  });

  it("builds the params from the decisions and nothing else: no epoch, no example", () => {
    const built = submission(view(), "make-a-note", "1.0.0", { slot_1: "constant", slot_2: "input" });
    expect(built).toEqual({
      ok: true,
      value: {
        recording_id: REC,
        name: "make-a-note",
        semver: "1.0.0",
        slots: [
          { slot_id: "slot_1", classification: "constant" },
          { slot_id: "slot_2", classification: "input" },
        ],
      },
    });
    expect(JSON.stringify(built)).not.toContain("epoch");
    expect(JSON.stringify(built)).not.toContain("Quarterly report");
  });
});

// ------------------------------------------------------------------ the bridge

const temporaryDirs: string[] = [];

interface Call {
  readonly method: string;
  readonly url: string;
  readonly body: Record<string, unknown> | null;
}

function bridge(
  calls: Call[],
  answer: (call: Call) => unknown = () => ({ command_id: "c", accepted_at: "now", replayed: false, result: {} }),
  status = 200,
): DesktopBridge {
  const dir = mkdtempSync(join(tmpdir(), "quansio-recording-"));
  temporaryDirs.push(dir);
  return new DesktopBridge({
    serverOrigin: "http://127.0.0.1:8080",
    auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
    session: new SessionFile(dir),
    openSocket: () => {
      throw new Error("no socket in this test");
    },
    updates: new UpdateHooks("stable", null, "http://x", false),
    consumer: "desktop:test",
    push: () => undefined,
    fetch: (url, init) => {
      const call: Call = { method: init?.method ?? "GET", url: url, body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null };
      calls.push(call);
      return Promise.resolve(new Response(JSON.stringify(answer(call)), { status, headers: { "content-type": "application/json" } }));
    },
  });
}

describe("the bridge's recording channels", () => {
  it("are allowlisted, handled, and the generic command path refuses the recording commands", async () => {
    for (const channel of ["recording.status", "recording.start", "recording.finish", "recording.cancel", "recording.list", "recording.compile", "recording.submit"]) {
      expect(PRELOAD_ALLOWLIST).toContain(channel);
    }
    const calls: Call[] = [];
    const handlers = bridge(calls).handlers();
    for (const name of RECORDING_COMMANDS) {
      await expect(handlers["commands.invoke"]({ name, commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params: {} })).rejects.toThrow(/recording channels/);
    }
    expect(calls).toEqual([]);
  });

  it("finish needs a click: without one the recorder is not even asked", async () => {
    const calls: Call[] = [];
    const handlers = bridge(calls).handlers();
    for (const payload of [{ gesture: false }, {}, { gesture: "true" }]) {
      expect(await handlers["recording.finish"](payload)).toMatchObject({ ok: false, code: "GESTURE_REQUIRED" });
    }
    expect(calls).toEqual([]);
  });

  it("a start with no recorder in this build says so and contacts no server", async () => {
    const calls: Call[] = [];
    const handlers = bridge(calls).handlers();
    expect(await handlers["recording.start"]({})).toMatchObject({ ok: false, code: "RECORDER_UNAVAILABLE" });
    expect(calls).toEqual([]);
    expect(await handlers["recording.status"]({})).toEqual(IDLE_RECORDER);
  });

  it("compile and submit carry the epoch main holds, never one the page sent", async () => {
    const calls: Call[] = [];
    const instance = bridge(calls);
    const handlers = instance.handlers();
    instance.recording.accountChanged();
    instance.recording.accountChanged();
    const epoch = instance.recording.epoch;
    expect(epoch).toBe(3);
    await handlers["recording.compile"]({ recordingId: REC, epoch: 99 });
    await handlers["recording.submit"]({ recordingId: REC, name: "make-a-note", semver: "1.0.0", slots: [{ slot_id: "slot_1", classification: "input" }], epoch: 99 });
    const [compile, submit] = calls;
    expect(compile?.url).toMatch(/CompileRecording$/);
    expect(compile?.body?.["params"]).toEqual({ recording_id: REC, epoch });
    expect(submit?.url).toMatch(/SubmitRecordingSkill$/);
    expect(submit?.body?.["params"]).toEqual({ recording_id: REC, epoch, name: "make-a-note", semver: "1.0.0", slots: [{ slot_id: "slot_1", classification: "input" }] });
    expect(await handlers["recording.compile"]({ recordingId: "run_1" })).toMatchObject({ ok: false, error: { code: "INTERNAL" } });
  });

  it("returns a server refusal as data the page branches on", async () => {
    const calls: Call[] = [];
    const handlers = bridge(calls, () => ({ code: "RECORDING_NOT_FINISHED", message: "finish first", correlation_id: "c", retryable: false }), 409).handlers();
    expect(await handlers["recording.compile"]({ recordingId: REC })).toMatchObject({ ok: false, error: { code: "RECORDING_NOT_FINISHED" } });
    expect(await handlers["recording.submit"]({ recordingId: REC, name: "x-skill", semver: "1.0.0", slots: [] })).toMatchObject({ ok: false, error: { code: "RECORDING_NOT_FINISHED" } });
  });
});

// ------------------------------------------------------------------ who can finish a recording

function sources(directory: string): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry);
    if (statSync(path).isDirectory()) {
      found.push(...sources(path));
    } else if (/\.(ts|tsx)$/.test(entry)) {
      found.push(path);
    }
  }
  return found;
}

describe("the structure that keeps a recording the person's", () => {
  const root = join(__dirname, "..", "..", "..");
  const all = sources(join(root, "apps", "desktop", "src"));
  const read = (path: string): string => readFileSync(path, "utf8");

  it("has one renderer file that finishes a recording, from a click handler built with the event", () => {
    const renderer = all.filter((path) => path.includes("/renderer/"));
    const finishing = renderer.filter((path) => read(path).includes("finishRecording(")).map((path) => relative(root, path));
    expect(finishing).toEqual(["apps/desktop/src/renderer/RecordingIndicator.tsx"]);
    const source = read(join(root, "apps/desktop/src/renderer/RecordingIndicator.tsx"));
    expect(source).toMatch(/window\.quansio\.finishRecording\(finishRequest\(event\)\)/);
    expect(source).toMatch(/void finish\(event\.nativeEvent\)/);
    // Nothing in the page reacts to a stream frame, a message or a timer by finishing.
    expect(source.match(/finishRecording\(/g)).toHaveLength(1);
    expect(source).not.toMatch(/onStreamFrame|setTimeout/);
  });

  it("never lets the page hold or send the owner fence", () => {
    const renderer = all.filter((path) => path.includes("/renderer/") && /Recording/.test(path));
    for (const path of renderer) {
      expect(read(path), path).not.toMatch(/\bepoch\b/i);
    }
    const bridgeSource = read(join(root, "apps/desktop/src/preload/bridge.ts"));
    expect(bridgeSource).not.toMatch(/epoch/);
  });

  it("has one main-process caller of the recorder's finish, behind the gesture check", () => {
    const callers = all.filter((path) => /\.finish\(\{ gesture/.test(read(path))).map((path) => relative(root, path));
    expect(callers).toEqual(["apps/desktop/src/main/ipc.ts"]);
    const host = read(join(root, "apps/desktop/src/main/recorder.ts"));
    const finishBody = host.slice(host.indexOf("async finish("), host.indexOf("async cancel("));
    expect(finishBody.indexOf("GESTURE_REQUIRED")).toBeGreaterThan(-1);
    expect(finishBody.indexOf("GESTURE_REQUIRED")).toBeLessThan(finishBody.indexOf('op: "finish"'));
  });
});
