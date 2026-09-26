/**
 * Unit tests for the shell's main-process modules (APP-003): durable session, stream
 * client bookkeeping, deep links, update hooks and the IPC bridge. The socket and fetch are
 * in-process doubles here; the reconnect E2E against the real server is
 * `reconnect.e2e.test.ts`.
 */

import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  DesktopBridge,
  SessionFile,
  StreamClient,
  UpdateHooks,
  deepLinkFromArgv,
  parseDeepLink,
  parseSession,
  parseStreamFrame,
  reconnectDelayMs,
  registerBridgeHandlers,
  serializeSession,
  subscribeRequest,
  updateFeedUrl,
  type AutoUpdaterLike,
  type SocketLike,
} from "../src/index.js";
import { PRELOAD_ALLOWLIST } from "../src/preload/bridge.js";

const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const THREAD = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

/** Narrow a possibly-missing value in a test without a non-null assertion. */
function must<T>(value: T | null | undefined, what: string): T {
  if (value === null || value === undefined) {
    throw new Error(`expected ${what}`);
  }
  return value;
}

const temporaryDirs: string[] = [];
function userData(): string {
  const dir = mkdtempSync(join(tmpdir(), "quansio-desktop-"));
  temporaryDirs.push(dir);
  return dir;
}
afterEach(() => {
  for (const dir of temporaryDirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** The cursor a socket's subscribe request carried, if any. */
function subscribeCursor(socket: FakeSocket): string | undefined {
  const request = JSON.parse(must(socket.sent[0], "a subscribe request")) as { cursor?: string };
  return request.cursor;
}

/** A socket double the StreamClient drives; tests play the server. */
class FakeSocket implements SocketLike {
  readonly sent: string[] = [];
  readyState = 0;
  readonly #listeners = new Map<string, ((event: never) => void)[]>();
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    this.readyState = 3;
    this.emit("close", { code: 1000, reason: "" });
  }
  addEventListener(type: string, listener: (event: never) => void): void {
    this.#listeners.set(type, [...(this.#listeners.get(type) ?? []), listener]);
  }
  emit(type: string, event: unknown): void {
    for (const listener of this.#listeners.get(type) ?? []) {
      (listener as (event: unknown) => void)(event);
    }
  }
  open(): void {
    this.readyState = 1;
    this.emit("open", undefined);
  }
  message(frame: unknown): void {
    this.emit("message", { data: JSON.stringify(frame) });
  }
}

describe("durable session file", () => {
  it("round-trips through disk atomically and survives a fresh SessionFile", () => {
    const dir = userData();
    const first = new SessionFile(dir);
    expect(first.current()).toBeNull();
    first.begin("http://127.0.0.1:8080", WORKSPACE);
    first.attach({ runId: RUN, threadId: THREAD, generation: 4, streamCursor: "c-4", workspaceId: WORKSPACE });
    first.advance("c-5");
    const onDisk = JSON.parse(readFileSync(join(dir, "session.json"), "utf8")) as Record<string, unknown>;
    expect(onDisk["schema_version"]).toBe(1);
    expect(onDisk["stream_cursor"]).toBe("c-5");

    const second = new SessionFile(dir);
    const restored = second.current();
    expect(restored?.activeRun?.runId).toBe(RUN);
    expect(restored?.activeRun?.generation).toBe(4);
    expect(restored?.activeRun?.streamCursor).toBe("c-5");
    expect(restored?.streamCursor).toBe("c-5");
    second.detach();
    expect(new SessionFile(dir).current()?.activeRun).toBeNull();
    expect(new SessionFile(dir).current()?.streamCursor).toBe("c-5");
    second.clear();
    expect(new SessionFile(dir).current()).toBeNull();
  });

  it("treats malformed or foreign content as no session", () => {
    expect(parseSession(null)).toBeNull();
    expect(parseSession({ schema_version: 2 })).toBeNull();
    expect(parseSession({ schema_version: 1, server_origin: 1, workspace_id: WORKSPACE })).toBeNull();
    const session = parseSession({ schema_version: 1, server_origin: "http://x", workspace_id: WORKSPACE, active_run: { run_id: 1 } });
    expect(session?.activeRun).toBeNull();
    const decoded = JSON.parse(serializeSession(must(session, "a parsed session"))) as Record<string, unknown>;
    expect(decoded["active_run"]).toBeNull();
  });
});

describe("stream client", () => {
  it("parses §9.3 frames and §15 errors, refusing anything else", () => {
    expect(parseStreamFrame(JSON.stringify({ kind: "event", cursor: "c1", event: { type: "thread.created" } }))).toEqual({
      kind: "event",
      cursor: "c1",
      event: { type: "thread.created" },
    });
    expect(parseStreamFrame(JSON.stringify({ kind: "live", channel: `run:${RUN}`, frame: { kind: "model.delta", text: "h" } }))).toMatchObject({ kind: "live" });
    expect(parseStreamFrame(JSON.stringify({ code: "STREAM_BACKPRESSURE", message: "slow", correlation_id: "s", retryable: true }))).toMatchObject({ code: "STREAM_BACKPRESSURE", retryable: true });
    expect(() => parseStreamFrame(JSON.stringify({ kind: "event" }))).toThrow(/unrecognized/);
    expect(() => parseStreamFrame("[]")).toThrow();
  });

  it("builds the subscribe request the server expects and bounds the backoff", () => {
    expect(subscribeRequest([`workspace:${WORKSPACE}`], null, "desktop:1")).toEqual({ subscribe: [{ channel: `workspace:${WORKSPACE}` }], consumer: "desktop:1" });
    expect(subscribeRequest([`run:${RUN}`], "c9", "desktop:1").cursor).toBe("c9");
    expect(() => subscribeRequest([], null, "x")).toThrow();
    expect(() => subscribeRequest(["admin:*"], null, "x")).toThrow(/invalid stream channel/);
    expect([1, 2, 3, 4, 5, 6, 7].map(reconnectDelayMs)).toEqual([250, 500, 1000, 2000, 4000, 8000, 8000]);
    expect(() => reconnectDelayMs(0)).toThrow();
  });

  it("advances the cursor on durable events only and resumes from it after a drop", () => {
    const sockets: FakeSocket[] = [];
    const timers: (() => void)[] = [];
    const events: string[] = [];
    const live: string[] = [];
    const states: string[] = [];
    const client = new StreamClient({
      url: "ws://127.0.0.1:1/v1/stream",
      headers: { "x-quansio-tenant": "tn_x" },
      channels: [`workspace:${WORKSPACE}`],
      consumer: "desktop:test",
      openSocket: () => {
        const socket = new FakeSocket();
        sockets.push(socket);
        return socket;
      },
      cursor: "c-0",
      onEvent: (frame) => events.push(frame.cursor),
      onLive: (frame) => live.push(frame.frame.kind),
      onState: (state) => states.push(state),
      setTimer: (callback) => {
        timers.push(callback);
        return 0;
      },
    });
    client.connect();
    const first = must(sockets[0], "the first socket");
    first.open();
    expect(JSON.parse(must(first.sent[0], "a subscribe request"))).toEqual({ subscribe: [{ channel: `workspace:${WORKSPACE}` }], consumer: "desktop:test", cursor: "c-0" });
    first.message({ kind: "event", cursor: "c-1", event: { type: "a" } });
    first.message({ kind: "live", channel: `run:${RUN}`, frame: { kind: "terminal.bytes", bytes: "AA==" } });
    first.message({ kind: "event", cursor: "c-2", event: { type: "b" } });
    first.message("not json at all");
    expect(events).toEqual(["c-1", "c-2"]);
    expect(live).toEqual(["terminal.bytes"]);
    expect(client.cursor).toBe("c-2");

    // The transport drops: the client schedules a reconnect and resumes from c-2, not c-0.
    client.dropForTest();
    expect(states.at(-1)).toBe("reconnecting");
    expect(timers).toHaveLength(1);
    must(timers[0], "the reconnect timer")();
    const second = must(sockets[1], "the second socket");
    second.open();
    expect(subscribeCursor(second)).toBe("c-2");
    expect(states.at(-1)).toBe("open");

    client.close();
    expect(client.state).toBe("closed");
    expect(timers).toHaveLength(1);
  });

  it("gives up after the bounded number of attempts", () => {
    const timers: (() => void)[] = [];
    const sockets: FakeSocket[] = [];
    const client = new StreamClient({
      url: "ws://127.0.0.1:1/v1/stream",
      headers: {},
      channels: [`workspace:${WORKSPACE}`],
      consumer: "desktop:test",
      openSocket: () => {
        const socket = new FakeSocket();
        sockets.push(socket);
        return socket;
      },
      maxAttempts: 3,
      setTimer: (callback) => {
        timers.push(callback);
        return 0;
      },
    });
    client.connect();
    for (let attempt = 0; attempt < 5; attempt += 1) {
      must(sockets.at(-1), "a socket").close();
      const timer = timers.pop();
      if (timer) {
        timer();
      }
    }
    expect(sockets.length).toBe(3);
    expect(client.state).toBe("closed");
  });
});

describe("stream handshake refusals (QA-008)", () => {
  const refusedClient = (status: number) => {
    const timers: (() => void)[] = [];
    const errors: { code: string; retryable: boolean }[] = [];
    const sockets: FakeSocket[] = [];
    const client = new StreamClient({
      url: "ws://127.0.0.1:1/v1/stream",
      headers: {},
      channels: [`workspace:${WORKSPACE}`],
      consumer: "desktop:test",
      openSocket: () => {
        const socket = new FakeSocket();
        sockets.push(socket);
        return socket;
      },
      onError: (error) => errors.push(error),
      setTimer: (callback) => {
        timers.push(callback);
        return 0;
      },
    });
    client.connect();
    const socket = must(sockets.at(-1), "a socket");
    // How the `ws` client reports a handshake the server answered with an HTTP status.
    socket.emit("error", { message: `Unexpected server response: ${String(status)}` });
    socket.close();
    return { client, timers, errors, sockets };
  };

  it("reports a refused credential instead of retrying it", () => {
    const unauthorized = refusedClient(401);
    expect(unauthorized.errors).toEqual([expect.objectContaining({ code: "AUTH_INVALID_TOKEN", retryable: false })]);
    expect(unauthorized.timers).toHaveLength(0);
    expect(unauthorized.client.state).toBe("closed");
    const forbidden = refusedClient(403);
    expect(forbidden.errors).toEqual([expect.objectContaining({ code: "SCOPE_FORBIDDEN" })]);
    expect(forbidden.timers).toHaveLength(0);
  });

  it("still retries a server that failed for another reason", () => {
    const unavailable = refusedClient(502);
    expect(unavailable.errors).toEqual([]);
    expect(unavailable.timers).toHaveLength(1);
    expect(unavailable.client.state).toBe("reconnecting");
  });
});

describe("deep links", () => {
  it("parses only exact quansio://<kind>/<id> links into typed intents", () => {
    expect(parseDeepLink(`quansio://thread/${THREAD}`)).toEqual({ kind: "open-thread", threadId: THREAD });
    expect(parseDeepLink(`quansio://run/${RUN}`)).toEqual({ kind: "open-run", runId: RUN });
    expect(parseDeepLink(`quansio://workspace/${WORKSPACE}`)).toEqual({ kind: "open-workspace", workspaceId: WORKSPACE });
    expect(parseDeepLink("quansio://thread/../etc")).toBeNull();
    expect(parseDeepLink("quansio://thread/thr_short")).toBeNull();
    expect(parseDeepLink(`quansio://run/${THREAD}`)).toBeNull();
    expect(parseDeepLink(`quansio://thread/${THREAD}?x=1`)).toBeNull();
    expect(parseDeepLink(`https://thread/${THREAD}`)).toBeNull();
    expect(parseDeepLink("quansio://exec/rm -rf")).toBeNull();
    expect(parseDeepLink("")).toBeNull();
    expect(deepLinkFromArgv(["electron", "--flag", `quansio://run/${RUN}`])).toBe(`quansio://run/${RUN}`);
    expect(deepLinkFromArgv(["electron"])).toBeNull();
  });
});

describe("update hooks", () => {
  it("reports unavailable on unsigned builds and drives the platform updater when signed", () => {
    expect(updateFeedUrl("http://127.0.0.1:8080/", "beta", "darwin", "arm64")).toBe("http://127.0.0.1:8080/v1/desktop/updates/beta/darwin-arm64");
    const unsigned = new UpdateHooks("stable", null, "http://x/feed", false);
    expect(unsigned.status()).toMatchObject({ state: "unavailable" });
    expect(unsigned.install()).toBe(false);

    const handlers = new Map<string, (...args: unknown[]) => void>();
    const calls: string[] = [];
    const updater = {
      setFeedURL: (options: { url: string }) => calls.push(`feed:${options.url}`),
      checkForUpdates: () => calls.push("check"),
      quitAndInstall: () => calls.push("install"),
      on: (event: string, listener: (...args: unknown[]) => void) => handlers.set(event, listener),
    } as unknown as AutoUpdaterLike;
    const signed = new UpdateHooks("beta", updater, "http://x/feed", true);
    const seen: string[] = [];
    signed.onStatus((status) => seen.push(status.state));
    expect(signed.status()).toEqual({ state: "idle", channel: "beta" });
    signed.checkForUpdates();
    handlers.get("checking-for-update")?.();
    handlers.get("update-downloaded")?.({}, "notes", "1.2.3");
    expect(signed.status()).toEqual({ state: "downloaded", channel: "beta", version: "1.2.3" });
    expect(signed.install()).toBe(true);
    expect(calls).toEqual(["feed:http://x/feed", "check", "install"]);
    expect(seen).toEqual(["checking", "downloaded"]);
  });
});

describe("IPC bridge", () => {
  function bridgeWith(dir: string, fetchImpl: (input: string, init?: RequestInit) => Promise<Response>, setAppearance?: (theme: string) => void) {
    const pushed: { channel: string; payload: unknown }[] = [];
    const sockets: FakeSocket[] = [];
    const bridge = new DesktopBridge({
      serverOrigin: "http://127.0.0.1:8080",
      auth: {
        kind: "trusted-headers",
        scope: { tenantId: "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", userId: "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" },
        workspaceId: WORKSPACE,
      },
      session: new SessionFile(dir),
      openSocket: () => {
        const socket = new FakeSocket();
        sockets.push(socket);
        return socket;
      },
      updates: new UpdateHooks("stable", null, "http://x", false),
      consumer: "desktop:test",
      push: (channel, payload) => pushed.push({ channel, payload }),
      fetch: fetchImpl,
      ...(setAppearance ? { setAppearance } : {}),
    });
    return { bridge, pushed, sockets };
  }

  it("applies only a theme the person can choose to the native chrome", async () => {
    const applied: string[] = [];
    const { bridge } = bridgeWith(userData(), () => Promise.reject(new Error("unused")), (theme) => applied.push(theme));
    const handlers = bridge.handlers();
    await expect(handlers["appearance.set"]({ theme: "dark" })).resolves.toEqual({ theme: "dark" });
    await expect(handlers["appearance.set"]({ theme: "system" })).resolves.toEqual({ theme: "system" });
    await expect(handlers["appearance.set"]({ theme: "blue" })).rejects.toThrow(/system, light or dark/);
    await expect(handlers["appearance.set"]({})).rejects.toThrow(/system, light or dark/);
    await expect(handlers["appearance.set"]("dark")).rejects.toThrow();
    expect(applied).toEqual(["dark", "system"]);
  });

  it("registers exactly the allowlisted channels", () => {
    const registered: string[] = [];
    const { bridge } = bridgeWith(userData(), () => Promise.reject(new Error("unused")));
    registerBridgeHandlers({ handle: (channel) => registered.push(channel) }, bridge);
    expect(registered).toEqual([...PRELOAD_ALLOWLIST]);
  });

  it("validates renderer payloads as untrusted data", async () => {
    const { bridge } = bridgeWith(userData(), () => Promise.reject(new Error("unused")));
    const handlers = bridge.handlers();
    await expect(handlers["commands.invoke"]({ name: "PostMessage", commandId: "not-a-ulid", params: {} })).rejects.toThrow(/cmd_ ULID/);
    await expect(handlers["commands.invoke"]({ name: "../admin", commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" })).rejects.toThrow(/invalid command name/);
    await expect(handlers["commands.invoke"]("PostMessage")).rejects.toThrow();
    await expect(handlers["deepLink.open"]({})).rejects.toThrow(/needs \{url\}/);
    expect(await handlers["deepLink.open"]({ url: "quansio://exec/x" })).toBeNull();
  });

  it("attaches the active run from a PostMessage result, streams with scope headers main holds, and never exposes scope", async () => {
    const requests: { url: string; headers: Record<string, string>; body: string }[] = [];
    const { bridge, pushed, sockets } = bridgeWith(userData(), (input, init) => {
      requests.push({ url: input, headers: init?.headers as Record<string, string>, body: typeof init?.body === "string" ? init.body : "" });
      return Promise.resolve(
        new Response(JSON.stringify({ command_id: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", replayed: false, result: { thread_id: THREAD, run_id: RUN } }), { status: 200 }),
      );
    });
    const handlers = bridge.handlers();
    const before = await handlers["session.activeRun"]({});
    expect(before).toEqual({ connected: false, workspaceId: WORKSPACE, activeRun: null, streamState: "closed" });

    await handlers["commands.invoke"]({ name: "PostMessage", commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params: { workspace_id: WORKSPACE, content: "hi" } });
    expect(requests[0]?.url).toBe("http://127.0.0.1:8080/v1/commands/PostMessage");
    expect(requests[0]?.headers["x-quansio-tenant"]).toBe("tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(JSON.parse(must(requests[0], "the first request").body)).toEqual({ command_id: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params: { workspace_id: WORKSPACE, content: "hi" } });

    const projection = (await handlers["session.activeRun"]({})) as { activeRun: { runId: string } | null };
    expect(projection.activeRun?.runId).toBe(RUN);
    expect(JSON.stringify(projection)).not.toContain("tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(JSON.stringify(projection)).not.toContain("usr_");

    await handlers["stream.subscribe"]({ cursor: "renderer-supplied-cursor-must-be-ignored" });
    const socket = must(sockets.at(-1), "the stream socket");
    socket.open();
    const subscribe = JSON.parse(must(socket.sent[0], "a subscribe request")) as { subscribe: { channel: string }[]; cursor?: string };
    // One durable subscription, the workspace: a cursor is bound to its channel set.
    expect(subscribe.subscribe.map((entry) => entry.channel)).toEqual([`workspace:${WORKSPACE}`]);
    expect(subscribe.cursor).toBeUndefined();

    socket.message({ kind: "event", cursor: "c-1", event: { type: "message.posted" } });
    socket.message({ kind: "live", channel: `run:${RUN}`, frame: { kind: "model.delta", text: "…" } });
    expect(bridge.session()?.streamCursor).toBe("c-1");
    expect(bridge.session()?.activeRun?.streamCursor).toBe("c-1");
    expect(pushed.filter((entry) => entry.channel === "stream.frame")).toHaveLength(2);
    expect(pushed.some((entry) => entry.channel === "stream.state" && entry.payload === "open")).toBe(true);

    // Reconnect resumes from the durable cursor with the same run still attached.
    await handlers["stream.reconnect"]({});
    const reopened = must(sockets.at(-1), "the reopened socket");
    reopened.open();
    expect(subscribeCursor(reopened)).toBe("c-1");
    const after = (await handlers["session.activeRun"]({})) as { activeRun: { runId: string } | null; connected: boolean };
    expect(after.activeRun?.runId).toBe(RUN);
    expect(after.connected).toBe(true);

    // The server refuses the cursor outright (non-retryable): the bridge forgets it and
    // resumes from the server's current position instead of repeating the refusal.
    reopened.message({ code: "INTERNAL", message: "event store error: cursor stream mismatch", correlation_id: "stream", retryable: false });
    const fresh = must(sockets.at(-1), "the fresh socket");
    expect(fresh).not.toBe(reopened);
    fresh.open();
    expect(subscribeCursor(fresh)).toBeUndefined();
    expect(bridge.session()?.streamCursor).toBeNull();
    expect(bridge.session()?.activeRun?.runId).toBe(RUN);
    expect(pushed.some((entry) => entry.channel === "stream.error")).toBe(true);
    bridge.close();
  });
});
