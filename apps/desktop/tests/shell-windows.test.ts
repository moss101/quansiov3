/**
 * Windows (ADP-004, DOMAIN.md 13.6 B): every window type is built on the renderer security baseline, a pop-out opens
 * only with validated launch data, events sent before a renderer subscribes wait in a bounded per-window queue, and
 * the indicator's position is remembered per display.
 */

import { describe, expect, it } from "vitest";

import { RENDERER_WEB_PREFERENCES, navigationAllowed } from "../src/main/security.js";
import { parseShellCommand } from "../src/shell-command.js";
import { parseShellConfig } from "../src/main/shell-config.js";
import { INDICATOR_SIZE, PendingQueue, POPOUT_SIZE, QUICK_ENTRY_SIZE, WINDOW_TYPES, WindowManager, clampOrigin, defaultIndicatorOrigin, indicatorOrigin, indicatorPosition, specMeetsBaseline, windowSpec, type LaunchData } from "../src/main/windows.js";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { FakeDisplays, FakeFactory, LEFT, OTHER_THREAD, RIGHT, THREAD } from "./shell-fakes.js";

const CONFIG = parseShellConfig(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "config", "desktop-shell.yaml"), "utf8"));

const LAUNCHES: LaunchData[] = [
  { type: "main" },
  { type: "popout", threadId: THREAD, tab: "conversation", sidebar: false },
  { type: "quick_entry" },
  { type: "indicator" },
];

function manager(config = CONFIG): { windows: WindowManager; factory: FakeFactory; displays: FakeDisplays; log: string[] } {
  const factory = new FakeFactory();
  const displays = new FakeDisplays();
  const log: string[] = [];
  return { windows: new WindowManager({ factory, platform: "darwin", preload: "/app/preload.cjs", config, displays, log: (line) => log.push(line) }), factory, displays, log };
}

describe("every window type", () => {
  it("is built on the one sandboxed baseline and loads only the app: scheme", () => {
    expect(WINDOW_TYPES).toEqual(["main", "popout", "quick_entry", "indicator"]);
    for (const launch of LAUNCHES) {
      for (const platform of ["darwin", "win32", "linux"]) {
        const spec = windowSpec(launch, { platform, preload: "/app/preload.cjs" });
        const web = spec.options.webPreferences;
        expect(web.sandbox, launch.type).toBe(true);
        expect(web.contextIsolation, launch.type).toBe(true);
        expect(web.nodeIntegration, launch.type).toBe(false);
        expect(web.nodeIntegrationInWorker, launch.type).toBe(false);
        expect(web.nodeIntegrationInSubFrames, launch.type).toBe(false);
        expect(web.webSecurity, launch.type).toBe(true);
        expect(web.allowRunningInsecureContent, launch.type).toBe(false);
        expect(web.webviewTag, launch.type).toBe(false);
        expect(web.enableRemoteModule, launch.type).toBe(false);
        expect(web.preload, launch.type).toBe("/app/preload.cjs");
        expect({ ...web, preload: RENDERER_WEB_PREFERENCES.preload }, launch.type).toEqual(RENDERER_WEB_PREFERENCES);
        expect(spec.url.startsWith("app://desktop/"), launch.type).toBe(true);
        expect(navigationAllowed(spec.url), launch.type).toBe(true);
        expect(specMeetsBaseline(spec), launch.type).toBe(true);
      }
    }
  });

  it("is refused by the baseline check when any one setting is weakened", () => {
    const spec = windowSpec({ type: "main" }, { platform: "darwin", preload: "/app/preload.cjs" });
    const weaker = (change: Partial<typeof spec.options.webPreferences>) => ({ ...spec, options: { ...spec.options, webPreferences: { ...spec.options.webPreferences, ...change } } });
    for (const change of [{ sandbox: false }, { contextIsolation: false }, { nodeIntegration: true }, { webSecurity: false }, { allowRunningInsecureContent: true }, { webviewTag: true }, { nodeIntegrationInSubFrames: true }, { preload: "" }]) {
      expect(specMeetsBaseline(weaker(change)), JSON.stringify(change)).toBe(false);
    }
    expect(specMeetsBaseline({ ...spec, url: "https://example.com/" })).toBe(false);
    expect(specMeetsBaseline({ ...spec, url: "app://other/index.html" })).toBe(false);
    expect(specMeetsBaseline({ ...spec, url: "file:///etc/hosts" })).toBe(false);
  });

  it("differs in shape, not in security", () => {
    const quick = windowSpec({ type: "quick_entry" }, { platform: "darwin", preload: "p" });
    expect(quick.options).toMatchObject({ ...QUICK_ENTRY_SIZE, frame: false, alwaysOnTop: true, skipTaskbar: true });
    expect(quick.behaviour.hideOnBlur).toBe(true);
    const indicator = windowSpec({ type: "indicator" }, { platform: "darwin", preload: "p" });
    expect(indicator.options).toMatchObject({ ...INDICATOR_SIZE, transparent: true, frame: false, alwaysOnTop: true, focusable: false, hasShadow: false, skipTaskbar: true });
    expect(indicator.behaviour).toEqual({ clickThrough: true, visibleOnAllWorkspaces: true, hiddenInMissionControl: true, hideOnBlur: false });
    const popout = windowSpec({ type: "popout", threadId: THREAD, tab: "inspector", sidebar: true }, { platform: "win32", preload: "p", pinned: true, origin: { x: 10.4, y: 20.6 } });
    expect(popout.options).toMatchObject({ ...POPOUT_SIZE, alwaysOnTop: true, x: 10, y: 21 });
    expect(popout.options.titleBarStyle).toBeUndefined();
    expect(windowSpec({ type: "main" }, { platform: "darwin", preload: "p" }).options.titleBarStyle).toBe("hiddenInset");
  });
});

describe("the pop-out", () => {
  it("opens with validated launch data and shows the thread it was given", () => {
    const { windows, factory } = manager();
    const result = windows.openPopout({ threadId: THREAD, tab: "inspector", sidebar: true });
    expect(result).toMatchObject({ ok: true, reused: false });
    expect(factory.windows).toHaveLength(1);
    const window = factory.windows[0];
    expect(window?.spec.launch).toEqual({ type: "popout", threadId: THREAD, tab: "inspector", sidebar: true });
    expect(window?.loaded).toBe("app://desktop/index.html?w=popout");
    expect(window?.calls).toContain("show");
    expect(windows.byWebContents(window?.webContentsId ?? 0)?.threadId).toBe(THREAD);
  });

  it("refuses launch data that is not exactly a thread, a tab and a sidebar state, and creates no window", () => {
    const { windows, factory } = manager();
    const bad: unknown[] = [
      { threadId: "thr_short", tab: "conversation", sidebar: false },
      { threadId: THREAD, tab: "settings", sidebar: false },
      { threadId: THREAD, tab: "conversation" },
      { threadId: THREAD, tab: "conversation", sidebar: "yes" },
      { threadId: THREAD, tab: "conversation", sidebar: false, url: "https://example.com" },
      { threadId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", tab: "conversation", sidebar: false },
      null,
      "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    ];
    for (const raw of bad) {
      expect(windows.openPopout(raw), JSON.stringify(raw)).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    }
    expect(factory.windows).toHaveLength(0);
  });

  it("brings an existing pop-out forward instead of opening a second window for the same thread", () => {
    const { windows, factory } = manager();
    windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    const again = windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    expect(again).toMatchObject({ ok: true, reused: true });
    expect(factory.windows).toHaveLength(1);
    windows.openPopout({ threadId: OTHER_THREAD, tab: "conversation", sidebar: false });
    expect(factory.windows).toHaveLength(2);
  });

  it("opens at the drop point, inside the display's work area", () => {
    const { windows, factory } = manager();
    windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false }, { x: 600, y: 200 });
    expect(factory.windows[0]?.spec.options).toMatchObject({ x: 600 - POPOUT_SIZE.width / 2, y: 176 });
    // Dropped at the edge of the right-hand display: pulled back inside it.
    windows.openPopout({ threadId: OTHER_THREAD, tab: "conversation", sidebar: false }, { x: 3190, y: 790 });
    const placed = factory.windows[1]?.spec.options;
    expect(placed?.x).toBe(RIGHT.workArea.x + RIGHT.workArea.width - POPOUT_SIZE.width);
    expect(placed?.y).toBe(RIGHT.workArea.y + RIGHT.workArea.height - POPOUT_SIZE.height);
  });

  it("can be kept on top, only when the configuration allows it and only for a pop-out", () => {
    const { windows, factory } = manager();
    windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    const popout = windows.ofType("popout")[0];
    expect(popout && windows.pinPopout(popout, true)).toBe(true);
    expect(factory.windows[0]?.alwaysOnTop).toBe(true);
    expect(popout && windows.pinPopout(popout, false)).toBe(true);
    expect(factory.windows[0]?.alwaysOnTop).toBe(false);
    const main = windows.openMain();
    expect(windows.pinPopout(main, true)).toBe(false);
    const denied = manager(parseShellConfig(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "config", "desktop-shell.yaml"), "utf8").replace("popout.always_on_top_allowed: true", "popout.always_on_top_allowed: false")));
    denied.windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    const locked = denied.windows.ofType("popout")[0];
    expect(locked && denied.windows.pinPopout(locked, true)).toBe(false);
    expect(denied.factory.windows[0]?.alwaysOnTop).toBe(false);
  });

  it("is forgotten when it closes: no lookup, no delivery", () => {
    const { windows, factory } = manager();
    windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    const window = windows.ofType("popout")[0];
    const id = factory.windows[0]?.webContentsId ?? 0;
    factory.windows[0]?.close();
    expect(windows.byWebContents(id)).toBeNull();
    expect(window?.closed).toBe(true);
    if (window) {
      windows.deliver(window, "shell.command", { kind: "find" });
    }
    expect(factory.windows[0]?.sent).toEqual([]);
    expect(windows.ofType("popout")).toEqual([]);
  });
});

describe("quick entry and the indicator", () => {
  it("quick entry is made on first summon, centred near the top of the pointer's display, and focused", () => {
    const { windows, factory } = manager();
    expect(factory.windows).toHaveLength(0);
    windows.summonQuickEntry({ x: 2500, y: 300 });
    const quick = factory.ofType("quick_entry")[0];
    expect(quick?.visible).toBe(true);
    expect(quick?.focused).toBe(true);
    expect(quick?.bounds.x).toBe(Math.round(RIGHT.workArea.x + (RIGHT.workArea.width - QUICK_ENTRY_SIZE.width) / 2));
    // Summoning again reuses the window.
    windows.summonQuickEntry({ x: 100, y: 100 });
    expect(factory.ofType("quick_entry")).toHaveLength(1);
    expect(quick?.bounds.x).toBe(Math.round(LEFT.workArea.x + (LEFT.workArea.width - QUICK_ENTRY_SIZE.width) / 2));
  });

  it("do not exist when the configuration turns them off", () => {
    const text = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "config", "desktop-shell.yaml"), "utf8").replace("quick_entry.enabled: true", "quick_entry.enabled: false").replace("indicator.enabled: true", "indicator.enabled: false");
    const { windows, factory } = manager(parseShellConfig(text));
    expect(windows.summonQuickEntry({ x: 0, y: 0 })).toBeNull();
    expect(windows.indicator()).toBeNull();
    expect(factory.windows).toHaveLength(0);
  });

  it("the indicator is created click-through, so only what the page marks interactive takes the mouse", () => {
    const { windows, factory } = manager();
    windows.indicator();
    expect(factory.ofType("indicator")[0]?.ignoreMouse).toEqual({ ignore: true, forward: true });
    expect(windows.indicator()).toBe(windows.single("indicator"));
    expect(factory.ofType("indicator")).toHaveLength(1);
  });
});

describe("events sent before a renderer subscribes", () => {
  it("wait per window, up to the cap, oldest dropped first, and arrive in order when the page pulls", () => {
    const { windows, factory } = manager();
    const main = windows.openMain();
    const other = windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    for (let i = 0; i < 70; i += 1) {
      windows.deliver(main, "shell.command", { kind: "open-run", runId: `run_${String(i)}` });
    }
    expect(factory.windows[0]?.sent).toEqual([]);
    expect(main.pending.size).toBe(64);
    // The other window's queue is its own.
    expect(other.ok && other.window.pending.size).toBe(0);
    const taken = windows.consumePending(main);
    expect(taken).toHaveLength(64);
    expect(taken[0]?.payload).toEqual({ kind: "open-run", runId: "run_6" });
    expect(taken[63]?.payload).toEqual({ kind: "open-run", runId: "run_69" });
    expect(main.pending.size).toBe(0);
    // Once it has pulled, events are pushed straight to it.
    windows.deliver(main, "shell.command", { kind: "find" });
    expect(factory.windows[0]?.sent).toEqual([{ channel: "shell.command", payload: { kind: "find" } }]);
  });

  it("a page that navigates is a new page: events wait for it again", () => {
    const { windows, factory } = manager();
    const main = windows.openMain();
    windows.consumePending(main);
    factory.windows[0]?.emit("navigated");
    expect(main.subscribed).toBe(false);
    windows.deliver(main, "deepLink.intent", { kind: "open-thread", threadId: THREAD });
    expect(main.pending.size).toBe(1);
  });

  it("are cleared when the window is destroyed", () => {
    const { windows, factory } = manager();
    const main = windows.openMain();
    windows.deliver(main, "shell.command", { kind: "find" });
    expect(main.pending.size).toBe(1);
    factory.windows[0]?.close();
    expect(main.pending.size).toBe(0);
  });

  it("the queue itself is bounded by its cap", () => {
    const queue = new PendingQueue(2);
    queue.push({ channel: "a", payload: 1 });
    queue.push({ channel: "a", payload: 2 });
    queue.push({ channel: "a", payload: 3 });
    expect(queue.drain().map((event) => event.payload)).toEqual([2, 3]);
    expect(queue.size).toBe(0);
  });

  it("broadcast reaches only windows of the named types, and no destroyed one", () => {
    const { windows, factory } = manager();
    windows.openMain();
    windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
    windows.quickEntry();
    windows.broadcast(["main", "popout"], "stream.state", "open");
    expect(factory.ofType("main")[0]?.sent).toHaveLength(1);
    expect(factory.ofType("popout")[0]?.sent).toHaveLength(1);
    expect(factory.ofType("quick_entry")[0]?.sent).toHaveLength(0);
  });
});

describe("the indicator's position", () => {
  it("starts at the top-right of the work area and stays inside it", () => {
    const origin = defaultIndicatorOrigin(LEFT.workArea);
    expect(origin).toEqual({ x: 1920 - INDICATOR_SIZE.width - 16, y: 25 + 16 });
    expect(clampOrigin({ x: -500, y: -500 }, INDICATOR_SIZE, LEFT.workArea)).toEqual({ x: 0, y: 25 });
    expect(clampOrigin({ x: 99999, y: 99999 }, INDICATOR_SIZE, LEFT.workArea)).toEqual({ x: 1920 - INDICATOR_SIZE.width, y: 25 + 1055 - INDICATOR_SIZE.height });
  });

  it("is remembered per display, relative to that display", () => {
    const onRight = indicatorPosition(RIGHT, { x: 2000, y: 100 });
    expect(onRight).toMatchObject({ key: "d2", position: { x: 80, y: 100 }, origin: { x: 2000, y: 100 } });
    const saved = { [onRight.key]: onRight.position };
    expect(indicatorOrigin(RIGHT, saved)).toEqual({ x: 2000, y: 100 });
    // The other display has none saved and gets the default.
    expect(indicatorOrigin(LEFT, saved)).toEqual(defaultIndicatorOrigin(LEFT.workArea));
    // A display that came back smaller pulls the saved position inside it.
    const smaller = { ...RIGHT, bounds: { ...RIGHT.bounds, width: 600 }, workArea: { ...RIGHT.workArea, width: 600 } };
    const clamped = indicatorOrigin(smaller, { d2: { x: 1100, y: 100 } });
    expect(clamped.x).toBe(smaller.workArea.x + 600 - INDICATOR_SIZE.width);
  });

  it("a dragged position beyond a display is clamped before it is saved", () => {
    const placed = indicatorPosition(LEFT, { x: -300, y: 5000 });
    expect(placed.origin).toEqual({ x: 0, y: 25 + 1055 - INDICATOR_SIZE.height });
    expect(placed.position).toEqual({ x: 0, y: 25 + 1055 - INDICATOR_SIZE.height });
  });
});

describe("shell commands pushed to a page", () => {
  it("are parsed as data: a malformed or unknown command is nothing", () => {
    expect(parseShellCommand({ kind: "new-thread" })).toEqual({ kind: "new-thread" });
    expect(parseShellCommand({ kind: "open-thread", threadId: THREAD })).toEqual({ kind: "open-thread", threadId: THREAD });
    expect(parseShellCommand({ kind: "open-run", runId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" })).toMatchObject({ kind: "open-run" });
    expect(parseShellCommand({ kind: "open-thread", threadId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" })).toBeNull();
    expect(parseShellCommand({ kind: "open-thread" })).toBeNull();
    expect(parseShellCommand({ kind: "rm -rf" })).toBeNull();
    expect(parseShellCommand(null)).toBeNull();
    expect(parseShellCommand([])).toBeNull();
    expect(parseShellCommand("find")).toBeNull();
  });
});
