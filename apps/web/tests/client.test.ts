/**
 * APP-012: the web client addresses only `/v1/`, presents the session as a bearer (or
 * the harness scope as headers), mints command ids, and turns the §15 error body into a
 * typed error the page can branch on.
 */

import { describe, expect, it } from "vitest";

import { ApiError, SESSION_KEY, WebClient, WebSession, newCommandId, parseStoredSession, publicPath } from "../src/index.js";

function fakeFetch(responder: (input: string, init: RequestInit) => { status: number; body: unknown }) {
  const calls: { input: string; init: RequestInit }[] = [];
  const fetchImpl = (input: string, init: RequestInit): Promise<Response> => {
    calls.push({ input, init });
    const { status, body } = responder(input, init);
    return Promise.resolve(new Response(body === undefined ? null : JSON.stringify(body), { status, headers: { "content-type": "application/json" } }));
  };
  return { calls, fetchImpl };
}

describe("web client", () => {
  it("addresses /v1 only, with the credential as a bearer or as harness headers", async () => {
    expect(publicPath("https://q.example", "/v1/audit")).toBe("https://q.example/v1/audit");
    expect(() => publicPath("https://q.example", "/admin")).toThrow(/not a public API path/);
    const { calls, fetchImpl } = fakeFetch(() => ({ status: 200, body: { ok: true } }));
    const bearer = new WebClient("https://q.example/", { kind: "bearer", accessToken: () => "tok" }, fetchImpl);
    await bearer.get("/v1/me");
    expect(calls[0]?.input).toBe("https://q.example/v1/me");
    expect((calls[0]?.init.headers as Record<string, string>)["authorization"]).toBe("Bearer tok");
    const harness = new WebClient("https://q.example", { kind: "trusted-headers", tenantId: "tn_1", userId: "usr_1" }, fetchImpl);
    await harness.get("/v1/members");
    expect((calls[1]?.init.headers as Record<string, string>)["x-quansio-tenant"]).toBe("tn_1");
    expect((calls[1]?.init.headers as Record<string, string>)["x-quansio-user"]).toBe("usr_1");
    expect((calls[1]?.init.headers as Record<string, string>)["authorization"]).toBeUndefined();
  });

  it("sends a command envelope with a fresh command id and returns the owner's result", async () => {
    const { calls, fetchImpl } = fakeFetch((input) => ({ status: 200, body: { command_id: "cmd_x", replayed: false, result: { input } } }));
    const client = new WebClient("https://q.example", { kind: "bearer", accessToken: () => "tok" }, fetchImpl);
    const result = await client.invoke<{ input: string }>("SetPolicy", { scope: "tenant", rules: [] });
    expect(result.input).toBe("https://q.example/v1/commands/SetPolicy");
    const envelope = JSON.parse(calls[0]?.init.body as string) as { command_id: string; params: unknown };
    expect(envelope.command_id).toMatch(/^cmd_[0-9A-HJKMNP-TV-Z]{26}$/);
    expect(envelope.params).toEqual({ scope: "tenant", rules: [] });
    expect(newCommandId()).not.toBe(newCommandId());
    await expect(client.invoke("../v1/commands/x", {})).rejects.toThrow(/not a command name/);
  });

  it("surfaces the §15 error body as a typed error, and knows an RBAC refusal", async () => {
    const { fetchImpl } = fakeFetch(() => ({
      status: 403,
      body: { code: "SCOPE_FORBIDDEN", message: "manage_tenant is not allowed for this user's role (rbac_insufficient_role)", correlation_id: "c1", retryable: false, details: { kind: "rbac", action: "manage_tenant", reason: "rbac_insufficient_role" } },
    }));
    const client = new WebClient("https://q.example", { kind: "bearer", accessToken: () => "tok" }, fetchImpl);
    const error = await client.get("/v1/audit").catch((failure: unknown) => failure);
    expect(error).toBeInstanceOf(ApiError);
    if (error instanceof ApiError) {
      expect(error.code).toBe("SCOPE_FORBIDDEN");
      expect(error.status).toBe(403);
      expect(error.isRbacRefusal).toBe(true);
      expect(error.correlationId).toBe("c1");
    }
    const { fetchImpl: plain } = fakeFetch(() => ({ status: 502, body: "not json" }));
    const other = await new WebClient("https://q.example", { kind: "bearer", accessToken: () => null }, plain).get("/v1/me").catch((failure: unknown) => failure);
    expect(other).toBeInstanceOf(ApiError);
    if (other instanceof ApiError) {
      expect(other.code).toBe("INTERNAL");
      expect(other.isRbacRefusal).toBe(false);
    }
  });
});

describe("web session", () => {
  const storage = () => {
    const map = new Map<string, string>();
    return { getItem: (key: string) => map.get(key) ?? null, setItem: (key: string, value: string) => { map.set(key, value); }, removeItem: (key: string) => { map.delete(key); }, map };
  };

  it("keeps a bearer session for the tab, refreshes once on AUTH_EXPIRED, and forgets it on sign-out", async () => {
    expect(parseStoredSession(null)).toBeNull();
    expect(parseStoredSession("{")).toBeNull();
    expect(parseStoredSession(JSON.stringify({ access_token: "a" }))).toBeNull();
    const store = storage();
    let expired = true;
    const { calls, fetchImpl } = fakeFetch((input, init) => {
      const auth = (init.headers as Record<string, string>)["authorization"];
      if (input.endsWith("/v1/auth/magic-link/complete")) {
        return { status: 200, body: { session_id: "ses_1", access_token: "old", refresh_token: "r1", tenant_id: "tn_1", user_id: "usr_1", workspace_id: "ws_1" } };
      }
      if (input.endsWith("/v1/auth/refresh")) {
        expired = false;
        return { status: 200, body: { session_id: "ses_1", access_token: "new", refresh_token: "r2", tenant_id: "tn_1", user_id: "usr_1" } };
      }
      if (input.endsWith("/v1/auth/sign-out")) {
        return { status: 200, body: {} };
      }
      if (expired && auth === "Bearer old") {
        return { status: 401, body: { code: "AUTH_EXPIRED", message: "expired", correlation_id: "c", retryable: false } };
      }
      return { status: 200, body: { seen: auth } };
    });
    const session = new WebSession("https://q.example", { storage: store, fetchImpl });
    expect(session.signedIn).toBe(false);
    await session.completeMagicLink("qml_token");
    expect(session.signedIn).toBe(true);
    expect(parseStoredSession(store.getItem(SESSION_KEY))?.workspace_id).toBe("ws_1");
    const seen = await session.client().get<{ seen: string }>("/v1/me");
    expect(seen.seen).toBe("Bearer new");
    expect(calls.filter((call) => call.input.endsWith("/v1/auth/refresh"))).toHaveLength(1);
    expect(parseStoredSession(store.getItem(SESSION_KEY))?.access_token).toBe("new");
    // A second session from the same storage picks the session up.
    expect(new WebSession("https://q.example", { storage: store, fetchImpl }).signedIn).toBe(true);
    await session.signOut();
    expect(session.signedIn).toBe(false);
    expect(store.getItem(SESSION_KEY)).toBeNull();
    // A harness scope is a credential of headers, never a bearer.
    const harness = new WebSession("https://q.example", { fetchImpl, harness: { tenantId: "tn_1", userId: "usr_2" } });
    expect(harness.signedIn).toBe(true);
    expect(harness.credential()).toEqual({ kind: "trusted-headers", tenantId: "tn_1", userId: "usr_2" });
  });
});
