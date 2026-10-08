/**
 * IPC trust (ADP-004, DOMAIN.md 13.6 B): no window can invoke IPC from an untrusted URL, a window type reaches only its
 * own channels, and a pop-out acts only on its own thread. Every case runs through `registerGuardedHandlers`, the
 * function `app.ts` installs on `ipcMain`, with handlers that record whether they were ever called.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { BRIDGE_CHANNELS, FILE_CHANNELS, PRELOAD_ALLOWLIST, SECRET_CHANNELS, SHELL_CHANNELS, type PreloadChannel } from "../src/preload/bridge.js";
import { IpcGuard, POLICY, registerGuardedHandlers, senderOf, type WindowHandler } from "../src/main/ipc-trust.js";
import { parseShellConfig } from "../src/main/shell-config.js";
import { WindowManager, type ManagedWindow, type WindowType } from "../src/main/windows.js";

import { FakeDisplays, FakeFactory, OTHER_THREAD, THREAD, TRUSTED_URL, ipcEvent } from "./shell-fakes.js";

const CONFIG = parseShellConfig(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "config", "desktop-shell.yaml"), "utf8"));

interface Rig {
  windows: WindowManager;
  factory: FakeFactory;
  log: string[];
  calls: { channel: string; window: WindowType; payload: unknown }[];
  results: Map<string, unknown>;
  invoke: (channel: PreloadChannel, event: unknown, payload?: unknown) => Promise<unknown>;
  ids: Record<WindowType, number>;
  popout: ManagedWindow;
}

function rig(): Rig {
  const factory = new FakeFactory();
  const log: string[] = [];
  const windows = new WindowManager({ factory, platform: "darwin", preload: "/app/preload.cjs", config: CONFIG, displays: new FakeDisplays(), log: (line) => log.push(line) });
  const main = windows.openMain();
  const opened = windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
  if (!opened.ok) {
    throw new Error("the pop-out did not open");
  }
  const quick = windows.quickEntry();
  const indicator = windows.indicator();
  if (!quick || !indicator) {
    throw new Error("a window did not open");
  }
  const calls: Rig["calls"] = [];
  const results = new Map<string, unknown>();
  const handlers = Object.fromEntries(
    PRELOAD_ALLOWLIST.map((channel) => [
      channel,
      ((window: ManagedWindow, payload: unknown) => {
        calls.push({ channel, window: window.type, payload });
        return Promise.resolve(results.get(channel) ?? { ok: true, result: {} });
      }),
    ]),
  ) as Record<PreloadChannel, WindowHandler>;
  const registered = new Map<string, (event: unknown, payload: unknown) => unknown>();
  const registeredChannels = registerGuardedHandlers({ handle: (channel, listener) => registered.set(channel, listener) }, new IpcGuard(windows, (line) => log.push(line)), handlers);
  expect(registeredChannels).toEqual([...PRELOAD_ALLOWLIST]);
  return {
    windows,
    factory,
    log,
    calls,
    results,
    invoke: (channel, event, payload = {}) => Promise.resolve(registered.get(channel)?.(event, payload)),
    ids: { main: main.handle.webContentsId, popout: opened.window.handle.webContentsId, quick_entry: quick.handle.webContentsId, indicator: indicator.handle.webContentsId },
    popout: opened.window,
  };
}

describe("an untrusted sender", () => {
  const untrusted: [string, string | null, unknown][] = [
    ["a remote site", "https://evil.example/", null],
    ["a local web server", "http://127.0.0.1:8080/", null],
    ["a file", "file:///Users/someone/page.html", null],
    ["a data: page", "data:text/html,<script>window.quansio</script>", null],
    ["javascript:", "javascript:alert(1)", null],
    ["about:blank", "about:blank", null],
    ["another app: host", "app://evil/index.html", null],
    ["a lookalike host", "app://desktop.evil.com/index.html", null],
    ["a different scheme with the right host", "https://desktop/index.html", null],
    ["a frame that is gone", null, null],
    ["a subframe of the trusted page", TRUSTED_URL, { url: TRUSTED_URL }],
    ["an unparseable URL", "not a url", null],
  ];

  it("is a logged no-op on every channel: the handler never runs and nothing is answered", async () => {
    const r = rig();
    for (const [why, url, parent] of untrusted) {
      for (const channel of PRELOAD_ALLOWLIST) {
        const answer = await r.invoke(channel, ipcEvent(r.ids.main, url, parent));
        expect(answer, `${why}: ${channel}`).toBeUndefined();
      }
    }
    expect(r.calls).toEqual([]);
    expect(r.log.filter((line) => line.includes("untrusted sender")).length).toBe(untrusted.length * PRELOAD_ALLOWLIST.length);
  });

  it("is refused in every window type, whatever the window is", async () => {
    const r = rig();
    for (const type of ["main", "popout", "quick_entry", "indicator"] as const) {
      for (const channel of PRELOAD_ALLOWLIST) {
        expect(await r.invoke(channel, ipcEvent(r.ids[type], "https://evil.example/")), `${type}: ${channel}`).toBeUndefined();
      }
    }
    expect(r.calls).toEqual([]);
  });

  it("a window the shell did not create is a no-op even from a trusted URL", async () => {
    const r = rig();
    expect(await r.invoke("commands.invoke", ipcEvent(999))).toBeUndefined();
    expect(await r.invoke("commands.invoke", { sender: {}, senderFrame: { url: TRUSTED_URL, parent: null } })).toBeUndefined();
    expect(await r.invoke("commands.invoke", {})).toBeUndefined();
    expect(await r.invoke("commands.invoke", null)).toBeUndefined();
    expect(r.calls).toEqual([]);
    expect(r.log.filter((line) => line.includes("not a window the shell created"))).toHaveLength(2);
    expect(r.log.filter((line) => line.includes("untrusted sender (no frame)"))).toHaveLength(2);
  });

  it("a window that has closed can call nothing", async () => {
    const r = rig();
    const id = r.ids.popout;
    r.factory.windows.find((window) => window.webContentsId === id)?.close();
    expect(await r.invoke("session.state", ipcEvent(id))).toBeUndefined();
    expect(r.calls).toEqual([]);
  });

  it("is read from the event without trusting its shape", () => {
    expect(senderOf(null)).toEqual({ webContentsId: null, frameUrl: null, isMainFrame: false });
    expect(senderOf({ sender: { id: "7" }, senderFrame: { url: 5 } })).toMatchObject({ webContentsId: null, frameUrl: null });
    expect(senderOf(ipcEvent(7))).toEqual({ webContentsId: 7, frameUrl: TRUSTED_URL, isMainFrame: true });
    expect(senderOf(ipcEvent(7, TRUSTED_URL, {})).isMainFrame).toBe(false);
  });
});

describe("a trusted sender", () => {
  it("reaches a handler in its own window, and the handler is told which window", async () => {
    const r = rig();
    const answer = await r.invoke("session.state", ipcEvent(r.ids.main));
    expect(answer).toEqual({ ok: true, result: {} });
    expect(r.calls).toEqual([{ channel: "session.state", window: "main", payload: {} }]);
  });

  it("registers exactly the allowlist (and the policy covers exactly it)", () => {
    expect(Object.keys(POLICY).sort()).toEqual([...PRELOAD_ALLOWLIST].sort());
    expect([...BRIDGE_CHANNELS, ...SHELL_CHANNELS, ...FILE_CHANNELS, ...SECRET_CHANNELS]).toEqual([...PRELOAD_ALLOWLIST]);
  });
});

// The expected reach of each window type, written out here rather than read from the policy under test.
const POPOUT_BRIDGE = ["session.state", "session.me", "session.activeRun", "stream.subscribe", "appearance.set", "update.status", "threads.messages", "threads.view", "questions.list", "questions.answer", "turns.cancel", "runs.timeline", "messages.post", "artifacts.upload", "teammates.list"];
const POPOUT_SHELL = ["shell.window.context", "shell.pending.consume", "shell.popout.pin", "shell.window.close"];
// The composer's attach control (ADP-005): a pop-out may ask for a native dialog for its own thread; folders, edits and watches are main only.
const POPOUT_FILES = ["files.pick", "files.grant.attach", "files.grant.discard", "files.grant.preview"];
const QUICK_BRIDGE = ["session.state", "session.me", "appearance.set", "teammates.list"];
const QUICK_SHELL = ["shell.window.context", "shell.pending.consume", "shell.window.close", "shell.quickEntry.submit"];
const INDICATOR = ["shell.window.context", "shell.pending.consume", "shell.activity.list", "shell.activity.dismiss", "shell.activity.open", "shell.remote.answer", "shell.indicator.interactive", "shell.indicator.moveTo"];
const MAIN_NEVER = ["shell.popout.pin", "shell.window.close", "shell.quickEntry.submit", "shell.indicator.interactive", "shell.indicator.moveTo"];

describe("window types", () => {
  it("a window may call exactly the channels its type has, and no others", async () => {
    const r = rig();
    const expected: Record<WindowType, Set<string>> = {
      main: new Set(PRELOAD_ALLOWLIST.filter((channel) => !MAIN_NEVER.includes(channel))),
      popout: new Set([...POPOUT_BRIDGE, ...POPOUT_SHELL, ...POPOUT_FILES]),
      quick_entry: new Set([...QUICK_BRIDGE, ...QUICK_SHELL]),
      indicator: new Set(INDICATOR),
    };
    for (const type of ["main", "popout", "quick_entry", "indicator"] as const) {
      for (const channel of PRELOAD_ALLOWLIST) {
        r.calls.length = 0;
        // A pop-out is given the payload that keeps it on its thread, so only the window type is under test.
        const payload = type === "popout" ? (channel === "questions.list" ? { threadId: THREAD } : { threadId: THREAD, questionId: "q_known", runId: "run_known" }) : {};
        r.popout.known.questions.add("q_known");
        r.popout.known.runs.add("run_known");
        const answer = await r.invoke(channel, ipcEvent(r.ids[type]), payload);
        const allowed = expected[type].has(channel);
        expect(r.calls.length === 1, `${type} → ${channel} should ${allowed ? "reach" : "not reach"} its handler`).toBe(allowed);
        if (!allowed) {
          expect(answer, `${type} → ${channel}`).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
        }
      }
    }
  });

  it("a quick-entry window cannot call settings, commands, approvals or anything beyond posting a new thread", async () => {
    const r = rig();
    for (const channel of ["settings.updateWorkspace", "settings.updateUser", "commands.invoke", "approvals.approve", "approvals.deny", "auth.signOut", "recording.start", "artifacts.save", "messages.post", "threads.messages", "threads.list", "shell.preferences.set", "shell.shortcuts.set", "shell.popout.open", "update.install", "diagnostics.export"] as const) {
      expect(await r.invoke(channel, ipcEvent(r.ids.quick_entry)), channel).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    }
    expect(r.calls).toEqual([]);
    expect(await r.invoke("shell.quickEntry.submit", ipcEvent(r.ids.quick_entry), { content: "hi" })).toMatchObject({ ok: true });
    expect(r.calls.map((call) => call.channel)).toEqual(["shell.quickEntry.submit"]);
  });

  it("the indicator cannot read a thread, post, or change a preference", async () => {
    const r = rig();
    for (const channel of ["threads.list", "threads.messages", "messages.post", "commands.invoke", "settings.updateUser", "shell.preferences.set", "shell.shortcuts.set", "shell.quickEntry.submit", "shell.popout.open", "session.me"] as const) {
      expect(await r.invoke(channel, ipcEvent(r.ids.indicator)), channel).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    }
    expect(r.calls).toEqual([]);
  });

  it("answers a refusal as data and logs it with the window type", async () => {
    const r = rig();
    const answer = await r.invoke("settings.updateWorkspace", ipcEvent(r.ids.popout));
    expect(answer).toEqual({ ok: false, error: { code: "SCOPE_FORBIDDEN", message: "a popout window may not call settings.updateWorkspace", retryable: false, details: null } });
    expect(r.log.some((line) => line.includes("refused settings.updateWorkspace from a popout window"))).toBe(true);
  });
});

describe("a pop-out", () => {
  const forbidden = { ok: false, error: { code: "SCOPE_FORBIDDEN" } };

  it("reads and posts only in its own thread", async () => {
    const r = rig();
    const popout = ipcEvent(r.ids.popout);
    expect(await r.invoke("threads.messages", popout, { threadId: THREAD })).toMatchObject({ ok: true });
    expect(await r.invoke("threads.messages", popout, { threadId: OTHER_THREAD })).toMatchObject(forbidden);
    expect(await r.invoke("threads.messages", popout, {})).toMatchObject(forbidden);
    expect(await r.invoke("threads.view", popout, { threadId: OTHER_THREAD })).toMatchObject(forbidden);
    expect(await r.invoke("messages.post", popout, { threadId: THREAD, content: "hi" })).toMatchObject({ ok: true });
    expect(await r.invoke("messages.post", popout, { threadId: OTHER_THREAD, content: "hi" })).toMatchObject(forbidden);
    // A pop-out cannot start a new thread (a null thread is a new one) or post without naming its own.
    expect(await r.invoke("messages.post", popout, { threadId: null, content: "hi" })).toMatchObject(forbidden);
    expect(await r.invoke("messages.post", popout, { content: "hi" })).toMatchObject(forbidden);
    expect(await r.invoke("artifacts.upload", popout, { threadId: OTHER_THREAD, name: "a" })).toMatchObject(forbidden);
    expect(await r.invoke("artifacts.upload", popout, { threadId: THREAD, name: "a" })).toMatchObject({ ok: true });
    expect(r.calls.map((call) => `${call.channel}:${(call.payload as { threadId?: string }).threadId ?? ""}`)).toEqual([
      `threads.messages:${THREAD}`,
      `messages.post:${THREAD}`,
      `artifacts.upload:${THREAD}`,
    ]);
  });

  it("lists questions by its own thread only, never by a run", async () => {
    const r = rig();
    const popout = ipcEvent(r.ids.popout);
    expect(await r.invoke("questions.list", popout, { threadId: THREAD })).toMatchObject({ ok: true });
    expect(await r.invoke("questions.list", popout, { threadId: OTHER_THREAD })).toMatchObject(forbidden);
    expect(await r.invoke("questions.list", popout, { runId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" })).toMatchObject(forbidden);
    expect(await r.invoke("questions.list", popout, { threadId: THREAD, runId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" })).toMatchObject(forbidden);
  });

  it("answers, cancels and inspects only what its own thread's reads returned", async () => {
    const r = rig();
    const popout = ipcEvent(r.ids.popout);
    // Nothing is known yet: another thread's question or run is refused.
    expect(await r.invoke("questions.answer", popout, { questionId: "q_other", value: 1 })).toMatchObject(forbidden);
    expect(await r.invoke("turns.cancel", popout, { runId: "run_other" })).toMatchObject(forbidden);
    expect(await r.invoke("runs.timeline", popout, { runId: "run_other" })).toMatchObject(forbidden);

    r.results.set("questions.list", { questions: [{ id: "q_mine", run_id: "run_questioned" }] });
    await r.invoke("questions.list", popout, { threadId: THREAD });
    r.results.set("threads.messages", { messages: [{ id: "msg_1", run_id: "run_replied" }] });
    await r.invoke("threads.messages", popout, { threadId: THREAD });
    r.results.set("messages.post", { thread_id: THREAD, run_id: "run_posted", message_id: "msg_2" });
    await r.invoke("messages.post", popout, { threadId: THREAD, content: "hi" });

    expect(await r.invoke("questions.answer", popout, { questionId: "q_mine", value: 1 })).toMatchObject({ ok: true });
    expect(await r.invoke("questions.answer", popout, { questionId: "q_other", value: 1 })).toMatchObject(forbidden);
    for (const run of ["run_questioned", "run_replied", "run_posted"]) {
      expect(await r.invoke("turns.cancel", popout, { runId: run })).toMatchObject({ ok: true });
      expect(await r.invoke("runs.timeline", popout, { runId: run })).toMatchObject({ ok: true });
    }
    expect(await r.invoke("turns.cancel", popout, { runId: "run_other" })).toMatchObject(forbidden);
  });

  it("what one pop-out learned is not known to another window", async () => {
    const r = rig();
    const second = r.windows.openPopout({ threadId: OTHER_THREAD, tab: "conversation", sidebar: false });
    if (!second.ok) {
      throw new Error("no second pop-out");
    }
    r.results.set("threads.messages", { messages: [{ id: "msg_1", run_id: "run_first" }] });
    await r.invoke("threads.messages", ipcEvent(r.ids.popout), { threadId: THREAD });
    expect(await r.invoke("turns.cancel", ipcEvent(r.ids.popout), { runId: "run_first" })).toMatchObject({ ok: true });
    expect(await r.invoke("turns.cancel", ipcEvent(second.window.handle.webContentsId), { runId: "run_first" })).toMatchObject(forbidden);
  });

  it("the main window is not scoped to a thread", async () => {
    const r = rig();
    expect(await r.invoke("threads.messages", ipcEvent(r.ids.main), { threadId: OTHER_THREAD })).toMatchObject({ ok: true });
    expect(await r.invoke("turns.cancel", ipcEvent(r.ids.main), { runId: "run_any" })).toMatchObject({ ok: true });
  });
});
