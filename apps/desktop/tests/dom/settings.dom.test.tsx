// @vitest-environment jsdom
/** APP-002 / ADP-004 on a page: Settings (workspace + personal layers, effective values) and DesktopSettings (updates, shortcuts). */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { DesktopSettings } from "../../src/renderer/DesktopSettings.js";
import { Settings } from "../../src/renderer/Settings.js";
import { click, mount, text, type Rig } from "./rig.js";

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
  delete document.documentElement.dataset["theme"];
});

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

function labelled(label: string): HTMLInputElement | HTMLSelectElement {
  const element = [...rig.container.querySelectorAll("label")].find((candidate) => candidate.textContent === label);
  const target = (element?.control ?? null) as HTMLInputElement | HTMLSelectElement | null;
  if (!target) {
    throw new Error(`no field labelled ${label}`);
  }
  return target;
}

async function setField(label: string, value: string): Promise<void> {
  const element = labelled(label);
  const prototype = element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  await act(async () => {
    Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(element, value);
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
    await Promise.resolve();
  });
  await rig.flush();
}

async function submit(formLabel: string): Promise<void> {
  const form = rig.container.querySelector<HTMLFormElement>(`form[aria-label='${formLabel}']`);
  await act(async () => {
    form?.requestSubmit();
    await Promise.resolve();
  });
  await rig.flush();
}

function me(role: string, effective: Record<string, unknown> = {}): Record<string, unknown> {
  return { user: { id: "usr_1" }, workspaces: [{ id: "ws_1", name: "Acme", role, effective_settings: { execution_target: "cloud", retention_days: 90, autonomy_mode: "ask_for_consequences", telemetry: true, ...effective } }] };
}

describe("the Settings view", () => {
  it("shows the effective values the server returned, and applies the effective theme", async () => {
    rig = await mount(<Settings workspaceId="ws_1" />, { "session.me": me("admin", { theme: "dark", extra: { a: 1 } }) });
    expect(labelled("Workspace name").value).toBe("Acme");
    expect(labelled("Where work runs").value).toBe("cloud");
    expect(labelled("Keep data for (days)").value).toBe("90");
    expect((labelled("Send diagnostic telemetry") as HTMLInputElement).checked).toBe(true);
    expect(labelled("How careful the agent is in this workspace").value).toBe("ask_for_consequences");
    const effective = [...rig.container.querySelectorAll("dl")].at(-1)?.textContent ?? "";
    expect(effective).toContain("execution_target");
    expect(effective).toContain('{"a":1}');
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(sent("appearance.set")[0]?.payload).toEqual({ theme: "dark" });
  });

  it("saves the workspace layer: only what changed, the autonomy floor only when edited", async () => {
    rig = await mount(<Settings workspaceId="ws_1" />, { "session.me": me("admin"), "settings.updateWorkspace": { effective_settings: {}, effects: [{ kind: "restart", reason: "Restart to apply." }] } });
    await setField("Workspace name", " Acme 2 ");
    await setField("Where work runs", "local_capsule");
    await setField("Keep data for (days)", "30");
    await click(rig, "Share product analytics");
    await submit("This workspace");
    const call = sent("settings.updateWorkspace")[0]?.payload as { workspaceId: string; name: string; settings: Record<string, unknown> };
    expect(call.workspaceId).toBe("ws_1");
    expect(call.name).toBe("Acme 2");
    expect(call.settings).toMatchObject({ execution_target: "local_capsule", retention_days: 30, share_analytics: true, telemetry: true });
    expect(call.settings).not.toHaveProperty("autonomy_mode");
    expect(rig.container.querySelector("[role=status]")?.textContent).toBe("Saved. Restart to apply.");
    await setField("How careful the agent is in this workspace", "read_only");
    await submit("This workspace");
    expect((sent("settings.updateWorkspace")[1]?.payload as { settings: Record<string, unknown> }).settings["autonomy_mode"]).toBe("read_only");
  });

  it("does not send a retention that is not a positive number", async () => {
    rig = await mount(<Settings workspaceId="ws_1" />, { "session.me": me("admin"), "settings.updateWorkspace": {} });
    await setField("Keep data for (days)", "");
    await submit("This workspace");
    expect((sent("settings.updateWorkspace")[0]?.payload as { settings: Record<string, unknown> }).settings).not.toHaveProperty("retention_days");
  });

  it("saves the personal layer with the theme, and the autonomy only if the person changed it", async () => {
    rig = await mount(<Settings workspaceId="ws_1" />, { "session.me": me("member"), "settings.updateUser": { effective_settings: {} } });
    await setField("Theme", "light");
    await submit("Just for you");
    expect(sent("settings.updateUser")[0]?.payload).toEqual({ workspaceId: "ws_1", settings: { theme: "light" } });
    await setField("How careful I want my agent to be", "ask_every_action");
    await submit("Just for you");
    expect(sent("settings.updateUser")[1]?.payload).toEqual({ workspaceId: "ws_1", settings: { theme: "system", autonomy_mode: "ask_every_action" } }); // the saved theme is re-read from the server's effective settings
  });

  it("tells a non-administrator the workspace layer is read-only", async () => {
    rig = await mount(<Settings workspaceId="ws_1" />, { "session.me": me("member") });
    expect(text(rig)).toContain("Only a workspace administrator can change these.");
    expect(rig.container.querySelector("fieldset")?.disabled).toBe(true);
  });

  it("shows a refused save as the status line", async () => {
    // eslint-disable-next-line @typescript-eslint/prefer-promise-reject-errors -- a non-Error rejection is the case under test
    rig = await mount(<Settings workspaceId="ws_1" />, { "session.me": me("admin"), "settings.updateWorkspace": () => Promise.reject(new Error("VALIDATION_SCHEMA: no")), "settings.updateUser": () => Promise.reject("odd") });
    await submit("This workspace");
    expect(rig.container.querySelector("[role=status]")?.textContent).toContain("VALIDATION_SCHEMA: no");
    await submit("Just for you");
    expect(rig.container.querySelector("[role=status]")?.textContent).toBe("The command was refused.");
  });
});

const SHORTCUTS = {
  ok: true,
  result: {
    platform: "darwin",
    snapshot: {
      recording: false,
      shortcuts: [
        { id: "new_thread", scope: "window", binding: "mod+n", display: "⌘N", registered: true, failure: null },
        { id: "toggle_window", scope: "global", binding: "mod+alt+q", display: "⌘⌥Q", registered: false, failure: "registration_failed" },
        { id: "find", scope: "window", binding: null, display: null, registered: true, failure: null },
      ],
    },
  },
};

function prefs(extra: Record<string, unknown> = {}, state = "idle"): Record<string, unknown> {
  return { ok: true, result: { keepAwake: false, installWhenIdle: false, waitingForIdle: false, updateStatus: { state }, platform: "darwin", ...extra } };
}

function rowButton(index: number, label: string): HTMLButtonElement {
  const button = [...(rig.container.querySelectorAll(".shortcut-row")[index]?.querySelectorAll("button") ?? [])].find((candidate) => candidate.textContent === label);
  if (!button) {
    throw new Error(`no ${label} button in row ${String(index)}`);
  }
  return button;
}

async function press(init: KeyboardEventInit): Promise<void> {
  await act(async () => {
    window.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
    await Promise.resolve();
  });
  await rig.flush();
}

describe("the Desktop settings", () => {
  it("lists shortcuts with their binding, a note when the system would not register one, and update state", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs(), "shell.shortcuts.get": SHORTCUTS });
    const rows = [...rig.container.querySelectorAll(".shortcut-row")];
    expect(rows.map((row) => row.querySelector(".shortcut-binding")?.textContent)).toEqual(["⌘N", "⌘⌥Q", "Not set"]);
    expect(rows[1]?.textContent).toContain("Not registered by the system");
    expect(rowButton(2, "Clear").disabled).toBe(true);
    expect(text(rig)).toContain("Up to date.");
    expect(rig.container.querySelector<HTMLButtonElement>("button:last-of-type")).not.toBeNull();
  });

  it("sends keep-awake and install-when-idle changes and shows what main answered", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs(), "shell.preferences.set": prefs({ keepAwake: true, waitingForIdle: true }) });
    await click(rig, "Keep this computer awake");
    expect(sent("shell.preferences.set")[0]?.payload).toEqual({ keepAwake: true });
    expect(text(rig)).toContain("as soon as nothing is running");
    await click(rig, "Install updates when nothing is running");
    expect(sent("shell.preferences.set")[1]?.payload).toEqual({ installWhenIdle: true });
  });

  it("restarts to update only when one is downloaded, and says when there is none after all", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs({}, "downloaded"), "update.install": { ok: true, result: { kind: "not_downloaded" } } });
    expect(text(rig)).toContain("An update is ready to install.");
    await click(rig, "Restart to update");
    expect(sent("update.install")[0]?.payload).toEqual({ mode: "now" });
    expect(text(rig)).toContain("no downloaded update");
  });

  it("disables restart without a downloaded update", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs({}, "checking") });
    expect([...rig.container.querySelectorAll("button")].find((button) => button.textContent === "Restart to update")?.disabled).toBe(true);
    expect(text(rig)).toContain("Checking for updates");
  });

  it("clears and resets a binding through main and reloads", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs(), "shell.shortcuts.get": SHORTCUTS, "shell.shortcuts.set": { ok: true, result: {} }, "shell.shortcuts.reset": { ok: true, result: {} } });
    const reads = (): number => sent("shell.shortcuts.get").length;
    const before = reads();
    await act(async () => { rowButton(0, "Clear").click(); await Promise.resolve(); });
    await rig.flush();
    expect(sent("shell.shortcuts.set")[0]?.payload).toEqual({ id: "new_thread", binding: null });
    expect(reads()).toBeGreaterThan(before);
    await act(async () => { rowButton(1, "Reset").click(); await Promise.resolve(); });
    await rig.flush();
    expect(sent("shell.shortcuts.reset")[0]?.payload).toEqual({ id: "toggle_window" });
  });

  it("explains a refused clear", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs(), "shell.shortcuts.get": SHORTCUTS, "shell.shortcuts.set": { ok: false, error: { code: "registration_failed", message: "x", details: {} } } });
    await act(async () => { rowButton(0, "Clear").click(); await Promise.resolve(); });
    await rig.flush();
    expect(text(rig)).toContain("would not register that shortcut");
  });

  it("records a new binding from the next key press, and ends the recording", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs(), "shell.shortcuts.get": SHORTCUTS, "shell.recording.begin": { ok: true, result: {} }, "shell.shortcuts.set": { ok: true, result: {} } });
    await act(async () => { rowButton(0, "Record").click(); await Promise.resolve(); });
    await rig.flush();
    expect(rig.container.querySelector(".shortcut-row .shortcut-binding")?.textContent).toContain("Press the new keys");
    expect(rowButton(1, "Record").disabled).toBe(true);
    await press({ code: "ShiftLeft", key: "Shift", shiftKey: true });
    expect(sent("shell.shortcuts.set")).toHaveLength(0);
    await press({ code: "KeyK", key: "k", metaKey: true, altKey: true });
    expect(sent("shell.shortcuts.set")[0]?.payload).toEqual({ id: "new_thread", binding: "mod+alt+k" });
    expect(sent("shell.recording.end")).toHaveLength(1);
    expect(rig.container.querySelector(".shortcut-row .shortcut-binding")?.textContent).toBe("⌘N");
  });

  it("names the other shortcut when a recorded binding conflicts, and Escape cancels", async () => {
    rig = await mount(<DesktopSettings />, {
      "shell.preferences.get": prefs(), "shell.shortcuts.get": SHORTCUTS, "shell.recording.begin": { ok: true, result: {} },
      "shell.shortcuts.set": { ok: false, error: { code: "conflict", message: "taken", details: { with: "quick_entry" } } },
    });
    await act(async () => { rowButton(0, "Record").click(); await Promise.resolve(); });
    await rig.flush();
    await press({ code: "KeyQ", key: "q", metaKey: true });
    expect(text(rig)).toContain("That is already used by quick entry.");
    await act(async () => { rowButton(0, "Record").click(); await Promise.resolve(); });
    await rig.flush();
    await press({ code: "Escape", key: "Escape" });
    expect(rig.container.querySelector(".shortcut-row .shortcut-binding")?.textContent).toBe("⌘N");
    expect(sent("shell.recording.end").length).toBeGreaterThan(1);
  });

  it("does not start recording when main refuses, stops when main ends it, and ends an open recording on leaving the page", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs(), "shell.shortcuts.get": SHORTCUTS, "shell.recording.begin": { ok: false, error: { code: "busy", message: "x" } } });
    await act(async () => { rowButton(0, "Record").click(); await Promise.resolve(); });
    await rig.flush();
    expect(rig.container.querySelector(".shortcut-row .shortcut-binding")?.textContent).toBe("⌘N");
    rig.answer("shell.recording.begin", { ok: true, result: {} });
    await act(async () => { rowButton(0, "Record").click(); await Promise.resolve(); });
    await rig.flush();
    await rig.push("shell.recording.ended", {});
    expect(rig.container.querySelector(".shortcut-row .shortcut-binding")?.textContent).toBe("⌘N");
    await act(async () => { rowButton(0, "Record").click(); await Promise.resolve(); });
    await rig.flush();
    await rig.unmount();
    expect(sent("shell.recording.end").length).toBeGreaterThan(0);
    rig = await mount(<DesktopSettings />);
  });

  it("follows pushed shortcut snapshots and update status", async () => {
    rig = await mount(<DesktopSettings />, { "shell.preferences.get": prefs(), "shell.shortcuts.get": SHORTCUTS });
    await rig.push("shell.shortcuts", { snapshot: { recording: false, shortcuts: [{ id: "find", scope: "window", binding: "mod+f", display: "⌘F", registered: true, failure: null }] } });
    expect([...rig.container.querySelectorAll(".shortcut-row .shortcut-binding")].map((node) => node.textContent)).toEqual(["⌘F"]);
    const before = sent("shell.preferences.get").length;
    await rig.push("update.status", { state: "downloaded" });
    expect(sent("shell.preferences.get").length).toBeGreaterThan(before);
  });
});
