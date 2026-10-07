/**
 * The shell host (ADP-004): the pieces composed, driven through the same channel handlers the IPC layer installs. The
 * windows, power-save blocker, global shortcuts, clock and updater are doubles; everything between them, the registry,
 * the gates, the tracker and the host, is the shipped code.
 */

import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import type { EventFrame } from "../src/client/stream.js";
import { ActivityTracker } from "../src/main/activity.js";
import { ConnectivityMonitor } from "../src/main/connectivity.js";
import { KeepAwake } from "../src/main/keep-awake.js";
import type { MenuItem } from "../src/main/menu.js";
import { PreferencesFile } from "../src/main/preferences.js";
import { QuitGate } from "../src/main/quit.js";
import { parseShellConfig } from "../src/main/shell-config.js";
import { ShellHost } from "../src/main/shell-host.js";
import { ShortcutRegistry } from "../src/main/shortcuts.js";
import { UpdateGate, type UpdateChoice } from "../src/main/update-gate.js";
import { UpdateHooks } from "../src/main/updates.js";
import { WindowManager, INDICATOR_SIZE, type ManagedWindow } from "../src/main/windows.js";
import { SHELL_CHANNELS, type ShellChannel } from "../src/preload/bridge.js";
import type { MaintenanceStatus, WorkStatus } from "../src/main/work-status.js";

import { Blocker, Clock, FakeDisplays, FakeFactory, FakeSystem, FakeUpdater, LEFT, RIGHT, THREAD, OTHER_THREAD } from "./shell-fakes.js";

const CONFIG = parseShellConfig(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "config", "desktop-shell.yaml"), "utf8"));
const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

interface Rig {
  host: ShellHost;
  windows: WindowManager;
  factory: FakeFactory;
  displays: FakeDisplays;
  clock: Clock;
  system: FakeSystem;
  blocker: Blocker;
  updater: FakeUpdater;
  updates: UpdateHooks;
  quit: QuitGate;
  keepAwake: KeepAwake;
  preferences: PreferencesFile;
  directory: string;
  menus: MenuItem[][];
  posted: unknown[];
  reconnects: boolean[];
  work: { status: WorkStatus; server: MaintenanceStatus | null };
  asked: WorkStatus[];
  choice: { value: UpdateChoice };
  postResult: { value: unknown };
  checks: number;
  main: ManagedWindow;
  call: (channel: ShellChannel, window: ManagedWindow, payload?: unknown) => Promise<{ ok: boolean; result?: Record<string, unknown>; error?: { code: string; message: string; details: unknown } }>;
}

function rig(options: { toggleDefault?: boolean; directory?: string } = {}): Rig {
  const directory = options.directory ?? mkdtempSync(join(tmpdir(), "quansio-host-"));
  if (!options.directory) {
    directories.push(directory);
  }
  const factory = new FakeFactory();
  const displays = new FakeDisplays();
  const clock = new Clock();
  const system = new FakeSystem();
  const blocker = new Blocker();
  const updater = new FakeUpdater();
  const preferences = new PreferencesFile(directory);
  const config = options.toggleDefault === false ? { ...CONFIG, toggleDefault: null } : CONFIG;
  const windows = new WindowManager({ factory, platform: "darwin", preload: "/app/preload.cjs", config, displays, log: () => undefined });
  const updates = new UpdateHooks("stable", updater, "http://feed", true, preferences.current().installOnQuit);
  const keepAwake = new KeepAwake(blocker);
  keepAwake.update({ onAcPower: true });
  const work: Rig["work"] = { status: { kind: "idle" }, server: null };
  const asked: WorkStatus[] = [];
  const choice = { value: "later" as UpdateChoice };
  const postResult = { value: { thread_id: THREAD, run_id: RUN, message_id: "msg_1" } as unknown };
  const quit = new QuitGate({ probe: () => Promise.resolve(work.status), ask: (status) => { asked.push(status); return Promise.resolve(false); } });
  const reconnects: boolean[] = [];
  const posted: unknown[] = [];
  const menus: MenuItem[][] = [];
  let checks = 0;
  const shortcuts = new ShortcutRegistry({ platform: "darwin", maxLength: 64, toggleDefault: config.toggleDefault, adapter: system, preferences, onGlobal: (id) => host.onGlobalShortcut(id), log: () => undefined });
  const connectivity = new ConnectivityMonitor({ graceMs: config.offlineGraceMs, minGapMs: config.reconnectMinGapMs, reconnect: (force) => reconnects.push(force), streamOpen: () => false, now: () => clock.now, setTimer: clock.setTimer, clearTimer: clock.clearTimer });
  const activity = new ActivityTracker({ maxItems: config.activityMaxItems, completedTtlMs: config.activityCompletedTtlMs, now: () => clock.now });
  const updateGate = new UpdateGate({
    updates,
    probe: () => Promise.resolve(work.status),
    ask: (status) => { asked.push(status); return Promise.resolve(choice.value); },
    allowQuit: () => quit.allow(),
    revokeQuit: () => quit.revoke(),
    pollMs: config.updateIdlePollMs,
    setTimer: clock.setTimer,
    clearTimer: clock.clearTimer,
  });
  const host: ShellHost = new ShellHost({
    platform: "darwin",
    config,
    preferences,
    windows,
    bridge: {
      postMessage: (raw) => { posted.push(raw); return postResult.value instanceof Error ? Promise.reject(postResult.value) : Promise.resolve(postResult.value); },
      signedIn: () => true,
      streamOpen: () => false,
      resumeStream: (force) => { reconnects.push(force); return true; },
      maintenance: () => Promise.resolve(work.server),
    },
    shortcuts,
    keepAwake,
    quit,
    updateGate,
    updates,
    connectivity,
    activity,
    displays,
    pointer: () => ({ x: 100, y: 100 }),
    setMenu: (items) => menus.push([...items]),
    checkForUpdates: () => { checks += 1; },
    quitApp: () => undefined,
    log: () => undefined,
  });
  host.start();
  const main = windows.openMain();
  const handlers = host.handlers();
  return {
    host, windows, factory, displays, clock, system, blocker, updater, updates, quit, keepAwake, preferences, directory, menus, posted, reconnects, work, asked, choice, postResult,
    get checks() { return checks; },
    main,
    call: (channel, window, payload = {}) => handlers[channel](window, payload) as ReturnType<Rig["call"]>,
  };
}

const frame = (type: string, aggregate: string, payload: Record<string, unknown> = {}): EventFrame => ({ kind: "event", cursor: "c", event: { type, aggregate_id: aggregate, payload } });

async function settle(): Promise<void> {
  for (let i = 0; i < 8; i += 1) {
    await Promise.resolve();
  }
}

describe("every shell channel validates its payload", () => {
  it("refuses a field it does not know, and the bad payloads of the rest, as VALIDATION_SCHEMA", async () => {
    const r = rig();
    const popout = r.windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    if (!popout.ok) {
      throw new Error("no pop-out");
    }
    const quick = r.windows.quickEntry();
    const indicator = r.windows.indicator();
    const callers: Record<ShellChannel, ManagedWindow | null> = {
      "shell.window.context": r.main,
      "shell.pending.consume": r.main,
      "shell.connectivity.report": r.main,
      "shell.shortcuts.get": r.main,
      "shell.shortcuts.set": r.main,
      "shell.shortcuts.reset": r.main,
      "shell.recording.begin": r.main,
      "shell.recording.end": r.main,
      "shell.preferences.get": r.main,
      "shell.preferences.set": r.main,
      "shell.popout.open": r.main,
      "shell.popout.pin": popout.window,
      "shell.window.close": popout.window,
      "shell.quickEntry.submit": quick,
      "shell.activity.list": r.main,
      "shell.activity.dismiss": r.main,
      "shell.activity.open": r.main,
      "shell.indicator.interactive": indicator,
      "shell.indicator.moveTo": indicator,
      "update.install": r.main,
    };
    for (const channel of SHELL_CHANNELS) {
      const window = callers[channel];
      expect(window, channel).not.toBeNull();
      if (window) {
        const answer = await r.call(channel, window, { bogus: "/etc/passwd" });
        expect(answer, channel).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
        expect(answer.error?.message, channel).toMatch(/bogus/);
      }
    }
    expect(r.posted).toEqual([]);
    const bad: [ShellChannel, unknown][] = [
      ["shell.connectivity.report", { online: "yes" }],
      ["shell.shortcuts.set", { id: "find", binding: 5 }],
      ["shell.shortcuts.set", { id: "../../x", binding: null }],
      ["shell.preferences.set", { keepAwake: "no" }],
      ["shell.popout.open", { threadId: "thr_x" }],
      ["shell.popout.open", { threadId: THREAD, tab: "elsewhere" }],
      ["shell.popout.open", { threadId: THREAD, x: 1.5, y: 2 }],
      ["shell.activity.dismiss", { key: "" }],
      ["shell.indicator.moveTo", { x: "1", y: 2 }],
      ["update.install", { mode: "force" }],
      ["shell.window.context", { extra: 1 }],
    ];
    for (const [channel, payload] of bad) {
      const window = callers[channel];
      const answer = window ? await r.call(channel, window, payload) : null;
      expect(answer, `${channel} ${JSON.stringify(payload)}`).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    }
  });
});

describe("window context and events that arrive early", () => {
  it("tells a window what it is, from the type main recorded", async () => {
    const r = rig();
    expect((await r.call("shell.window.context", r.main)).result).toMatchObject({ type: "main", platform: "darwin" });
    const popout = r.windows.openPopout({ threadId: THREAD, tab: "inspector", sidebar: true });
    if (!popout.ok) {
      throw new Error("no pop-out");
    }
    expect((await r.call("shell.window.context", popout.window)).result).toMatchObject({ type: "popout", launch: { type: "popout", threadId: THREAD, tab: "inspector", sidebar: true } });
  });

  it("queues a notification click and a link until the page pulls, then pushes", async () => {
    const r = rig();
    r.host.onNoticeClicked({ notificationId: "ntf_1", title: "t", body: "b", ref: { thread_id: THREAD } });
    expect(r.factory.ofType("main")[0]?.sent).toEqual([]);
    expect(r.factory.ofType("main")[0]?.calls).toContain("show");
    const pulled = await r.call("shell.pending.consume", r.main);
    expect(pulled.result?.["events"]).toEqual([{ channel: "shell.command", payload: { kind: "open-thread", threadId: THREAD } }]);
    r.host.command({ kind: "find" });
    expect(r.factory.ofType("main")[0]?.sent).toEqual([{ channel: "shell.command", payload: { kind: "find" } }]);
    // A click on a notification that points at nothing opens the app without navigating it.
    r.host.onNoticeClicked({ notificationId: "ntf_2", title: "t", body: "b", ref: {} });
    expect(r.factory.ofType("main")[0]?.sent.at(-1)).toEqual({ channel: "shell.command", payload: { kind: "find" } });
  });
});

describe("activity, keep-awake and the indicator", () => {
  it("a Turn in flight holds the machine awake on AC power, and finishing or unplugging releases it", () => {
    const r = rig();
    r.host.onStreamFrame(frame("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN }));
    expect(r.keepAwake.holding).toBe(true);
    r.host.onPowerSource(false);
    expect(r.keepAwake.holding).toBe(false);
    r.host.onPowerSource(true);
    expect(r.keepAwake.holding).toBe(true);
    r.host.onStreamFrame(frame("turn.completed", "trn_1", { turn_id: "trn_1", run_id: RUN }));
    expect(r.keepAwake.holding).toBe(false);
    expect(r.blocker.started).toBe(2);
    expect(r.blocker.stopped).toHaveLength(2);
  });

  it("the person's setting turns keep-awake off and on, and survives a restart", async () => {
    const r = rig();
    r.host.onStreamFrame(frame("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN }));
    expect(r.keepAwake.holding).toBe(true);
    const off = await r.call("shell.preferences.set", r.main, { keepAwake: false });
    expect(off.result).toMatchObject({ keepAwake: false });
    expect(r.keepAwake.holding).toBe(false);
    const again = rig({ directory: r.directory });
    expect(again.host.keepAwakeEnabled()).toBe(false);
    again.host.start();
    again.host.onStreamFrame(frame("turn.started", "trn_9", { turn_id: "trn_9", run_id: RUN }));
    expect(again.keepAwake.holding).toBe(false);
  });

  it("shows the indicator without focus when something needs the person and they are not looking, and hides it when they look", () => {
    const r = rig();
    expect(r.factory.ofType("indicator")).toHaveLength(0);
    r.host.onStreamFrame(frame("approval.requested", "apr_1", { approval_id: "apr_1", run_id: RUN }));
    const indicator = r.factory.ofType("indicator")[0];
    expect(indicator?.calls).toContain("showInactive");
    expect(indicator?.visible).toBe(true);
    expect(indicator?.focused).toBe(false);
    // The main window gains focus: the list is already in front of the person.
    r.factory.ofType("main")[0]?.emit("focus");
    expect(indicator?.visible).toBe(false);
    r.factory.ofType("main")[0]?.emit("blur");
    expect(indicator?.visible).toBe(true);
    // Nothing left: hidden.
    r.host.onStreamFrame(frame("approval.granted", "apr_1", { approval_id: "apr_1" }));
    expect(indicator?.visible).toBe(false);
  });

  it("pushes the ordered list to the indicator and the main window", () => {
    const r = rig();
    r.host.onStreamFrame(frame("run.succeeded", "run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"));
    r.host.onStreamFrame(frame("approval.requested", "apr_1", { approval_id: "apr_1" }));
    const pushed = r.factory.ofType("indicator")[0]?.sent.at(-1)?.payload as { items: { kind: string }[] };
    expect(pushed.items.map((item) => item.kind)).toEqual(["needs_approval", "completed"]);
    expect(r.factory.ofType("main")[0]?.sent.some((message) => message.channel === "shell.activity")).toBe(true);
  });

  it("opening an item brings the main window forward on it; dismissing removes it", async () => {
    const r = rig();
    r.host.onStreamFrame(frame("approval.requested", "apr_1", { approval_id: "apr_1", run_id: RUN }));
    await r.call("shell.window.context", r.main);
    await r.call("shell.pending.consume", r.main);
    const indicator = r.windows.indicator();
    const opened = await r.call("shell.activity.open", indicator ?? r.main, { key: "approval:apr_1" });
    expect(opened.ok).toBe(true);
    expect(r.factory.ofType("main")[0]?.sent.at(-1)).toEqual({ channel: "shell.command", payload: { kind: "open-approval", approvalId: "apr_1" } });
    expect((await r.call("shell.activity.open", r.main, { key: "approval:gone" })).error?.code).toBe("NOT_FOUND");
    expect((await r.call("shell.activity.dismiss", r.main, { key: "approval:apr_1" })).result).toEqual({ dismissed: true });
    expect((await r.call("shell.activity.list", r.main)).result).toEqual({ items: [] });
  });

  it("only the list and the anchor take the mouse: the page says when the pointer is over them", async () => {
    const r = rig();
    r.host.onStreamFrame(frame("run.failed", RUN));
    const indicator = r.windows.single("indicator");
    if (!indicator) {
      throw new Error("no indicator");
    }
    const handle = r.factory.ofType("indicator")[0];
    expect(handle?.ignoreMouse).toEqual({ ignore: true, forward: true });
    await r.call("shell.indicator.interactive", indicator, { active: true });
    expect(handle?.ignoreMouse).toEqual({ ignore: false, forward: true });
    await r.call("shell.indicator.interactive", indicator, { active: false });
    expect(handle?.ignoreMouse).toEqual({ ignore: true, forward: true });
  });

  it("is placed in the top-right corner, remembered per display when dragged, and put back inside a display that changed", async () => {
    const r = rig();
    r.host.onStreamFrame(frame("run.failed", RUN));
    const indicator = r.windows.single("indicator");
    const handle = r.factory.ofType("indicator")[0];
    if (!indicator || !handle) {
      throw new Error("no indicator");
    }
    expect(handle.bounds).toMatchObject({ x: LEFT.workArea.x + LEFT.workArea.width - INDICATOR_SIZE.width - 16, y: LEFT.workArea.y + 16 });
    const moved = await r.call("shell.indicator.moveTo", indicator, { x: 2400, y: 200 });
    expect(moved.result).toEqual({ x: 2400, y: 200 });
    expect(handle.bounds).toMatchObject({ x: 2400, y: 200, width: INDICATOR_SIZE.width });
    expect(r.preferences.current().indicator).toEqual({ [RIGHT.key]: { x: 480, y: 200 } });
    // Dragged off the screen: clamped before it is saved.
    const clamped = await r.call("shell.indicator.moveTo", indicator, { x: 99999, y: -5000 });
    expect(clamped.result).toEqual({ x: RIGHT.workArea.x + RIGHT.workArea.width - INDICATOR_SIZE.width, y: RIGHT.workArea.y });
    // After a restart it is on the display it was left on.
    const again = rig({ directory: r.directory });
    again.host.onStreamFrame(frame("run.failed", RUN));
    expect(again.factory.ofType("indicator")[0]?.bounds).toMatchObject({ x: RIGHT.workArea.x + RIGHT.workArea.width - INDICATOR_SIZE.width, y: RIGHT.workArea.y });
    // The display it was left on is unplugged: the default corner of the primary.
    again.displays.attached = [LEFT];
    again.host.onDisplaysChanged();
    expect(again.factory.ofType("indicator")[0]?.bounds).toMatchObject({ x: LEFT.workArea.x + LEFT.workArea.width - INDICATOR_SIZE.width - 16 });
  });
});

describe("quick entry", () => {
  it("starts a new thread through the same PostMessage path and hides itself", async () => {
    const r = rig();
    r.host.onGlobalShortcut("quick_entry");
    const quick = r.windows.single("quick_entry");
    expect(quick?.handle.isVisible()).toBe(true);
    if (!quick) {
      throw new Error("no quick entry");
    }
    const answer = await r.call("shell.quickEntry.submit", quick, { content: "Draft the launch note" });
    expect(answer).toEqual({ ok: true, result: { thread_id: THREAD, run_id: RUN, message_id: "msg_1" } });
    // A new thread (no thread id), the text, and nothing else: no attachments, no mentions, no scope.
    expect(r.posted).toEqual([{ threadId: null, content: "Draft the launch note" }]);
    expect(quick.handle.isVisible()).toBe(false);
  });

  it("can address a teammate, and refuses anything that is not one", async () => {
    const r = rig();
    const quick = r.windows.quickEntry();
    if (!quick) {
      throw new Error("no quick entry");
    }
    await r.call("shell.quickEntry.submit", quick, { content: "hi", teammateId: "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    expect(r.posted).toEqual([{ threadId: null, content: "hi", teammateId: "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }]);
    for (const payload of [{ content: "hi", teammateId: "../../etc" }, { content: "" }, { content: "x".repeat(16_001) }, { content: "hi", threadId: THREAD }, { content: "hi", attachments: ["art_1"] }]) {
      expect(await r.call("shell.quickEntry.submit", quick, payload), JSON.stringify(payload).slice(0, 60)).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    }
    expect(r.posted).toHaveLength(1);
  });

  it("shows the server's refusal as data and stays open so nothing typed is lost", async () => {
    const r = rig();
    r.host.onGlobalShortcut("quick_entry");
    const quick = r.windows.single("quick_entry");
    if (!quick) {
      throw new Error("no quick entry");
    }
    r.postResult.value = new Error("budget exhausted");
    const answer = await r.call("shell.quickEntry.submit", quick, { content: "hi" });
    expect(answer).toMatchObject({ ok: false, error: { code: "INTERNAL", message: "budget exhausted" } });
    expect(quick.handle.isVisible()).toBe(true);
  });

  it("hides when it loses focus, and closing it hides rather than destroys it", async () => {
    const r = rig();
    r.host.onGlobalShortcut("quick_entry");
    const handle = r.factory.ofType("quick_entry")[0];
    handle?.emit("blur");
    expect(handle?.visible).toBe(false);
    r.host.onGlobalShortcut("quick_entry");
    const quick = r.windows.single("quick_entry");
    if (!quick) {
      throw new Error("no quick entry");
    }
    await r.call("shell.window.close", quick);
    expect(handle?.visible).toBe(false);
    expect(handle?.destroyed).toBe(false);
  });
});

describe("pop-outs", () => {
  it("opens, keeps on top, and closes through its own channels", async () => {
    const r = rig();
    const opened = await r.call("shell.popout.open", r.main, { threadId: THREAD, x: 700, y: 200 });
    expect(opened).toEqual({ ok: true, result: { reused: false } });
    const handle = r.factory.ofType("popout")[0];
    expect(handle?.spec.launch).toEqual({ type: "popout", threadId: THREAD, tab: "conversation", sidebar: false });
    expect(handle?.spec.options).toMatchObject({ x: 420, y: 176 });
    expect(await r.call("shell.popout.open", r.main, { threadId: THREAD })).toEqual({ ok: true, result: { reused: true } });
    const popout = r.windows.ofType("popout")[0];
    if (!popout) {
      throw new Error("no pop-out");
    }
    expect((await r.call("shell.popout.pin", popout, { pinned: true })).result).toEqual({ pinned: true });
    expect(handle?.alwaysOnTop).toBe(true);
    await r.call("shell.window.close", popout);
    expect(handle?.destroyed).toBe(true);
  });

  it("refuses one coordinate without the other, and bad launch data, and opens nothing", async () => {
    const r = rig();
    expect(await r.call("shell.popout.open", r.main, { threadId: THREAD, x: 1 })).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    expect(await r.call("shell.popout.open", r.main, { threadId: "thr_nope" })).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    expect(r.factory.ofType("popout")).toHaveLength(0);
    // The menu's "open thread in a new window" goes to the renderer, which names the thread.
    r.host.command({ kind: "popout-current-thread" });
    expect(r.factory.ofType("popout")).toHaveLength(0);
  });
});

describe("shortcuts through the channels", () => {
  it("applies a free binding, answers a conflict and a system refusal with typed errors, and keeps the previous binding each time", async () => {
    const r = rig();
    const snapshot = (await r.call("shell.shortcuts.get", r.main)).result as { shortcuts: { id: string; binding: string | null; registered: boolean }[]; platform: string };
    expect(snapshot.platform).toBe("darwin");
    expect(snapshot.shortcuts.find((entry) => entry.id === "toggle_window")).toMatchObject({ binding: "mod+alt+q", registered: true });
    expect(snapshot.shortcuts.find((entry) => entry.id === "quick_entry")).toMatchObject({ binding: null, registered: false });

    expect((await r.call("shell.shortcuts.set", r.main, { id: "quick_entry", binding: "mod+shift+space" })).ok).toBe(true);
    expect(r.system.held.has("Command+Shift+Space")).toBe(true);

    const conflict = await r.call("shell.shortcuts.set", r.main, { id: "quick_entry", binding: "mod+k" });
    expect(conflict).toMatchObject({ ok: false, error: { code: "conflict", details: { with: "find" } } });
    const invalid = await r.call("shell.shortcuts.set", r.main, { id: "quick_entry", binding: "Mod+K" });
    expect(invalid).toMatchObject({ ok: false, error: { code: "invalid", details: { problem: "not_lower_case" } } });
    r.system.declined.add("Command+Shift+F10");
    const declined = await r.call("shell.shortcuts.set", r.main, { id: "quick_entry", binding: "mod+shift+f10" });
    expect(declined).toMatchObject({ ok: false, error: { code: "registration_failed" } });
    // The binding from before the refusals is the one in force, and still registered.
    const after = (await r.call("shell.shortcuts.get", r.main)).result as { shortcuts: { id: string; binding: string | null; registered: boolean }[] };
    expect(after.shortcuts.find((entry) => entry.id === "quick_entry")).toMatchObject({ binding: "mod+shift+space", registered: true });
    expect([...r.system.held.keys()].sort()).toEqual(["Command+Option+Q", "Command+Shift+Space"]);
    expect((await r.call("shell.shortcuts.reset", r.main, { id: "quick_entry" })).ok).toBe(true);
    expect(r.system.held.has("Command+Shift+Space")).toBe(false);
  });

  it("a global shortcut toggles the main window and summons quick entry", () => {
    const r = rig();
    const main = r.factory.ofType("main")[0];
    if (!main) {
      throw new Error("no main window");
    }
    main.visible = true;
    main.focused = true;
    r.system.held.get("Command+Option+Q")?.();
    expect(main.visible).toBe(false);
    r.system.held.get("Command+Option+Q")?.();
    expect(main.visible).toBe(true);
    expect(main.calls.at(-1)).toBe("focus");
    r.host.onGlobalShortcut("quick_entry");
    expect(r.factory.ofType("quick_entry")[0]?.visible).toBe(true);
  });

  it("recording pauses the shortcuts and the menu, and ends by itself when the window loses focus", async () => {
    const r = rig();
    expect(r.system.held.size).toBe(1);
    const menusBefore = r.menus.length;
    expect(await r.call("shell.recording.begin", r.main)).toMatchObject({ ok: true });
    expect(r.system.held.size).toBe(0);
    const paused = r.menus.at(-1) ?? [];
    expect(JSON.stringify(paused)).not.toContain("accelerator");
    expect(r.menus.length).toBeGreaterThan(menusBefore);
    r.factory.ofType("main")[0]?.emit("blur");
    expect(r.system.held.size).toBe(1);
    expect(JSON.stringify(r.menus.at(-1))).toContain("accelerator");
    expect(r.factory.ofType("main")[0]?.sent.at(-1)).toEqual({ channel: "shell.recording.ended", payload: { reason: "blur" } });
    // Another window cannot end a recording it does not own.
    const popout = r.windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    await r.call("shell.recording.begin", r.main);
    if (popout.ok) {
      expect((await r.call("shell.recording.end", popout.window)).result).toEqual({ ended: false });
    }
    expect((await r.call("shell.recording.end", r.main)).result).toEqual({ ended: true });
    expect(r.system.held.size).toBe(1);
  });

  it("the menu is rebuilt when a window gains focus or a binding changes", async () => {
    const r = rig();
    const count = r.menus.length;
    r.factory.ofType("main")[0]?.emit("focus");
    expect(r.menus.length).toBe(count + 1);
    await r.call("shell.shortcuts.set", r.main, { id: "find", binding: "mod+shift+f" });
    expect(JSON.stringify(r.menus.at(-1))).toContain("Command+Shift+F");
  });
});

describe("connectivity and updates through the channels", () => {
  it("the renderer's offline report is held for the grace, then the return reconnects through the one reconnect", async () => {
    const r = rig();
    await r.call("shell.connectivity.report", r.main, { online: false });
    r.clock.advance(9_000);
    await r.call("shell.connectivity.report", r.main, { online: true });
    r.clock.advance(60_000);
    expect(r.reconnects).toEqual([]);
    await r.call("shell.connectivity.report", r.main, { online: false });
    r.clock.advance(10_000);
    expect(await r.call("shell.connectivity.report", r.main, { online: true })).toMatchObject({ ok: true, result: { state: "online" } });
    expect(r.reconnects).toEqual([true]);
    r.host.onResume();
    expect(r.reconnects).toEqual([true]);
    r.clock.advance(5_000);
    r.host.onResume();
    expect(r.reconnects).toEqual([true, true]);
  });

  it("an update restarts only when idle, or when the person says so", async () => {
    const r = rig();
    r.updater.emit("update-downloaded", {}, "", "2.0.0");
    r.work.status = { kind: "busy", activeTurns: 1, inflightActions: 0 };
    expect((await r.call("update.install", r.main, { mode: "now" })).result).toEqual({ kind: "deferred" });
    expect(r.updater.installs).toBe(0);
    expect(r.asked).toHaveLength(1);
    r.choice.value = "install";
    expect((await r.call("update.install", r.main, { mode: "now" })).result).toEqual({ kind: "installing" });
    expect(r.updater.installs).toBe(1);
  });

  it("install when idle waits for an idle reading, and the person can cancel it", async () => {
    const r = rig();
    r.updater.emit("update-downloaded", {}, "", "2.0.0");
    r.work.status = { kind: "busy", activeTurns: 1, inflightActions: 0 };
    expect((await r.call("update.install", r.main, { mode: "when_idle" })).result).toEqual({ kind: "waiting_for_idle" });
    r.clock.advance(30_000);
    await settle();
    expect(r.updater.installs).toBe(0);
    r.work.status = { kind: "idle" };
    r.clock.advance(30_000);
    await settle();
    expect(r.updater.installs).toBe(1);
    // A cancel before idle installs nothing.
    const other = rig();
    other.updater.emit("update-downloaded", {}, "", "2.0.0");
    other.work.status = { kind: "busy", activeTurns: 1, inflightActions: 0 };
    await other.call("update.install", other.main, { mode: "when_idle" });
    await other.call("update.install", other.main, { mode: "cancel_when_idle" });
    other.work.status = { kind: "idle" };
    other.clock.advance(120_000);
    await settle();
    expect(other.updater.installs).toBe(0);
  });

  it("install-when-idle as a preference starts when a download finishes", async () => {
    const r = rig();
    await r.call("shell.preferences.set", r.main, { installWhenIdle: true });
    r.work.status = { kind: "idle" };
    r.host.onUpdateStatus({ state: "downloaded", channel: "stable", version: "2.0.0" });
    // Not downloaded as far as the hooks know: nothing to wait for.
    expect(r.updater.installs).toBe(0);
    r.updater.emit("update-downloaded", {}, "", "2.0.0");
    r.host.onUpdateStatus({ state: "downloaded", channel: "stable", version: "2.0.0" });
    r.clock.advance(30_000);
    await settle();
    expect(r.updater.installs).toBe(1);
  });

  it("install-on-quit is the person's choice and is remembered", async () => {
    const r = rig();
    await r.call("shell.preferences.set", r.main, { installOnQuit: true });
    expect(r.updates.installOnQuit).toBe(true);
    expect(r.preferences.current().installOnQuit).toBe(true);
    const again = rig({ directory: r.directory });
    expect(again.updates.installOnQuit).toBe(true);
  });

  it("answers the work probe from the server's status and the stream together", async () => {
    const r = rig();
    r.work.server = { idle: true, activeTurns: 0, inflightActions: 0, openTerminals: 0, asOf: "x" };
    expect(await r.host.probe()).toEqual({ kind: "idle" });
    r.host.onStreamFrame(frame("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN }));
    expect(await r.host.probe()).toEqual({ kind: "busy", activeTurns: 1, inflightActions: 0 });
    r.host.onStreamFrame(frame("turn.completed", "trn_1", { turn_id: "trn_1", run_id: RUN }));
    r.work.server = { idle: false, activeTurns: 0, inflightActions: 2, openTerminals: 0, asOf: "x" };
    expect(await r.host.probe()).toEqual({ kind: "busy", activeTurns: 0, inflightActions: 2 });
    r.work.server = null;
    expect(await r.host.probe()).toEqual({ kind: "unknown" });
    expect(r.checks).toBe(0);
    r.host.checkForUpdates();
    expect(r.checks).toBe(1);
    expect(OTHER_THREAD).not.toBe(THREAD);
  });
});
