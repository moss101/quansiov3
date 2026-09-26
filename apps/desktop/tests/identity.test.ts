/**
 * Unit tests for the desktop's identity half (APP-002): keychain-backed token custody, the
 * session manager and client (refresh on expiry, custody cleared on refusal), the onboarding
 * state machine, and the bridge's auth/settings channels with the payloads validated as
 * untrusted data. The server is a recording fetch double here; the new-user E2E against the
 * real binary is `onboarding.e2e.test.ts`.
 */

import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { DesktopBridge, SessionFile, UpdateHooks, parseDeepLink } from "../src/index.js";
import { ServerClient, type Session } from "../src/client/server.js";
import { SessionManager } from "../src/main/session-manager.js";
import { NO_ENCRYPTION, TokenStore, type SecretCipher } from "../src/main/token-store.js";
import { INITIAL_ONBOARDING, reduceOnboarding, recommendedExecutionTarget } from "../src/onboarding/flow.js";

const temporaryDirs: string[] = [];
function userData(): string {
  const dir = mkdtempSync(join(tmpdir(), "quansio-identity-"));
  temporaryDirs.push(dir);
  return dir;
}
afterEach(() => {
  for (const dir of temporaryDirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** A cipher double: reversible, and visibly not plaintext. */
const XOR_CIPHER: SecretCipher = {
  isEncryptionAvailable: () => true,
  encryptString: (plain) => Buffer.from(Buffer.from(plain, "utf8").map((byte) => byte ^ 0x5a)),
  decryptString: (encrypted) => Buffer.from(encrypted.map((byte) => byte ^ 0x5a)).toString("utf8"),
};

function session(overrides: Partial<Session> = {}): Session {
  return {
    session_id: "ses_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    user_id: "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    access_token: "eyJ.access.1",
    access_expires_at: new Date(Date.now() + 900_000).toISOString(),
    refresh_token: "qrt_one",
    refresh_expires_at: new Date(Date.now() + 86_400_000).toISOString(),
    workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    onboarded: true,
    ...overrides,
  };
}

/** A server double keyed by method+path; records every request. */
function fakeServer(routes: Record<string, (body: unknown, headers: Record<string, string>) => { status?: number; body: unknown }>) {
  const requests: { method: string; path: string; headers: Record<string, string>; body: unknown }[] = [];
  const fetchImpl = (input: string, init?: RequestInit): Promise<Response> => {
    const url = new URL(input);
    const method = init?.method ?? "GET";
    const headers = (init?.headers as Record<string, string> | undefined) ?? {};
    const body = typeof init?.body === "string" && init.body ? (JSON.parse(init.body) as unknown) : null;
    requests.push({ method, path: url.pathname + url.search, headers, body });
    const route = routes[`${method} ${url.pathname}`];
    if (!route) {
      return Promise.resolve(new Response(JSON.stringify({ code: "NOT_FOUND", message: url.pathname, correlation_id: "x", retryable: false }), { status: 404 }));
    }
    const result = route(body, headers);
    return Promise.resolve(new Response(JSON.stringify(result.body), { status: result.status ?? 200 }));
  };
  return { requests, fetchImpl };
}

describe("token store", () => {
  it("keeps the refresh token only encrypted, and nothing at all without a platform cipher", () => {
    const dir = userData();
    const store = new TokenStore(dir, XOR_CIPHER);
    expect(store.persistent()).toBe(true);
    expect(store.load()).toBeNull();
    const stored = { serverOrigin: "http://127.0.0.1:8080", refreshToken: "qrt_secret", refreshExpiresAt: new Date(Date.now() + 1000).toISOString(), userId: "usr_1", tenantId: "tn_1" };
    expect(store.save(stored)).toBe(true);
    const bytes = readFileSync(store.path);
    expect(bytes.toString("utf8")).not.toContain("qrt_secret");
    expect(new TokenStore(dir, XOR_CIPHER).load()).toEqual(stored);
    store.clear();
    expect(existsSync(store.path)).toBe(false);

    const none = new TokenStore(userData(), NO_ENCRYPTION);
    expect(none.persistent()).toBe(false);
    expect(none.save(stored)).toBe(false);
    expect(existsSync(none.path)).toBe(false);
    expect(none.load()).toBeNull();
  });
});

describe("session manager and client", () => {
  it("signs a person out when the server says their session was revoked (QA-008)", async () => {
    // The session is revoked at the server: the access token is refused with AUTH_INVALID_TOKEN,
    // and so is the refresh token. The client tries one refresh, finds the session gone, and the
    // manager signs out and clears custody instead of surfacing a raw refusal forever.
    const server = fakeServer({
      "POST /v1/auth/magic-link/complete": () => ({ body: session() }),
      "GET /v1/me": () => ({ status: 401, body: { code: "AUTH_INVALID_TOKEN", message: "session was revoked", correlation_id: "x", retryable: false } }),
      "POST /v1/auth/refresh": () => ({ status: 401, body: { code: "AUTH_INVALID_TOKEN", message: "session was revoked", correlation_id: "x", retryable: false } }),
    });
    const tokens = new TokenStore(userData(), XOR_CIPHER);
    const states: string[] = [];
    const manager = new SessionManager({
      serverOrigin: "http://127.0.0.1:8080",
      tokens,
      client: (credential) => new ServerClient({ origin: "http://127.0.0.1:8080", credential, fetch: server.fetchImpl }),
      onState: (state) => states.push(state.kind),
    });
    await manager.completeMagicLink("qml_good");
    expect(manager.state().kind).toBe("signed-in");
    await expect(manager.me()).rejects.toMatchObject({ code: "AUTH_INVALID_TOKEN" });
    expect(server.requests.filter((request) => request.path === "/v1/auth/refresh")).toHaveLength(1);
    expect(manager.state()).toEqual({ kind: "signed-out", persistent: true });
    expect(tokens.load()).toBeNull();
    expect(states.at(-1)).toBe("signed-out");
  });

  it("signs in by magic link, presents the bearer, refreshes once on AUTH_EXPIRED, and clears custody when refused", async () => {
    let issued = 0;
    const server = fakeServer({
      "POST /v1/auth/magic-link/start": (body) => ({ body: { challenge_id: "ach_1", email: (body as { email: string }).email.toLowerCase(), expires_at: "2026-01-01T00:00:00Z", delivery: "recording" } }),
      "POST /v1/auth/magic-link/complete": (body) =>
        (body as { token: string }).token === "qml_good"
          ? { body: session() }
          : { status: 401, body: { code: "AUTH_INVALID_TOKEN", message: "no", correlation_id: "x", retryable: false } },
      "GET /v1/me": (_body, headers) => {
        const bearer = headers["authorization"] ?? "";
        if (bearer === "Bearer eyJ.access.1") {
          return { status: 401, body: { code: "AUTH_EXPIRED", message: "expired", correlation_id: "x", retryable: false } };
        }
        if (bearer === "Bearer eyJ.access.2") {
          return { body: { user: { id: "usr_1", primary_email: "ada@example.com", display_name: "Ada", auth_methods: [] }, session: { id: "ses_2", tenant_id: "tn_1" }, tenants: [], workspaces: [{ id: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", name: "Personal", role: "admin", effective_settings: {} }], auth_methods_offered: { email_magic_link: true, oauth: [] } } };
        }
        return { status: 401, body: { code: "AUTH_REQUIRED", message: "none", correlation_id: "x", retryable: false } };
      },
      "POST /v1/auth/refresh": (body) => {
        issued += 1;
        if ((body as { refresh_token: string }).refresh_token === "qrt_one" && issued === 1) {
          return { body: session({ access_token: "eyJ.access.2", refresh_token: "qrt_two", session_id: "ses_2" }) };
        }
        return { status: 401, body: { code: "AUTH_INVALID_TOKEN", message: "rotated", correlation_id: "x", retryable: false } };
      },
      "POST /v1/auth/sign-out": () => ({ body: { signed_out: true, session_id: "ses_2" } }),
    });
    const tokens = new TokenStore(userData(), XOR_CIPHER);
    const states: string[] = [];
    const manager = new SessionManager({
      serverOrigin: "http://127.0.0.1:8080",
      tokens,
      client: (credential) => new ServerClient({ origin: "http://127.0.0.1:8080", credential, fetch: server.fetchImpl }),
      onState: (state) => states.push(state.kind),
    });
    expect(manager.state()).toEqual({ kind: "signed-out", persistent: true });

    await manager.startMagicLink("Ada@Example.com", "Ada");
    expect(manager.state()).toMatchObject({ kind: "link-sent", email: "ada@example.com" });
    await expect(manager.completeMagicLink("qml_bad")).rejects.toMatchObject({ code: "AUTH_INVALID_TOKEN" });
    await manager.completeMagicLink("qml_good");
    expect(manager.state()).toMatchObject({ kind: "signed-in", workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", onboarded: true });
    expect(tokens.load()?.refreshToken).toBe("qrt_one");

    // The access token has expired at the server: one refresh, then the same request succeeds.
    const me = await manager.me();
    expect(me.user.primary_email).toBe("ada@example.com");
    const meRequests = server.requests.filter((request) => request.path === "/v1/me");
    expect(meRequests.map((request) => request.headers["authorization"])).toEqual(["Bearer eyJ.access.1", "Bearer eyJ.access.2"]);
    expect(manager.accessToken()).toBe("eyJ.access.2");
    expect(tokens.load()?.refreshToken).toBe("qrt_two");

    // A refresh the server refuses ends the session and clears custody.
    expect(await manager.refresh()).toBeNull();
    expect(manager.state()).toEqual({ kind: "signed-out", persistent: true });
    expect(tokens.load()).toBeNull();
    expect(states).toEqual(["link-sent", "signed-in", "signed-in", "signed-out"]);
    // No request ever carried the refresh token in a header, and none carried a scope header.
    for (const request of server.requests) {
      expect(JSON.stringify(request.headers)).not.toContain("qrt_");
      expect(request.headers["x-quansio-tenant"]).toBeUndefined();
    }
  });

  it("resumes from the keychain on launch, and forgets an expired or foreign stored session", async () => {
    const server = fakeServer({
      "POST /v1/auth/refresh": (body) =>
        (body as { refresh_token: string }).refresh_token === "qrt_stored"
          ? { body: { ...session({ access_token: "eyJ.access.9", refresh_token: "qrt_next" }), workspace_id: undefined } }
          : { status: 401, body: { code: "AUTH_INVALID_TOKEN", message: "no", correlation_id: "x", retryable: false } },
      "GET /v1/me": () => ({ body: { user: { id: "usr_1", primary_email: "a@b.co", display_name: "", auth_methods: [] }, session: { id: "s", tenant_id: "tn_1" }, tenants: [], workspaces: [{ id: "ws_01J8Z3K6F1N8VQ2X5W9Y0BBBBB", name: "Personal", role: "admin", effective_settings: {} }], auth_methods_offered: { email_magic_link: true, oauth: [] } } }),
    });
    const make = (dir: string) =>
      new SessionManager({
        serverOrigin: "http://127.0.0.1:8080",
        tokens: new TokenStore(dir, XOR_CIPHER),
        client: (credential) => new ServerClient({ origin: "http://127.0.0.1:8080", credential, fetch: server.fetchImpl }),
      });
    const dir = userData();
    new TokenStore(dir, XOR_CIPHER).save({ serverOrigin: "http://127.0.0.1:8080", refreshToken: "qrt_stored", refreshExpiresAt: new Date(Date.now() + 60_000).toISOString(), userId: "usr_1", tenantId: "tn_1" });
    const resumed = make(dir);
    expect(await resumed.resume()).toBe(true);
    expect(resumed.state()).toMatchObject({ kind: "signed-in", workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0BBBBB" });
    expect(new TokenStore(dir, XOR_CIPHER).load()?.refreshToken).toBe("qrt_next");

    const expiredDir = userData();
    new TokenStore(expiredDir, XOR_CIPHER).save({ serverOrigin: "http://127.0.0.1:8080", refreshToken: "qrt_stored", refreshExpiresAt: new Date(Date.now() - 1).toISOString(), userId: "usr_1", tenantId: "tn_1" });
    expect(await make(expiredDir).resume()).toBe(false);
    expect(new TokenStore(expiredDir, XOR_CIPHER).load()).toBeNull();

    const foreignDir = userData();
    new TokenStore(foreignDir, XOR_CIPHER).save({ serverOrigin: "http://other:1", refreshToken: "qrt_stored", refreshExpiresAt: new Date(Date.now() + 60_000).toISOString(), userId: "usr_1", tenantId: "tn_1" });
    expect(await make(foreignDir).resume()).toBe(false);
    expect(await make(userData()).resume()).toBe(false);
  });
});

describe("onboarding flow", () => {
  it("walks sign-in → inbox → execution target → defaults → ready, and returning users skip ahead", () => {
    let state = INITIAL_ONBOARDING;
    state = reduceOnboarding(state, { kind: "session", state: { kind: "signed-out", persistent: true } });
    expect(state.step).toBe("sign-in");
    state = reduceOnboarding(state, { kind: "link-sent", email: "ada@example.com" });
    expect(state).toMatchObject({ step: "check-inbox", email: "ada@example.com" });
    state = reduceOnboarding(state, { kind: "session", state: { kind: "signed-in", userId: "u", tenantId: "t", workspaceId: "ws_1", onboarded: true } });
    expect(state).toMatchObject({ step: "choose-execution-target", workspaceId: "ws_1" });
    expect(reduceOnboarding(state, { kind: "defaults-confirmed" }).step).toBe("choose-execution-target");
    state = reduceOnboarding(state, { kind: "target-chosen", target: "local_capsule" });
    expect(state).toMatchObject({ step: "confirm-defaults", executionTarget: "local_capsule" });
    state = reduceOnboarding(state, { kind: "failed", message: "refused" });
    expect(state.error).toBe("refused");
    state = reduceOnboarding(state, { kind: "defaults-confirmed" });
    expect(state).toMatchObject({ step: "ready", error: null });
    expect(reduceOnboarding(state, { kind: "signed-out" })).toEqual(INITIAL_ONBOARDING);

    const returning = reduceOnboarding(INITIAL_ONBOARDING, { kind: "session", state: { kind: "signed-in", userId: "u", tenantId: "t", workspaceId: "ws_2", onboarded: false } });
    expect(returning).toMatchObject({ step: "ready", workspaceId: "ws_2" });
    expect(recommendedExecutionTarget("darwin")).toBe("local_capsule");
    expect(recommendedExecutionTarget("linux")).toBe("cloud");
  });
});

describe("bridge in session mode", () => {
  it("exposes only a projection of the session, validates payloads, and reaches the settings commands", async () => {
    const server = fakeServer({
      "POST /v1/auth/magic-link/start": () => ({ body: { challenge_id: "ach_1", email: "ada@example.com", expires_at: "x", delivery: "recording" } }),
      "POST /v1/auth/magic-link/complete": () => ({ body: session() }),
      "GET /v1/me": () => ({ body: { user: { id: "usr_1", primary_email: "ada@example.com", display_name: "Ada", auth_methods: ["email_magic_link"] }, session: { id: "s", tenant_id: "tn_1" }, tenants: [], workspaces: [], auth_methods_offered: { email_magic_link: true, oauth: ["github"] } } }),
      "POST /v1/commands/UpdateWorkspace": (body) => ({ body: { command_id: (body as { command_id: string }).command_id, replayed: false, result: { workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", effective_settings: { execution_target: "cloud" }, effects: [{ kind: "restart", reason: "execution target changed" }] } } }),
      "POST /v1/commands/UpdateUserSettings": (body) => ({ body: { command_id: (body as { command_id: string }).command_id, replayed: false, result: { effective_settings: { theme: "dark" }, effects: [] } } }),
      "GET /v1/auth/oauth/github/start": () => ({ body: { provider: "github", authorization_url: "https://github.com/login/oauth/authorize?x=1", state: "st", expires_at: "x" } }),
      "POST /v1/auth/sign-out": () => ({ body: { signed_out: true, session_id: "s" } }),
    });
    const opened: string[] = [];
    const pushed: { channel: string; payload: unknown }[] = [];
    const dir = userData();
    const bridge = new DesktopBridge({
      serverOrigin: "http://127.0.0.1:8080",
      auth: { kind: "session", tokens: new TokenStore(dir, XOR_CIPHER) },
      session: new SessionFile(dir),
      openSocket: () => {
        throw new Error("no socket in this test");
      },
      updates: new UpdateHooks("stable", null, "http://x", false),
      consumer: "desktop:test",
      push: (channel, payload) => pushed.push({ channel, payload }),
      openExternal: (url) => {
        opened.push(url);
        return Promise.resolve();
      },
      fetch: server.fetchImpl,
    });
    const handlers = bridge.handlers();
    expect(await handlers["session.state"]({})).toEqual({ kind: "signed-out", persistent: true });
    expect(await bridge.resume()).toBe(false);

    await expect(handlers["auth.magicLink.start"]({ email: "not-an-email" })).rejects.toThrow(/email address/);
    await expect(handlers["auth.magicLink.complete"]({ token: "not-a-token" })).rejects.toThrow(/delivered token/);
    await expect(handlers["auth.oauth.start"]({ provider: "facebook" })).rejects.toThrow(/google, microsoft or github/);
    await expect(handlers["settings.updateWorkspace"]({ workspaceId: "nope" })).rejects.toThrow(/ws_ id/);
    await expect(handlers["settings.updateUser"]({ workspaceId: "ws_1", settings: [] })).rejects.toThrow(/object/);

    await handlers["auth.magicLink.start"]({ email: "ada@example.com", displayName: "Ada" });
    expect(await handlers["session.state"]({})).toMatchObject({ kind: "link-sent" });
    const signedIn = await handlers["auth.magicLink.complete"]({ token: "qml_good" });
    expect(signedIn).toMatchObject({ kind: "signed-in", workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    expect(JSON.stringify(signedIn)).not.toContain("eyJ");
    expect(JSON.stringify(signedIn)).not.toContain("qrt_");
    expect(pushed.filter((entry) => entry.channel === "session.state").map((entry) => (entry.payload as { kind: string }).kind)).toEqual(["link-sent", "signed-in"]);
    expect(bridge.workspaceId()).toBe("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(new SessionFile(dir).current()?.workspaceId).toBe("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");

    const updated = (await handlers["settings.updateWorkspace"]({ workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", settings: { execution_target: "cloud" } })) as { effects: unknown[] };
    expect(updated.effects).toHaveLength(1);
    const sent = server.requests.find((request) => request.path === "/v1/commands/UpdateWorkspace");
    expect(sent?.headers["authorization"]).toBe("Bearer eyJ.access.1");
    expect((sent?.body as { params: { settings: unknown } }).params.settings).toEqual({ execution_target: "cloud" });
    await handlers["settings.updateUser"]({ workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", settings: { theme: "dark" } });

    const oauth = (await handlers["auth.oauth.start"]({ provider: "github" })) as { state: string; opened: boolean };
    expect(oauth).toEqual({ state: "st", opened: true });
    expect(opened).toEqual(["https://github.com/login/oauth/authorize?x=1"]);
    expect(parseDeepLink("quansio://auth/callback?code=abc&state=st")).toEqual({ kind: "oauth-callback", code: "abc", state: "st" });
    expect(parseDeepLink("quansio://auth/callback?code=abc")).toBeNull();
    expect(parseDeepLink("quansio://auth/callback?code=abc&state=st&evil=1")).toBeNull();

    expect(await handlers["auth.signOut"]({})).toEqual({ kind: "signed-out", persistent: true });
    expect(new TokenStore(dir, XOR_CIPHER).load()).toBeNull();
  });
});
