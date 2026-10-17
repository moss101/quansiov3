/**
 * The person's side of driving a worker's browser (APP-007, DOMAIN.md 8.4.2): where a click on the picture lands in the
 * page, the input command's builder refusing what the server refuses, the one channel it travels on, and the live view
 * calling only that channel and only for a session the person holds. The server's half (delivery, the holder check, the
 * refusals) and the worker's (the page really sees the click) are proved against PostgreSQL and a real Chrome in
 * `crates/server/tests/worker_live.rs`.
 */

import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { browserInput, decodeBrowserFrame } from "../src/index.js";
import { frameToPage } from "../src/live/frames.js";
import { DesktopBridge } from "../src/main/ipc.js";
import { SessionFile } from "../src/main/session-file.js";
import { UpdateHooks } from "../src/main/updates.js";
import { PRELOAD_ALLOWLIST } from "../src/preload/bridge.js";

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const SESSION = "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

const frameOf = (extra: Record<string, unknown> = {}) =>
  decodeBrowserFrame({ kind: "browser.frame", session_id: SESSION, data: "AAAA", tab_id: "T1", width: 1000, height: 500, ...extra }, SESSION);

describe("a frame", () => {
  it("carries the tab it shows and the page's size, and ignores a size that makes no sense", () => {
    expect(frameOf()).toMatchObject({ tabId: "T1", width: 1000, height: 500 });
    for (const bad of [0, -5, Number.NaN, 1e9, "wide", null]) {
      expect(frameOf({ width: bad, height: bad }), String(bad)).toMatchObject({ width: null, height: null });
    }
    expect(frameOf({ tab_id: "" })?.tabId).toBeNull();
    expect(frameOf({ tab_id: 7 })?.tabId).toBeNull();
  });
});

describe("a click on the picture", () => {
  const frame = frameOf();
  if (!frame) {
    throw new Error("a frame");
  }

  it("lands where the same fraction of the page is, whatever size the picture is displayed at", () => {
    // Displayed at half size, offset in the window.
    const rect = { left: 100, top: 40, width: 500, height: 250 };
    expect(frameToPage(frame, rect, 350, 165)).toEqual({ x: 500, y: 250 });
    expect(frameToPage(frame, rect, 100, 40)).toEqual({ x: 0, y: 0 });
    expect(frameToPage(frame, rect, 600, 290)).toEqual({ x: 1000, y: 500 });
    // At the page's own size.
    expect(frameToPage(frame, { left: 0, top: 0, width: 1000, height: 500 }, 123.4, 56.7)).toEqual({ x: 123.4, y: 56.7 });
  });

  it("is not sent when it cannot be placed", () => {
    const rect = { left: 100, top: 40, width: 500, height: 250 };
    expect(frameToPage(frame, rect, 99, 100)).toBeNull();
    expect(frameToPage(frame, rect, 601, 100)).toBeNull();
    expect(frameToPage(frame, rect, 300, 291)).toBeNull();
    expect(frameToPage(frame, { ...rect, width: 0 }, 300, 100)).toBeNull();
    const unsized = frameOf({ width: undefined, height: undefined });
    expect(unsized && frameToPage(unsized, rect, 300, 100)).toBeNull();
    expect(frameToPage(frame, rect, Number.NaN, 100)).toBeNull();
  });
});

describe("the input command builder", () => {
  it("builds the command the catalog defines", () => {
    expect(browserInput(SESSION, "T1", { kind: "click", x: 5, y: 6 })).toEqual({
      name: "SendBrowserInput",
      params: { session_id: SESSION, tab_id: "T1", kind: "click", x: 5, y: 6 },
    });
    expect(browserInput(SESSION, "T1", { kind: "text", text: "héllo" })).toEqual({
      name: "SendBrowserInput",
      params: { session_id: SESSION, tab_id: "T1", kind: "text", text: "héllo" },
    });
  });

  it("refuses what the server refuses, before anything is sent", () => {
    expect(() => browserInput("tgt_x", "T1", { kind: "click", x: 1, y: 1 })).toThrow(/bsn_/);
    expect(() => browserInput(SESSION, "", { kind: "click", x: 1, y: 1 })).toThrow();
    for (const [x, y] of [[-1, 0], [0, -1], [20001, 0], [0, 20001], [Number.NaN, 0], [0, Number.POSITIVE_INFINITY]] as const) {
      expect(() => browserInput(SESSION, "T1", { kind: "click", x, y }), `${String(x)},${String(y)}`).toThrow();
    }
    expect(() => browserInput(SESSION, "T1", { kind: "text", text: "" })).toThrow();
    expect(() => browserInput(SESSION, "T1", { kind: "text", text: "x".repeat(4097) })).toThrow();
    // The limit is bytes, not characters.
    expect(() => browserInput(SESSION, "T1", { kind: "text", text: "é".repeat(2049) })).toThrow();
  });
});

describe("the channel", () => {
  it("is allowlisted and sends exactly the params the command defines", async () => {
    expect(PRELOAD_ALLOWLIST).toContain("live.input");
    const calls: { url: string; body: Record<string, unknown> | null }[] = [];
    const handlers = new DesktopBridge({
      serverOrigin: "http://127.0.0.1:8080",
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
      session: new SessionFile(mkdtempSync(join(tmpdir(), "quansio-input-"))),
      openSocket: () => {
        throw new Error("no socket in this test");
      },
      updates: new UpdateHooks("stable", null, "http://x", false),
      consumer: "desktop:test",
      push: () => undefined,
      fetch: (url, init) => {
        calls.push({ url, body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null });
        return Promise.resolve(new Response(JSON.stringify({ command_id: "c", accepted_at: "now", replayed: false, result: { delivered: true } }), { status: 200, headers: { "content-type": "application/json" } }));
      },
    }).handlers();

    expect(await handlers["live.input"]({ sessionId: SESSION, tabId: "T1", kind: "click", x: 5, y: 6 })).toMatchObject({ ok: true, result: { delivered: true } });
    expect(calls[0]?.url).toMatch(/\/v1\/commands\/SendBrowserInput$/);
    expect(calls[0]?.body?.["params"]).toEqual({ session_id: SESSION, tab_id: "T1", kind: "click", x: 5, y: 6 });
    await handlers["live.input"]({ sessionId: SESSION, tabId: "T1", kind: "text", text: "a" });
    expect(calls[1]?.body?.["params"]).toEqual({ session_id: SESSION, tab_id: "T1", kind: "text", text: "a" });
    expect(calls[1]?.body?.["command_id"]).not.toBe(calls[0]?.body?.["command_id"]);

    // A payload the builder refuses is data, and nothing is sent.
    const before = calls.length;
    for (const bad of [
      { sessionId: SESSION, tabId: "T1", kind: "drag" },
      { sessionId: SESSION, tabId: "T1", kind: "click", x: -1, y: 2 },
      { sessionId: "tgt_x", tabId: "T1", kind: "text", text: "a" },
      { sessionId: SESSION, tabId: "", kind: "text", text: "a" },
    ]) {
      expect(await handlers["live.input"](bad), JSON.stringify(bad)).toMatchObject({ ok: false });
    }
    expect(calls.length).toBe(before);
  });
});

describe("the live view", () => {
  const source = readFileSync(join(__dirname, "..", "src", "renderer", "Live.tsx"), "utf8");

  it("sends input only through the person's channel, only while they hold the session, and shows a refusal", () => {
    expect(source).toContain("window.quansio.sendBrowserInput");
    expect(source).toContain('session.controller !== "user"');
    expect(source).toContain('onClick={session.controller === "user" ? clickPicture : undefined}');
    expect(source).toContain("setNotice(refused)");
  });
});
