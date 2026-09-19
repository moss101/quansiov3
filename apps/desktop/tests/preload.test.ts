/**
 * Permission boundary test (APP-003): what the page can reach through `window.quansio` and
 * nothing more. The real preload script is loaded with Electron's `contextBridge` and
 * `ipcRenderer` replaced by recording fakes, so the exposed object is the shipped one.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

import { FORBIDDEN_RENDERER_APIS, PRELOAD_ALLOWLIST, PRELOAD_PUSH_CHANNELS, createPreloadApi, isForbiddenRendererApi, preloadAllows, pushAllows } from "../src/index.js";

const exposed: Record<string, unknown> = {};
const invoked: { channel: string; payload: unknown }[] = [];
const listeners = new Map<string, Set<(event: unknown, payload: unknown) => void>>();

vi.mock("electron", () => ({
  contextBridge: {
    exposeInMainWorld: (name: string, api: unknown) => {
      exposed[name] = api;
    },
  },
  ipcRenderer: {
    invoke: (channel: string, payload: unknown) => {
      invoked.push({ channel, payload });
      return Promise.resolve({ ok: true, channel });
    },
    on: (channel: string, listener: (event: unknown, payload: unknown) => void) => {
      if (!listeners.has(channel)) {
        listeners.set(channel, new Set());
      }
      listeners.get(channel)?.add(listener);
    },
    removeListener: (channel: string, listener: (event: unknown, payload: unknown) => void) => {
      listeners.get(channel)?.delete(listener);
    },
  },
}));

describe("preload bridge", () => {
  beforeEach(() => {
    invoked.length = 0;
    listeners.clear();
  });

  it("exposes exactly the allowlisted invoke and push surface on window.quansio", async () => {
    await import("../src/preload/index.js");
    const api = exposed["quansio"] as Record<string, (...args: unknown[]) => unknown>;
    expect(api).toBeDefined();
    expect(Object.keys(api).sort()).toEqual(
      [
        "activeRun",
        "sessionState",
        "me",
        "startMagicLink",
        "completeMagicLink",
        "startOAuth",
        "signOut",
        "updateWorkspace",
        "updateUserSettings",
        "invokeCommand",
        "openDeepLink",
        "reconnectStream",
        "subscribeStream",
        "updateStatus",
        "listThreads",
        "threadMessages",
        "listQuestions",
        "runTimeline",
        "postMessage",
        "answerQuestion",
        "cancelTurn",
        "uploadAttachment",
        "onStreamFrame",
        "onStreamState",
        "onStreamError",
        "onSessionState",
        "onDeepLink",
        "onUpdateStatus",
      ].sort(),
    );
    for (const key of Object.keys(api)) {
      expect(typeof api[key]).toBe("function");
    }
    // No raw ipcRenderer, no require, no Node globals are exposed.
    expect("ipcRenderer" in api).toBe(false);
    expect("require" in api).toBe(false);
    for (const forbidden of FORBIDDEN_RENDERER_APIS) {
      expect(forbidden in api).toBe(false);
      expect(isForbiddenRendererApi(forbidden)).toBe(true);
    }
  });

  it("routes every invoke to an allowlisted channel with the caller's payload", async () => {
    await import("../src/preload/index.js");
    const api = exposed["quansio"] as ReturnType<typeof createPreloadApi>;
    const command = { name: "PostMessage", commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params: { content: "hi" } };
    await api.invokeCommand(command);
    await api.subscribeStream();
    await api.reconnectStream("cursor-1");
    await api.activeRun();
    await api.sessionState();
    await api.me();
    await api.startMagicLink("ada@example.com", "Ada");
    await api.completeMagicLink("qml_token");
    await api.startOAuth("github");
    await api.signOut();
    await api.updateWorkspace("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", { settings: { execution_target: "cloud" } });
    await api.updateUserSettings("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", { theme: "dark" });
    await api.openDeepLink("quansio://thread/thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.updateStatus();
    await api.listThreads();
    await api.threadMessages("thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.listQuestions({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.runTimeline("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.postMessage({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", content: "hi", attachments: ["art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"] });
    await api.answerQuestion("q_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "alpha");
    await api.cancelTurn("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.uploadAttachment({ name: "a.bin", mediaType: "application/octet-stream", bytesBase64: "AAEC" });
    expect(invoked.map((call) => call.channel)).toEqual([...PRELOAD_ALLOWLIST]);
    expect(invoked[0]?.payload).toEqual(command);
    for (const call of invoked) {
      expect(preloadAllows(call.channel)).toBe(true);
    }
  });

  it("subscribes only on push channels and unsubscribes cleanly", async () => {
    await import("../src/preload/index.js");
    const api = exposed["quansio"] as ReturnType<typeof createPreloadApi>;
    const frames: unknown[] = [];
    const off = api.onStreamFrame((frame) => frames.push(frame));
    expect([...listeners.keys()]).toEqual(["stream.frame"]);
    for (const listener of listeners.get("stream.frame") ?? []) {
      listener({}, { kind: "live", channel: "run:run_x", frame: { kind: "model.delta", text: "…" } });
    }
    expect(frames).toHaveLength(1);
    off();
    expect(listeners.get("stream.frame")?.size ?? 0).toBe(0);
    for (const channel of PRELOAD_PUSH_CHANNELS) {
      expect(pushAllows(channel)).toBe(true);
    }
    expect(pushAllows("fs.read")).toBe(false);
  });

  it("refuses a channel outside the allowlist even if a caller reaches the send wrapper", () => {
    const calls: string[] = [];
    const api = createPreloadApi(
      (channel, payload) => {
        if (!preloadAllows(channel)) {
          throw new Error("never");
        }
        calls.push(channel);
        return Promise.resolve(payload);
      },
      () => () => undefined,
    );
    void api.activeRun();
    expect(calls).toEqual(["session.activeRun"]);
    expect(preloadAllows("fs.read")).toBe(false);
    expect(preloadAllows("child_process.exec")).toBe(false);
    expect(preloadAllows("commands.invoke")).toBe(true);
  });
});
