// @vitest-environment jsdom
/** ADP-003 / ADP-002 on a page: the recording flow, its always-visible indicator, and the permission guide it can raise. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { ComputerPermissions } from "../../src/renderer/ComputerPermissions.js";
import { Recording } from "../../src/renderer/Recording.js";
import { RecordingIndicator } from "../../src/renderer/RecordingIndicator.js";
import { click, mount, text, type, type Rig } from "./rig.js";

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

const IDLE = { state: "idle" };

function proposal(extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    manifest: { procedure: { steps: [{ id: "s1", intent: "open the form", path: "ui", needs_review: true }, { id: "s2", intent: "send mail", path: "tool", tools: ["mail.send"] }], preconditions: [], postconditions: [], recovery: [] }, tool_needs: ["mail.send"], capability_needs: ["email.send"] },
    trust: { source: "recording", level: "UNTRUSTED_EXTERNAL", injection_suspected: ["ignore previous"] },
    slots: [
      { slot_id: "sl_1", name: "recipient", type: "email", example: "ada@example.com", control: { label: "To" } },
      { slot_id: "sl_2", name: "secret", type: "text", example: "[REDACTED:api_key]", control: { label: "API key" } },
    ],
    ...extra,
  };
}

function recordingRow(id: string, status: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, workspace_id: "ws_1", status, started_at: "2026-09-20T00:00:00Z", expires_at: "2999-01-01T00:00:00Z", stats: { actions: 7, screenshots: 2 },
    redaction: { rule_set: "v1", counts: { api_key: 2, card_number: 1 }, screenshots_withheld: 3 }, ...extra,
  };
}

const ok = { ok: true };

describe("the Recording view", () => {
  it("starts a recording, and disables Start while the recorder is capturing", async () => {
    rig = await mount(<Recording onSubmitted={() => undefined} />, { "recording.status": IDLE, "recording.list": { recordings: [] }, "recording.start": ok });
    expect(text(rig)).toContain("No recordings yet.");
    await click(rig, "Start recording");
    expect(sent("recording.start")).toHaveLength(1);
    rig.answer("recording.status", { state: "recording", recordingId: "rec_9" });
    await click(rig, "Start recording");
    expect(rig.container.querySelector<HTMLButtonElement>(".recording-start")?.disabled).toBe(true);
  });

  it("raises the permission guide when the recorder needs a permission, and opens only the named pane on a click", async () => {
    const snapshot = { applicable: true, rows: [{ permission: "screen_recording", state: "denied", needed: true, probed: true, action: "open_settings" }], idleSeconds: null, pauseInput: false, ready: false, pollIntervalMs: 5000 };
    rig = await mount(<Recording onSubmitted={() => undefined} />, {
      "recording.status": IDLE, "recording.list": { recordings: [] },
      "recording.start": { ok: false, code: "RECORDING_PERMISSION_REQUIRED", permission: "screen_recording", status: "denied" },
      "computer.permissions": snapshot, "computer.openSettings": ok,
    });
    await click(rig, "Start recording");
    expect(text(rig)).toContain("Recording needs a permission first.");
    expect(sent("computer.permissions")[0]?.payload).toEqual({ needs: ["screen_recording"], pauseInput: false });
    expect(rig.container.querySelector("[data-permission=screen_recording]")?.getAttribute("data-state")).toBe("denied");
    expect(rig.container.querySelector<HTMLButtonElement>(".recording-start")?.textContent).toBe("Try again");
    await click(rig, "Open System Settings");
    expect(sent("computer.openSettings")[0]?.payload).toEqual({ permission: "screen_recording" });
    expect(rig.container.querySelector<HTMLButtonElement>(".permission-continue")?.disabled).toBe(true);
  });

  it("shows an unavailable recorder and any other refusal", async () => {
    rig = await mount(<Recording onSubmitted={() => undefined} />, { "recording.status": IDLE, "recording.list": { recordings: [] }, "recording.start": { ok: false, code: "RECORDER_UNAVAILABLE" } });
    await click(rig, "Start recording");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("This build cannot record.");
    rig.answer("recording.start", { ok: false, error: { code: "CONFLICT_STATE", message: "already recording" } });
    await click(rig, "Start recording");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("CONFLICT_STATE: already recording");
  });

  it("shows what redaction removed as counts, and compiles a finished recording", async () => {
    rig = await mount(<Recording onSubmitted={() => undefined} />, { "recording.status": IDLE, "recording.list": { recordings: [recordingRow("rec_1", "finished")] }, "recording.compile": ok });
    expect(rig.container.querySelector("[data-category=api_key]")?.textContent).toBe("2 keys and tokens");
    expect(rig.container.querySelector("[data-category=screenshots_withheld]")?.textContent).toBe("3 screenshots withheld");
    expect(rig.container.querySelector("[data-recording=rec_1]")?.getAttribute("data-status")).toBe("finished");
    await click(rig, "Compile into a draft skill");
    expect(sent("recording.compile")[0]?.payload).toEqual({ recordingId: "rec_1" });
  });

  it("shows a refused compile, and an aborted recording's reason", async () => {
    rig = await mount(<Recording onSubmitted={() => undefined} />, {
      "recording.status": IDLE, "recording.list": { recordings: [recordingRow("rec_1", "finished"), recordingRow("rec_2", "aborted", { abort_reason: "recorder_stopped" })] },
      "recording.compile": { ok: false, error: { code: "VALIDATION_SCHEMA", message: "nothing recorded" } },
    });
    expect(text(rig)).toContain("(recorder_stopped)");
    await click(rig, "Compile into a draft skill");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("VALIDATION_SCHEMA: nothing recorded");
  });

  it("makes the person decide every slot, refuses a redacted constant, then submits as a draft", async () => {
    const submitted: string[] = [];
    rig = await mount(<Recording onSubmitted={() => { submitted.push("yes"); }} />, {
      "recording.status": IDLE, "recording.list": { recordings: [recordingRow("rec_1", "compiled", { has_proposal: true, proposal: proposal() })] }, "recording.submit": ok,
    });
    const page = text(rig);
    expect(page).toContain("open the form — needs your review");
    expect(page).toContain("mail.send");
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("reads like instructions");
    expect(rig.container.querySelector("[data-step=s2]")?.getAttribute("data-path")).toBe("tool");
    const constants = [...rig.container.querySelectorAll<HTMLInputElement>(".slot input[type=radio]")].filter((_, index) => index % 2 === 1);
    expect(constants.map((input) => input.disabled)).toEqual([false, true]);
    await type(rig, "Skill name", "send-mail");
    await click(rig, "Submit as draft");
    expect(sent("recording.submit")).toHaveLength(0);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("decide recipient, secret");
    await act(async () => {
      for (const slot of ["sl_1", "sl_2"]) {
        rig.container.querySelector<HTMLInputElement>(`[data-slot=${slot}] input[type=radio]`)?.click();
      }
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector("[data-slot=sl_1]")?.getAttribute("data-decision")).toBe("input");
    await act(async () => {
      rig.container.querySelector<HTMLInputElement>("[data-slot=sl_1] label:nth-of-type(2) input")?.click();
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector("[data-slot=sl_1]")?.getAttribute("data-decision")).toBe("constant");
    await click(rig, "Submit as draft");
    expect(rig.calls.some((call) => call.channel === "recording.submit")).toBe(true);
    expect(submitted).toEqual(["yes"]);
    expect(text(rig)).toContain("Submitted as a draft.");
  });

  it("shows the server's refusal of a submission and refuses a bad name on the page", async () => {
    rig = await mount(<Recording onSubmitted={() => undefined} />, {
      "recording.status": IDLE, "recording.list": { recordings: [recordingRow("rec_1", "compiled", { proposal: proposal({ slots: [] }) })] },
      "recording.submit": { ok: false, error: { code: "CONFLICT_STATE", message: "name taken" } },
    });
    await type(rig, "Skill name", "Bad Name");
    await click(rig, "Submit as draft");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("lowercase");
    expect(rig.calls.some((call) => call.channel === "recording.submit")).toBe(false);
    await type(rig, "Skill name", "good-name");
    await type(rig, "Version", "2.0.0");
    await click(rig, "Submit as draft");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("CONFLICT_STATE: name taken");
  });

  it("selects another recording from the list and shows an unreadable list as a notice", async () => {
    rig = await mount(<Recording onSubmitted={() => undefined} />, {
      "recording.status": IDLE, "recording.list": { recordings: [recordingRow("rec_1", "finished"), recordingRow("rec_2", "compiled", { proposal: proposal({ slots: [] }) })] },
    });
    await click(rig, "rec_2");
    expect(rig.container.querySelector(".recording-review")?.getAttribute("data-recording")).toBe("rec_2");
    await rig.unmount();
    rig = await mount(<Recording onSubmitted={() => undefined} />, { "recording.list": () => Promise.reject(new Error("list down")) });
    expect(text(rig)).toContain("list down");
  });
});

describe("the recording indicator", () => {
  it("shows nothing while idle, and the recorder's notice after it stopped", async () => {
    rig = await mount(<RecordingIndicator />, { "recording.status": IDLE });
    expect(rig.container.innerHTML).toBe("");
    await rig.unmount();
    rig = await mount(<RecordingIndicator />, { "recording.status": { state: "idle", notice: "account_changed" } });
    expect(text(rig)).toContain("signed-in account changed");
    await rig.unmount();
    rig = await mount(<RecordingIndicator />, { "recording.status": { state: "idle", notice: "recorder_exited" } });
    expect(text(rig)).toContain("The recorder stopped");
  });

  it("follows the recorder's counts while capturing, and says when it is paused for secure input", async () => {
    rig = await mount(<RecordingIndicator />, { "recording.status": { state: "recording", recordingId: "rec_1", actions: 4, screenshots: 2, paused: "secure_input" } });
    const indicator = rig.container.querySelector(".recording-indicator");
    expect(indicator?.getAttribute("data-recording")).toBe("recording");
    expect(indicator?.textContent).toContain("4 actions");
    expect(indicator?.textContent).toContain("Paused: secure input");
  });

  it("shows starting without a Finish control", async () => {
    rig = await mount(<RecordingIndicator />, { "recording.status": { state: "starting" } });
    expect(text(rig)).toContain("Starting the recorder");
    expect(rig.container.querySelector(".recording-finish")).toBeNull();
  });

  it("never reports a script-made click as the person's gesture", async () => {
    const ended: string[] = [];
    rig = await mount(<RecordingIndicator onEnded={() => { ended.push("ended"); }} />, {
      "recording.status": { state: "recording", recordingId: "rec_1" },
      "recording.finish": { ok: false, code: "GESTURE_REQUIRED", message: "click Finish yourself" },
    });
    await click(rig, "Finish recording");
    expect(sent("recording.finish")[0]?.payload).toEqual({ gesture: false });
    expect(rig.container.querySelector(".recording-error")?.textContent).toBe("GESTURE_REQUIRED: click Finish yourself");
    expect(ended).toEqual(["ended"]);
  });

  it("finishing clears the error, and cancel ends the recording", async () => {
    rig = await mount(<RecordingIndicator />, { "recording.status": { state: "recording", recordingId: "rec_1" }, "recording.finish": { ok: true }, "recording.cancel": { ok: true } });
    await click(rig, "Finish recording");
    expect(rig.container.querySelector(".recording-error")).toBeNull();
    await click(rig, "Cancel recording");
    expect(sent("recording.cancel")).toHaveLength(1);
  });
});

describe("the permission guide", () => {
  const row = (permission: string, state: string, action: string, needed = true): Record<string, unknown> => ({ permission, state, needed, probed: true, action });
  const snapshot = (rows: unknown[], extra: Record<string, unknown> = {}): Record<string, unknown> => ({ applicable: true, rows, idleSeconds: null, pauseInput: false, ready: false, pollIntervalMs: 5000, ...extra });
  const attention = { code: "COMPUTER_PERMISSION_REQUIRED", permission: "accessibility", status: "denied", guide: { action: "open_settings", pane: "Privacy_Accessibility" } } as const;

  it("resumes the parked run only when the snapshot says it is ready, with the person's own command", async () => {
    const resumed: string[] = [];
    rig = await mount(<ComputerPermissions run={{ id: "run_1", generation: 2 }} attention={attention} onResumed={() => { resumed.push("r"); }} />, {
      "computer.permissions": snapshot([row("accessibility", "granted", "none")], { ready: true }), "commands.invoke": ok,
    });
    expect(rig.container.querySelector("[data-guide]")?.getAttribute("data-guide")).toBe("ready");
    await click(rig, "Continue");
    expect(sent("commands.invoke")[0]?.payload).toMatchObject({ name: "ResumeRun", params: { run_id: "run_1", generation: 2 } });
    expect(resumed).toEqual(["r"]);
  });

  it("explains a policy restriction and a pause-input request without offering Settings", async () => {
    rig = await mount(<ComputerPermissions run={null} attention={{ ...attention, status: "restricted" }} onResumed={() => undefined} />, {
      "computer.permissions": snapshot([row("accessibility", "restricted", "contact_administrator")]),
    });
    expect(text(rig)).toContain("Only an administrator can lift it");
    expect(text(rig)).toContain("Ask your administrator");
    expect(rig.container.querySelector("button.permission-continue")).toHaveProperty("disabled", true);
    await rig.unmount();
    rig = await mount(<ComputerPermissions run={{ id: "run_1", generation: 1 }} attention={{ code: "COMPUTER_FOREGROUND_CONTROL_LAPSED", permission: null, status: null, guide: { action: "pause_input", pane: null } }} onResumed={() => undefined} />, {
      "computer.permissions": snapshot([], { pauseInput: true, ready: true, idleSeconds: 3 }),
    });
    expect(text(rig)).toContain("Stop using them");
    expect(sent("computer.permissions")[0]?.payload).toEqual({ needs: [], pauseInput: true });
  });

  it("stays out of the way when nothing is parked, and where the permissions do not apply", async () => {
    rig = await mount(<ComputerPermissions run={null} attention={null} onResumed={() => undefined} />, { "computer.permissions": snapshot([row("accessibility", "granted", "none")], { ready: true }) });
    expect(rig.container.innerHTML).toBe("");
    await rig.unmount();
    rig = await mount(<ComputerPermissions run={null} attention={attention} onResumed={() => undefined} />, { "computer.permissions": snapshot([], { applicable: false }) });
    expect(rig.container.innerHTML).toBe("");
  });

  it("shows an unreadable snapshot and a refused resume or Settings open as a notice", async () => {
    rig = await mount(<ComputerPermissions run={null} attention={attention} onResumed={() => undefined} />, { "computer.permissions": () => Promise.reject(new Error("probe failed")) });
    expect(rig.container.querySelector(".notice")?.textContent).toBe("probe failed");
    await rig.unmount();
    rig = await mount(<ComputerPermissions run={{ id: "run_1", generation: 1 }} attention={attention} onResumed={() => undefined} />, {
      "computer.permissions": snapshot([row("accessibility", "granted", "none"), row("screen_recording", "denied", "open_settings", false)], { ready: true }),
      "commands.invoke": () => Promise.reject(new Error("stale generation")), "computer.openSettings": () => Promise.reject(new Error("no pane")),
    });
    await click(rig, "Continue");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("stale generation");
    await click(rig, "Open System Settings");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("no pane");
  });
});
