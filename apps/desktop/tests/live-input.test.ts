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

import { browserInput, decodeBrowserFrame, inputForKey, USER_KEYS } from "../src/index.js";
import { parseBinaryPicture, StreamClient, subscribeRequest, type LiveFrame, type SocketLike } from "../src/client/stream.js";
import { userKeyAllowed } from "../src/live/session.js";
import { frameToPage, wheelToPage } from "../src/live/frames.js";
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

describe("a key press", () => {
  const press = (key: string, held: Partial<{ ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean }> = {}) =>
    inputForKey({ key, ctrlKey: false, metaKey: false, altKey: false, ...held });

  it("is a named key, a printable character as text, or nothing", () => {
    for (const key of USER_KEYS) {
      expect(press(key), key).toEqual({ kind: "key", key });
    }
    expect(press("a")).toEqual({ kind: "text", text: "a" });
    expect(press("É")).toEqual({ kind: "text", text: "É" });
    expect(press(" ")).toEqual({ kind: "text", text: " " });
    expect(press("😀")).toEqual({ kind: "text", text: "😀" });
    expect(press("e\u0301")).toEqual({ kind: "text", text: "e\u0301" });
  });

  it("is a shortcut when a letter or digit is held with Alt, Control or Meta, named in lower case whatever Shift did", () => {
    expect(press("c", { metaKey: true })).toEqual({ kind: "key", key: "Meta+c" });
    expect(press("a", { ctrlKey: true })).toEqual({ kind: "key", key: "Control+a" });
    expect(press("Z", { metaKey: true, shiftKey: true })).toEqual({ kind: "key", key: "Meta+Shift+z" });
    expect(press("7", { altKey: true })).toEqual({ kind: "key", key: "Alt+7" });
    expect(press("Enter", { altKey: true })).toEqual({ kind: "key", key: "Alt+Enter" });
    expect(press("Tab", { shiftKey: true })).toEqual({ kind: "key", key: "Shift+Tab" });
    // Shift alone only changes the character; it is text.
    expect(press("A", { shiftKey: true })).toEqual({ kind: "text", text: "A" });
  });

  it("stays with the person's own keyboard when it is not a key the page can be told", () => {
    // A shortcut on something that is not a letter or a digit, and a printable character that is not a shortcut.
    expect(press("€", { metaKey: true })).toBeNull();
    expect(press("F12", { ctrlKey: true })).toBeNull();
    for (const key of ["Shift", "Control", "Meta", "Alt", "CapsLock", "F5", "F12", "Dead", "Process", "Unidentified", ""]) {
      expect(press(key), key).toBeNull();
    }
  });

  it("is judged by the same rule as the server's", () => {
    for (const allowed of ["Enter", "Shift+Tab", "Control+a", "Meta+Shift+z", "Alt+ArrowLeft", "Alt+Control+Delete", "Meta+7"]) {
      expect(userKeyAllowed(allowed), allowed).toBe(true);
    }
    for (const refused of ["a", "Shift+a", "F12", "Control+F12", "Control+", "Control+Control+a", "Shift+Control+a", "Hyper+a", "Control+é", "Control+ab", ""]) {
      expect(userKeyAllowed(refused), refused).toBe(false);
    }
  });
});

describe("a click, a drag and a held modifier", () => {
  it("are built from exactly what the server accepts, sending only what differs from a plain click", () => {
    expect(browserInput(SESSION, "T1", { kind: "click", x: 1, y: 2, button: "right" })).toEqual({
      name: "SendBrowserInput",
      params: { session_id: SESSION, tab_id: "T1", kind: "click", x: 1, y: 2, button: "right" },
    });
    expect(browserInput(SESSION, "T1", { kind: "click", x: 1, y: 2, clickCount: 2, modifiers: ["Shift"] })).toEqual({
      name: "SendBrowserInput",
      params: { session_id: SESSION, tab_id: "T1", kind: "click", x: 1, y: 2, click_count: 2, modifiers: ["Shift"] },
    });
    expect(browserInput(SESSION, "T1", { kind: "drag", fromX: 1, fromY: 2, toX: 30, toY: 40 })).toEqual({
      name: "SendBrowserInput",
      params: { session_id: SESSION, tab_id: "T1", kind: "drag", from_x: 1, from_y: 2, to_x: 30, to_y: 40 },
    });
    for (const bad of [
      { kind: "click", x: 1, y: 2, button: "back" },
      { kind: "click", x: 1, y: 2, clickCount: 4 },
      { kind: "click", x: 1, y: 2, modifiers: ["Hyper"] },
      { kind: "click", x: 1, y: 2, modifiers: ["Shift", "Shift"] },
      { kind: "drag", fromX: 1, fromY: 2, toX: 30000, toY: 40 },
      { kind: "drag", fromX: 1, fromY: 2, toX: Number.NaN, toY: 40 },
    ]) {
      expect(() => browserInput(SESSION, "T1", bad as never), JSON.stringify(bad)).toThrow();
    }
  });

  it("travel the same channel, and are sent from the live view only while the person holds the session", () => {
    const source = readFileSync(join(__dirname, "..", "src", "renderer", "Live.tsx"), "utf8");
    for (const handler of ["onMouseDown={session.controller", "onMouseUp={session.controller", "onAuxClick={session.controller", "onContextMenu={session.controller"]) {
      expect(source).toContain(`${handler} === "user" ?`);
    }
    // The local context menu is the remote page's to show: the right click is sent, not shown here.
    expect(source).toContain("event.preventDefault?.()");
    // The browser's own image drag would swallow the mouse-up that ends a drag on the picture (found in a real window).
    expect(source).toContain("draggable={false}");
  });
});

describe("a turn of the wheel over the picture", () => {
  const frame = frameOf();
  if (!frame) {
    throw new Error("a frame");
  }
  const rect = { left: 100, top: 40, width: 500, height: 250 };

  it("scrolls the page by the same fraction, scaled from the displayed picture to the page", () => {
    expect(wheelToPage(frame, rect, 350, 165, 10, 50)).toEqual({ x: 500, y: 250, deltaX: 20, deltaY: 100 });
    expect(wheelToPage(frame, rect, 350, 165, -5, -25)).toEqual({ x: 500, y: 250, deltaX: -10, deltaY: -50 });
  });

  it("is not sent when it cannot be placed, and never exceeds what the server accepts", () => {
    expect(wheelToPage(frame, rect, 50, 165, 0, 10)).toBeNull();
    expect(wheelToPage(frame, rect, 350, 165, Number.NaN, 10)).toBeNull();
    const unsized = frameOf({ width: undefined, height: undefined });
    expect(unsized && wheelToPage(unsized, rect, 350, 165, 0, 10)).toBeNull();
    expect(wheelToPage(frame, rect, 350, 165, 0, 1e9)?.deltaY).toBe(10000);
    expect(wheelToPage(frame, rect, 350, 165, 0, -1e9)?.deltaY).toBe(-10000);
  });
});

describe("the key and scroll commands", () => {
  it("are built from exactly what the server accepts", () => {
    expect(browserInput(SESSION, "T1", { kind: "key", key: "Enter" })).toEqual({
      name: "SendBrowserInput",
      params: { session_id: SESSION, tab_id: "T1", kind: "key", key: "Enter" },
    });
    expect(browserInput(SESSION, "T1", { kind: "scroll", x: 5, y: 6, deltaX: 0, deltaY: -120 })).toEqual({
      name: "SendBrowserInput",
      params: { session_id: SESSION, tab_id: "T1", kind: "scroll", x: 5, y: 6, delta_x: 0, delta_y: -120 },
    });
    expect(() => browserInput(SESSION, "T1", { kind: "key", key: "a" })).toThrow();
    expect(() => browserInput(SESSION, "T1", { kind: "key", key: "Control+F12" })).toThrow();
    expect(browserInput(SESSION, "T1", { kind: "key", key: "Control+a" })).toMatchObject({ params: { key: "Control+a" } });
    expect(() => browserInput(SESSION, "T1", { kind: "scroll", x: -1, y: 0, deltaX: 0, deltaY: 1 })).toThrow();
    expect(() => browserInput(SESSION, "T1", { kind: "scroll", x: 0, y: 0, deltaX: 0, deltaY: 10001 })).toThrow();
    expect(() => browserInput(SESSION, "T1", { kind: "scroll", x: 0, y: 0, deltaX: Number.NaN, deltaY: 1 })).toThrow();
  });

  it("travel the same channel with the params the command defines, and a refused one sends nothing", async () => {
    const calls: { body: Record<string, unknown> | null }[] = [];
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
      fetch: (_url, init) => {
        calls.push({ body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null });
        return Promise.resolve(new Response(JSON.stringify({ command_id: "c", accepted_at: "now", replayed: false, result: { delivered: true } }), { status: 200, headers: { "content-type": "application/json" } }));
      },
    }).handlers();
    expect(await handlers["live.input"]({ sessionId: SESSION, tabId: "T1", kind: "key", key: "Enter" })).toMatchObject({ ok: true });
    expect(calls[0]?.body?.["params"]).toEqual({ session_id: SESSION, tab_id: "T1", kind: "key", key: "Enter" });
    expect(await handlers["live.input"]({ sessionId: SESSION, tabId: "T1", kind: "scroll", x: 1, y: 2, deltaX: 3, deltaY: 4 })).toMatchObject({ ok: true });
    expect(calls[1]?.body?.["params"]).toEqual({ session_id: SESSION, tab_id: "T1", kind: "scroll", x: 1, y: 2, delta_x: 3, delta_y: 4 });
    const before = calls.length;
    for (const bad of [
      { sessionId: SESSION, tabId: "T1", kind: "key", key: "F12" },
      { sessionId: SESSION, tabId: "T1", kind: "scroll", x: 1, y: 2, deltaX: 0, deltaY: 99999 },
    ]) {
      expect(await handlers["live.input"](bad), JSON.stringify(bad)).toMatchObject({ ok: false });
    }
    expect(calls.length).toBe(before);
  });

  it("are sent from the live view only while the person holds the session", () => {
    const source = readFileSync(join(__dirname, "..", "src", "renderer", "Live.tsx"), "utf8");
    expect(source).toContain('onWheel={session.controller === "user" ? wheelOverPicture : undefined}');
    expect(source).toContain('onKeyDown={session.controller === "user" ? typeInPicture : undefined}');
    expect(source).toContain("inputForKey(event)");
  });
});

describe("a binary picture", () => {
  const message = (header: unknown, picture: Uint8Array): Uint8Array => {
    const head = new TextEncoder().encode(JSON.stringify(header));
    const out = new Uint8Array(8 + head.length + picture.length);
    out.set([0x51, 0x46, 0x52, 0x31], 0);
    new DataView(out.buffer).setUint32(4, head.length, false);
    out.set(head, 8);
    out.set(picture, 8 + head.length);
    return out;
  };
  const header = { kind: "live", channel: "target:tgt_1", frame: { kind: "browser.frame", session_id: SESSION, tab_id: "T1", media_type: "image/jpeg", width: 1000, height: 500, sequence: 3 } };

  it("becomes the live frame its text form would have been, the bytes as base64", () => {
    const picture = Uint8Array.from([0xff, 0xd8, 0xff, 0xe0, 1, 2, 3]);
    const live = parseBinaryPicture(message(header, picture));
    expect(live).toMatchObject({ kind: "live", channel: "target:tgt_1", frame: { kind: "browser.frame", sequence: 3, width: 1000 } });
    expect(live.frame["data"]).toBe(Buffer.from(picture).toString("base64"));
    // And the live view reads it as it reads any frame.
    expect(decodeBrowserFrame(live.frame, SESSION)).toMatchObject({ tabId: "T1", width: 1000 });
  });

  it("is refused when it is not one", () => {
    const ok = message(header, Uint8Array.from([1]));
    for (const bad of [new Uint8Array(0), Uint8Array.from([1, 2, 3, 4, 5, 6, 7, 8]), ok.subarray(0, 12), message({ kind: "event" }, Uint8Array.from([1])), message({ kind: "live", channel: "c" }, Uint8Array.from([1]))]) {
      expect(() => parseBinaryPicture(bad)).toThrow();
    }
    const lying = Uint8Array.from(ok);
    new DataView(lying.buffer).setUint32(4, 99999, false);
    expect(() => parseBinaryPicture(lying)).toThrow(/runs past/);
  });

  it("is asked for only by the live view of a target", () => {
    expect(subscribeRequest(["target:tgt_1"], null, "c", { binaryFrames: true })).toEqual({ subscribe: [{ channel: "target:tgt_1" }], consumer: "c", binary_frames: true });
    expect(subscribeRequest(["workspace:ws_1"], null, "c")).not.toHaveProperty("binary_frames");
    const source = readFileSync(join(__dirname, "..", "src", "main", "ipc.ts"), "utf8");
    expect(source.match(/binaryFrames: true/g)?.length).toBe(1);
    expect(source).toMatch(/channels: \[`target:\$\{targetId\}`\],[\s\S]{0,200}binaryFrames: true/);
  });

  it("is delivered by the stream client as the live frame, whatever buffer type the socket hands over, and a damaged one is dropped", () => {
    const listeners: Record<string, ((event: { data: unknown }) => void)[]> = {};
    const sent: string[] = [];
    const socket: SocketLike = {
      send: (data) => sent.push(data),
      close: () => undefined,
      readyState: 1,
      addEventListener: ((type: string, listener: (event: { data: unknown }) => void) => {
        (listeners[type] ??= []).push(listener);
      }) as SocketLike["addEventListener"],
    };
    const seen: LiveFrame[] = [];
    const client = new StreamClient({
      url: "ws://x/v1/stream",
      headers: {},
      channels: ["target:tgt_1"],
      consumer: "c",
      openSocket: () => socket,
      binaryFrames: true,
      onLive: (frame) => seen.push(frame),
    });
    client.connect();
    listeners["open"]?.forEach((listener) => { listener({ data: null }); });
    expect(JSON.parse(sent[0] ?? "{}")).toMatchObject({ binary_frames: true });
    const picture = Uint8Array.from([0xff, 0xd8, 9]);
    const bytes = message(header, picture);
    const deliver = (data: unknown): void => listeners["message"]?.forEach((listener) => { listener({ data }); });
    deliver(Buffer.from(bytes));
    deliver(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
    deliver(Buffer.from("QFR1garbage"));
    deliver(Buffer.alloc(0));
    expect(seen.map((frame) => frame.frame["data"])).toEqual([Buffer.from(picture).toString("base64"), Buffer.from(picture).toString("base64")]);
  });
});

