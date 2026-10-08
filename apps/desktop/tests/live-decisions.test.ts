/**
 * The person's side of two user-only browser decisions (ADP-001, DOMAIN.md 8.4.1 F): the command builders refuse
 * what the server would, the one channel each travels on is allowlisted, the main process sends exactly the params
 * the command defines with a fresh command id, and a server refusal comes back as data the page shows. The server's
 * half (a service principal refused, the row unchanged) is proved against PostgreSQL in
 * `crates/server/tests/live_sessions.rs`.
 */

import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { decidePermission, originOf, resolveCertificate } from "../src/index.js";
import { DesktopBridge } from "../src/main/ipc.js";
import { SessionFile } from "../src/main/session-file.js";
import { UpdateHooks } from "../src/main/updates.js";
import { PRELOAD_ALLOWLIST } from "../src/preload/bridge.js";

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const SESSION = "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

interface Call {
  readonly method: string;
  readonly url: string;
  readonly body: Record<string, unknown> | null;
}

function bridge(calls: Call[], answer: (call: Call) => unknown, status = 200): DesktopBridge {
  return new DesktopBridge({
    serverOrigin: "http://127.0.0.1:8080",
    auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
    session: new SessionFile(mkdtempSync(join(tmpdir(), "quansio-decision-"))),
    openSocket: () => {
      throw new Error("no socket in this test");
    },
    updates: new UpdateHooks("stable", null, "http://x", false),
    consumer: "desktop:test",
    push: () => undefined,
    fetch: (url, init) => {
      const call: Call = { method: init?.method ?? "GET", url, body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null };
      calls.push(call);
      return Promise.resolve(new Response(JSON.stringify(answer(call)), { status, headers: { "content-type": "application/json" } }));
    },
  });
}

describe("the command builders", () => {
  it("build the commands the catalog defines", () => {
    expect(resolveCertificate(SESSION, "cdp-1", "crt_1", "allow")).toEqual({
      name: "ResolveBrowserCertificate",
      params: { session_id: SESSION, tab_id: "cdp-1", certificate_id: "crt_1", decision: "allow" },
    });
    expect(decidePermission(SESSION, "cdp-1", "https://example.com", "camera", "deny")).toEqual({
      name: "DecideBrowserPermission",
      params: { session_id: SESSION, tab_id: "cdp-1", origin: "https://example.com", permission: "camera", decision: "deny" },
    });
  });

  it("refuse what the server would refuse, before anything is sent", () => {
    expect(() => resolveCertificate("tgt_x", "cdp-1", "crt_1", "allow")).toThrow(/bsn_/);
    expect(() => resolveCertificate(SESSION, "", "crt_1", "allow")).toThrow();
    expect(() => resolveCertificate(SESSION, "cdp-1", "", "allow")).toThrow();
    for (const origin of ["example.com", "https://example.com/path", "https://u@example.com", "ftp://example.com", "about:blank", ""]) {
      expect(() => decidePermission(SESSION, "cdp-1", origin, "camera", "allow"), origin).toThrow();
    }
    // Only camera and microphone exist; the type system forbids the rest and the runtime check backs it.
    expect(() => decidePermission(SESSION, "cdp-1", "https://example.com", "geolocation" as never, "allow")).toThrow(/camera and microphone/);
  });

  it("reduce a page address to an origin, or to nothing", () => {
    expect(originOf("https://example.com/a/b?c=1#d")).toBe("https://example.com");
    expect(originOf("http://127.0.0.1:8443/x")).toBe("http://127.0.0.1:8443");
    expect(originOf("about:blank")).toBeNull();
    expect(originOf("chrome://settings")).toBeNull();
    expect(originOf("not a url")).toBeNull();
  });
});

describe("the channels", () => {
  it("are allowlisted, and the generic command channel does not need them", () => {
    expect(PRELOAD_ALLOWLIST).toContain("live.certificate");
    expect(PRELOAD_ALLOWLIST).toContain("live.permission");
  });

  it("send the command with a fresh id and the params the command defines", async () => {
    const calls: Call[] = [];
    const handlers = bridge(calls, () => ({ command_id: "c", accepted_at: "now", replayed: false, result: { receipt: { kind: "certificate", decision: "allow" } } })).handlers();
    const settled = await handlers["live.certificate"]({ sessionId: SESSION, tabId: "cdp-1", certificateId: "crt_1", decision: "allow" });
    expect(settled).toMatchObject({ ok: true, result: { receipt: { kind: "certificate", decision: "allow" } } });
    expect(calls[0]?.url).toMatch(/\/v1\/commands\/ResolveBrowserCertificate$/);
    expect(calls[0]?.body?.["params"]).toEqual({ session_id: SESSION, tab_id: "cdp-1", certificate_id: "crt_1", decision: "allow" });
    expect(String(calls[0]?.body?.["command_id"])).toMatch(/^cmd_/);

    await handlers["live.permission"]({ sessionId: SESSION, tabId: "cdp-1", origin: "https://example.com", permission: "microphone", decision: "allow" });
    expect(calls[1]?.url).toMatch(/\/v1\/commands\/DecideBrowserPermission$/);
    expect(calls[1]?.body?.["params"]).toEqual({ session_id: SESSION, tab_id: "cdp-1", origin: "https://example.com", permission: "microphone", decision: "allow" });
    expect(calls[1]?.body?.["command_id"]).not.toBe(calls[0]?.body?.["command_id"]);
  });

  it("return a refusal as data and send nothing for a payload the builders refuse", async () => {
    const refused: Call[] = [];
    const handlers = bridge(refused, () => ({ code: "SCOPE_FORBIDDEN", message: "a person's decision", correlation_id: "c", retryable: false }), 403).handlers();
    expect(await handlers["live.certificate"]({ sessionId: SESSION, tabId: "cdp-1", certificateId: "crt_1", decision: "allow" })).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    const before = refused.length;
    for (const bad of [
      { sessionId: SESSION, tabId: "cdp-1", origin: "https://example.com", permission: "geolocation", decision: "allow" },
      { sessionId: SESSION, tabId: "cdp-1", origin: "https://example.com/x", permission: "camera", decision: "allow" },
      { sessionId: SESSION, tabId: "cdp-1", origin: "https://example.com", permission: "camera", decision: "maybe" },
    ]) {
      expect(await handlers["live.permission"](bad)).toMatchObject({ ok: false });
    }
    expect(await handlers["live.certificate"]({ sessionId: "tgt_x", tabId: "cdp-1", certificateId: "crt_1", decision: "allow" })).toMatchObject({ ok: false });
    expect(refused.length).toBe(before);
  });
});

describe("the live surface", () => {
  const source = readFileSync(join(__dirname, "..", "src", "renderer", "Live.tsx"), "utf8");

  it("offers the two decisions as buttons that call only the person's channels", () => {
    expect(source).toContain("window.quansio.resolveCertificate");
    expect(source).toContain("window.quansio.decidePermission");
    expect(source).toContain("live-certificate-continue");
    expect(source).toContain("live-permission-camera");
  });
});
