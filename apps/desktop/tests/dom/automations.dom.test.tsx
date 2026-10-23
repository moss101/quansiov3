// @vitest-environment jsdom
/** APP-010 / ADP-007 on a page: the Automations surface (and its parts) over the real preload API. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { Automations } from "../../src/renderer/Automations.js";
import { click, mount, text, type, type Rig } from "./rig.js";

const ME = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const RTN = "rtn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const HOOK = "rtn_01J8Z3K6F1N8VQ2X5W9Y0AAAAB";
const SECRET = "whsec_" + "c".repeat(40);

function routine(id: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, objective_template: { title: `Routine ${id.slice(-1)}`, prompt: "summarize" }, trigger: { kind: "interval", spec: "1h", timezone: "UTC" },
    absence_policy: "skip", status: "active", owner_user_id: ME, next_due_at: "2026-09-24T07:00:00Z", revision: 3,
    fires: [
      { id: "rfi_1", fire_key: "k1", source: "schedule", status: "fired", attempts: 1, window_at: "2026-09-23T10:00:00Z", run_id: "run_1" },
      { id: "rfi_2", fire_key: "k2", source: "schedule", status: "pending", attempts: 0, window_at: "2026-09-23T10:00:00Z", run_id: "run_1" },
    ],
    ...extra,
  };
}

const webhook = routine(HOOK, {
  trigger: { kind: "webhook", spec: "", timezone: "UTC", filters: [[{ path: "/action", op: "eq", value: "opened" }]] },
  hook: { endpoint_id: "hk_" + "A".repeat(43), path: "/v1/hooks/hk_" + "A".repeat(43), secret: "set", deliveries: { accepted: 3, filtered: 2 } },
  script: { max_agent_calls: 40, estimated_agent_calls: 12, steps: [{ id: "collect", kind: "agent" }] },
  script_runs: [{ fire_id: "rfi_1", run_id: "run_1", status: "waiting", calls_used: 40, max_agent_calls: 40, estimated_agent_calls: 12, open_gate: { gate: "extend_budget", calls: 60, approval_id: "apr_1" }, steps: [{ id: "collect", kind: "agent", status: "done" }] }],
});

const notifications = {
  items: [
    { id: "ntf_1", kind: "mention", title: "You were mentioned", channels: ["in_app"], channels_delivered: ["in_app"], read_at: null },
    { id: "ntf_2", kind: "completed", title: "Routine ran", channels: ["in_app"], channels_delivered: ["in_app"], read_at: "2026-09-20T00:00:00Z" },
  ],
  preferences: [{ kind: "mention", channels: ["in_app", "desktop"], chosen: true }],
};

interface Command { name: string; params: Record<string, unknown> }

/** Answers every routine command from `outcomes` (by name), defaulting to success. */
function bridge(outcomes: Record<string, unknown> = {}, routines: unknown[] = [routine(RTN), webhook]): Record<string, unknown> {
  return {
    "routines.list": { items: routines },
    "notifications.list": notifications,
    "session.me": { user: { id: ME }, workspaces: [] },
    "routines.command": (payload: Command) => outcomes[payload.name] ?? { ok: true, result: {} },
    "notifications.read": { ok: true, result: {} },
    "notifications.preference": { ok: true, result: {} },
  };
}

function commands(rig: Rig): Command[] {
  return rig.calls.filter((call) => call.channel === "routines.command").map((call) => call.payload as Command);
}

function within(rig: Rig, id: string): HTMLElement {
  const row = [...rig.container.querySelectorAll<HTMLElement>("li.routine")].find((li) => li.textContent.includes(`Routine ${id.slice(-1)}`));
  if (!row) {
    throw new Error(`no routine ${id}`);
  }
  return row;
}

async function press(rig: Rig, scope: HTMLElement, label: string | RegExp): Promise<void> {
  const button = [...scope.querySelectorAll<HTMLButtonElement>("button")].find((candidate) => {
    const names = [candidate.textContent, candidate.getAttribute("aria-label") ?? ""];
    return names.some((name) => (typeof label === "string" ? name.includes(label) : label.test(name)));
  });
  if (!button) {
    throw new Error(`no button ${String(label)} in ${scope.textContent.slice(0, 200)}`);
  }
  await act(async () => {
    button.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    await Promise.resolve();
  });
  await rig.flush();
}

async function submit(rig: Rig, form: HTMLFormElement): Promise<void> {
  await act(async () => {
    form.requestSubmit();
    await Promise.resolve();
  });
  await rig.flush();
}

async function select(rig: Rig, element: HTMLSelectElement, value: string): Promise<void> {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(element, value);
    element.dispatchEvent(new Event("change", { bubbles: true }));
    await Promise.resolve();
  });
  await rig.flush();
}

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

describe("the Automations surface", () => {
  it("lists routines with their state, hook line and script, and notifications with preferences", async () => {
    let approvals = 0;
    rig = await mount(<Automations workspaceId="ws_1" onOpenApprovals={() => { approvals += 1; }} />, bridge());
    const page = text(rig);
    expect(page).toContain("interval 1h");
    expect(page).toContain("fires 1 (+1)");
    expect(page).toContain("duplicate fires detected");
    expect(page).toContain("revision 3");
    const hook = within(rig, HOOK);
    expect(hook.textContent).toContain("last 24 h: 3 accepted, 2 filtered");
    expect(hook.textContent).toContain("/action = opened");
    expect(hook.querySelector("[data-attention=paused_budget]")).not.toBeNull();
    await press(rig, hook, /review/i);
    expect(approvals).toBe(1);
    expect(rig.container.querySelectorAll(".auto-notifications li").length).toBe(2);
    expect(text(rig)).toContain("1 unread");
    // Neither routine carries a secret on the page.
    expect(page).not.toContain("whsec_");
  });

  it("sends each control with its command name and shows a stale-list conflict as a notice", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      DeleteRoutine: { ok: false, error: { code: "CONFLICT_REVISION", message: "moved", details: { routine_id: RTN, expected_revision: 3, current_revision: 5 } } },
    }));
    const row = within(rig, RTN);
    await press(rig, row, "Pause");
    await press(rig, row, "Run now");
    expect(commands(rig).map((command) => command.name)).toEqual(["PauseRoutine", "TriggerRoutineNow"]);
    await press(rig, row, "Delete");
    expect(text(rig)).toContain("revision 5, expected 3");
    // A plain refusal names its code.
    await press(rig, within(rig, HOOK), "Delete");
  });

  it("creates a routine from the form and resets it; the draft is checked before it is sent", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge());
    const create = rig.container.querySelector<HTMLFormElement>("form.auto-new[aria-label='New routine']") as HTMLFormElement;
    expect(create.querySelector<HTMLButtonElement>("button[type=submit]")?.disabled).toBe(true);
    await type(rig, "Title", "Digest");
    await type(rig, "What should the teammate do", "summarize the day");
    expect(text(rig)).not.toContain("a routine needs");
    await type(rig, "Schedule or event", "soon");
    expect(text(rig)).toContain("an interval is a number");
    expect(commands(rig)).toEqual([]);
    await type(rig, "Schedule or event", "30m");
    await submit(rig, create);
    const sent = commands(rig)[0];
    expect(sent?.name).toBe("CreateRoutine");
    expect(sent?.params).toMatchObject({ workspace_id: "ws_1", title: "Digest", prompt: "summarize the day", trigger: { kind: "interval", spec: "30m", timezone: "UTC" }, absence_policy: "skip" });
    expect(String(sent?.params["create_request_id"])).toMatch(/^create-/);
    expect((rig.container.querySelector("input[aria-label='Title']") as HTMLInputElement).value).toBe("");
  });

  it("maps a refused create onto the field it is about, and other refusals onto a notice", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      CreateRoutine: { ok: false, error: { code: "AUTOMATION_LIMIT_EXCEEDED", message: "too frequent", details: { limit: "min_interval_seconds", value: 5, allowed: 60 } } },
    }));
    await type(rig, "Title", "Digest");
    await type(rig, "What should the teammate do", "p");
    await type(rig, "Schedule or event", "5s");
    await submit(rig, rig.container.querySelector("form.auto-new") as HTMLFormElement);
    expect(rig.container.querySelector("form.auto-new .error[role=alert]")?.textContent).toContain("allowed 60");
    expect((rig.container.querySelector("input[aria-label='Title']") as HTMLInputElement).value).toBe("Digest");
    rig.answer("routines.command", { ok: false, error: { code: "POLICY_DENIED", message: "no" } });
    await type(rig, "Schedule or event", "6s");
    await submit(rig, rig.container.querySelector("form.auto-new") as HTMLFormElement);
    expect(rig.container.querySelector(".notice[role=status] .notice, [role=status] p.notice")?.textContent).toContain("POLICY_DENIED");
  });

  it("shows a new inbound hook's secret once, copies it, and forgets it on dismissal", async () => {
    const copied: string[] = [];
    Object.defineProperty(navigator, "clipboard", { value: { writeText: (value: string) => { copied.push(value); return Promise.resolve(); } }, configurable: true });
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      CreateRoutine: { ok: true, result: { id: HOOK, hook: { path: "/v1/hooks/hk_x" }, secret: SECRET } },
    }));
    await type(rig, "Title", "Hooked");
    await type(rig, "What should the teammate do", "p");
    const kind = [...rig.container.querySelectorAll<HTMLSelectElement>("form.auto-new select")][0] as HTMLSelectElement;
    await select(rig, kind, "webhook");
    expect(text(rig)).toContain("A webhook has no schedule");
    await submit(rig, rig.container.querySelector("form.auto-new") as HTMLFormElement);
    expect(rig.container.querySelector(".hook-secret")?.textContent).toBe(SECRET);
    const region = rig.container.querySelector<HTMLElement>(".secret-once") as HTMLElement;
    await press(rig, region, "Copy signing secret");
    await press(rig, region, "Copy endpoint path");
    expect(copied).toEqual([SECRET, "/v1/hooks/hk_x"]);
    expect(text(rig)).toContain("Endpoint path copied.");
    await press(rig, region, "Dismiss");
    expect(text(rig)).not.toContain(SECRET);
    expect(rig.container.querySelector(".secret-once")).toBeNull();
  });

  it("says when the clipboard refuses", async () => {
    Object.defineProperty(navigator, "clipboard", { value: { writeText: () => Promise.reject(new Error("denied")) }, configurable: true });
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      CreateRoutine: { ok: true, result: { id: HOOK, hook: { path: "/v1/hooks/hk_x" }, secret: SECRET, replayed: false } },
    }));
    await type(rig, "Title", "T");
    await type(rig, "What should the teammate do", "p");
    await type(rig, "Schedule or event", "1h");
    await submit(rig, rig.container.querySelector("form.auto-new") as HTMLFormElement);
    await press(rig, rig.container.querySelector<HTMLElement>(".secret-once") as HTMLElement, "Copy signing secret");
    expect(text(rig)).toContain("Could not copy");
  });

  it("rotates a hook's secret only after a confirming second press", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      RotateRoutineHook: { ok: true, result: { id: HOOK, hook: { path: "/v1/hooks/hk_x" }, secret: SECRET } },
    }));
    const row = within(rig, HOOK);
    await press(rig, row, "Rotate secret");
    expect(commands(rig).some((command) => command.name === "RotateRoutineHook")).toBe(false);
    await press(rig, within(rig, HOOK), "Confirm");
    expect(commands(rig).find((command) => command.name === "RotateRoutineHook")?.params).toMatchObject({ routine_id: HOOK });
    expect(rig.container.querySelector(".secret-once")?.getAttribute("data-kind")).toBe("rotated");
    expect(text(rig)).toContain("The previous secret stopped working at once.");
  });

  it("edits a routine as a full replacement under its revision", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge());
    await press(rig, within(rig, RTN), "Edit");
    const edit = rig.container.querySelector<HTMLElement>("[data-edit]") as HTMLElement;
    const title = edit.querySelector("input[aria-label='Title']") as HTMLInputElement;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(title, "Renamed");
      title.dispatchEvent(new Event("input", { bubbles: true }));
      await Promise.resolve();
    });
    await submit(rig, edit.querySelector("form") as HTMLFormElement);
    const sent = commands(rig).find((command) => command.name === "UpdateRoutine");
    expect(sent?.params).toMatchObject({ routine_id: RTN, expected_revision: 3, title: "Renamed", mode: "act" });
    expect(rig.container.querySelector("[data-edit]")).toBeNull();
  });

  it("keeps the draft on a revision conflict, then reloads and reviews before saving again", async () => {
    let served = routine(RTN);
    rig = await mount(<Automations workspaceId="ws_1" />, {
      ...bridge({
        UpdateRoutine: { ok: false, error: { code: "CONFLICT_REVISION", message: "moved", details: { routine_id: RTN, expected_revision: 3, current_revision: 4 } } },
      }),
      "routines.list": () => ({ items: [served] }),
    });
    await press(rig, within(rig, RTN), "Edit");
    const edit = (): HTMLElement => rig.container.querySelector<HTMLElement>("[data-edit]") as HTMLElement;
    const title = edit().querySelector("input[aria-label='Title']") as HTMLInputElement;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(title, "Mine");
      title.dispatchEvent(new Event("input", { bubbles: true }));
      await Promise.resolve();
    });
    await submit(rig, edit().querySelector("form") as HTMLFormElement);
    expect(edit().querySelector(".conflict")?.textContent).toContain("revision 4");
    expect((edit().querySelector("input[aria-label='Title']") as HTMLInputElement).value).toBe("Mine");
    expect(edit().querySelector<HTMLButtonElement>("button[type=submit]")?.disabled).toBe(true);
    served = routine(RTN, { revision: 4, objective_template: { title: "Routine A", prompt: "changed elsewhere" } });
    await press(rig, edit(), "Reload and review");
    expect(edit().querySelector(".conflict")?.textContent).toContain("title");
    expect(edit().querySelector<HTMLButtonElement>("button[type=submit]")?.disabled).toBe(false);
    rig.answer("routines.command", { ok: true, result: {} });
    await submit(rig, edit().querySelector("form") as HTMLFormElement);
    const updates = commands(rig).filter((command) => command.name === "UpdateRoutine");
    expect(updates[updates.length - 1]?.params["expected_revision"]).toBe(4);
  });

  it("shows an edit's refusal on the form and cancels without sending", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      UpdateRoutine: { ok: false, error: { code: "AUTOMATION_TIMEZONE_UNKNOWN", message: "unknown zone", details: { timezone: "Mars/Base" } } },
    }));
    await press(rig, within(rig, RTN), "Edit");
    let edit = rig.container.querySelector<HTMLElement>("[data-edit]") as HTMLElement;
    await submit(rig, edit.querySelector("form") as HTMLFormElement);
    edit = rig.container.querySelector<HTMLElement>("[data-edit]") as HTMLElement;
    expect(edit.textContent).toContain("Mars/Base");
    await press(rig, edit, "Cancel");
    expect(rig.container.querySelector("[data-edit]")).toBeNull();
  });

  it("dry-runs an interval routine at once and a webhook routine from a sample, refusing bad JSON locally", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      DryRunRoutine: { ok: true, result: { valid: true, trigger: { kind: "webhook" }, next_windows: ["2026-09-24T08:00:00Z"], filter: { evaluated: true, matched: true, matched_group: 0 }, instruction: { bytes: 12, blocks: [{ type: "untrusted_data", text: "payload", source: "hook" }, { type: "text", text: "do it" }] }, script: { static_calls: 1, estimated_agent_calls: 5, max_agent_calls: 9, needs_approval: true, approval_threshold_calls: 4, steps: [] } } },
    }));
    await press(rig, within(rig, RTN), "Dry run");
    expect(commands(rig)[0]?.name).toBe("DryRunRoutine");
    expect(within(rig, RTN).querySelector(".auto-dry-result")?.getAttribute("data-valid")).toBe("true");
    expect(within(rig, RTN).textContent).toContain("Nothing was started");
    // The webhook routine waits for a sample.
    await press(rig, within(rig, HOOK), /^Dry run$/);
    const hook = within(rig, HOOK);
    expect(commands(rig).length).toBe(1);
    const sample = hook.querySelector("textarea.auto-sample") as HTMLTextAreaElement;
    const setText = async (value: string): Promise<void> => {
      await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set?.call(sample, value);
        sample.dispatchEvent(new Event("input", { bubbles: true }));
        await Promise.resolve();
      });
    };
    await setText("{not json");
    await press(rig, within(rig, HOOK), "Run the dry run");
    expect(commands(rig).length).toBe(1);
    expect(within(rig, HOOK).querySelector(".auto-dry [role=alert]")?.textContent).toContain("VALIDATION_SCHEMA");
    await setText('{"action":"opened"}');
    await press(rig, within(rig, HOOK), "Run the dry run");
    const body = within(rig, HOOK);
    expect(commands(rig)[1]?.params).toMatchObject({ routine_id: HOOK });
    expect(body.querySelector(".auto-dry-filter")?.getAttribute("data-verdict")).toBe("passes");
    expect(body.textContent).toContain("Untrusted data from hook");
    expect(body.textContent).toContain("Needs approval: more than 4 calls");
    // A second press closes the panel.
    await press(rig, within(rig, HOOK), /^Dry run$/);
    expect(within(rig, HOOK).querySelector(".auto-dry-result")).toBeNull();
  });

  it("says a rate-limited dry run should be retried", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({
      DryRunRoutine: { ok: false, error: { code: "RATE_LIMITED", message: "slow down" } },
    }));
    await press(rig, within(rig, RTN), "Dry run");
    expect(within(rig, RTN).textContent).toContain("Too many dry runs this minute");
  });

  it("dry-runs the draft in the create form and builds webhook filters", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge({ DryRunRoutine: { ok: true, result: { valid: false, problems: [{ code: "BAD", message: "no", at: "/trigger" }] } } }));
    await type(rig, "Title", "T");
    await type(rig, "What should the teammate do", "p");
    const create = rig.container.querySelector("form.auto-new") as HTMLFormElement;
    await select(rig, create.querySelector("select") as HTMLSelectElement, "webhook");
    const form = (): HTMLFormElement => rig.container.querySelector("form.auto-new") as HTMLFormElement;
    await press(rig, form(), "Add group");
    await press(rig, form(), "Add condition");
    await press(rig, form(), "Add group");
    expect(form().querySelectorAll(".auto-filter-group").length).toBe(2);
    await press(rig, form(), "Remove group 2");
    expect(form().querySelectorAll(".auto-filter-group").length).toBe(1);
    // An empty condition is refused before anything is sent.
    await press(rig, form(), "Dry run");
    expect(rig.calls.filter((call) => call.channel === "routines.command").length).toBe(0);
    expect(rig.container.querySelector(".auto-dry [role=alert]")).not.toBeNull();
    await press(rig, form(), "Remove condition 1.2");
    await type(rig, "JSON Pointer", "/action");
    await type(rig, "Value 1.1", "opened");
    await press(rig, form(), "Dry run");
    expect(commands(rig).length).toBe(1);
    expect(text(rig)).toContain("has problems");
    expect(text(rig)).toContain("BAD");
  });

  it("marks a notification read, sends a preference per kind, and follows notification events on the stream", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, bridge());
    await click(rig, "Mark read");
    expect(rig.calls.find((call) => call.channel === "notifications.read")?.payload).toEqual({ notificationId: "ntf_1" });
    const emailBox = [...rig.container.querySelectorAll<HTMLInputElement>(".auto-preferences input")][2] as HTMLInputElement;
    await act(async () => {
      emailBox.click();
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.calls.find((call) => call.channel === "notifications.preference")?.payload).toEqual({ workspace_id: "ws_1", kind: "mention", channels: ["in_app", "desktop", "email"] });
    await rig.push("stream.frame", { event: { type: "notification.created", payload: { user_id: ME, notification_id: "ntf_9", kind: "failed", title: "A run failed", channels: ["in_app"] } } });
    expect(text(rig)).toContain("A run failed");
    await rig.push("stream.frame", { event: { type: "notification.created", payload: { user_id: "usr_other", notification_id: "ntf_10", kind: "failed", title: "Someone else's", channels: ["in_app"] } } });
    expect(text(rig)).not.toContain("Someone else's");
    const before = rig.calls.filter((call) => call.channel === "routines.list").length;
    await rig.push("stream.frame", { event: { type: "routine.fired" } });
    expect(rig.calls.filter((call) => call.channel === "routines.list").length).toBeGreaterThan(before);
  });

  it("shows a load failure as a notice", async () => {
    rig = await mount(<Automations workspaceId="ws_1" />, { ...bridge(), "routines.list": () => { throw new Error("server down"); } });
    expect(text(rig)).toContain("server down");
  });
});
