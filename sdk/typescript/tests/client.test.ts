/**
 * APP-013: the TypeScript SDK's operation table is the OpenAPI document's, its paths are
 * versioned, its envelope is the server's, and its errors are typed — pure, no server.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { ApiError, ERROR_CODES, GENERATED_OPERATIONS, OPERATIONS, PUBLIC_API_VERSION, QuansioClient, READ_PROJECTIONS, newCommandId, publicApiPath } from "../src/index.js";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

function fakeFetch(responder: (input: string, init: RequestInit) => { status: number; body: unknown }) {
  const calls: { input: string; init: RequestInit }[] = [];
  const fetchImpl = (input: string, init: RequestInit): Promise<Response> => {
    calls.push({ input, init });
    const { status, body } = responder(input, init);
    return Promise.resolve(new Response(body === undefined ? null : JSON.stringify(body), { status, headers: { "content-type": "application/json" } }));
  };
  return { calls, fetchImpl };
}

describe("operation table", () => {
  it("is exactly the OpenAPI document's operation ids, and the projections the catalog names", () => {
    const openapi = readFileSync(join(REPO, "schemas", "openapi", "public-api-v1.yaml"), "utf8");
    // The commands are the OpenAPI paths under /v1/commands/; the auth endpoints are the
    // session flow the SDK exposes as methods, not as operations.
    const documented = [...openapi.matchAll(/^ {2}\/v1\/commands\/(\w+):/gm)].map((match) => match[1] ?? "").sort();
    expect([...GENERATED_OPERATIONS].sort()).toEqual(documented);
    for (const [id, entry] of Object.entries(OPERATIONS)) {
      expect(openapi).toContain(`operationId: ${id}`);
      expect(entry.family.length).toBeGreaterThan(0);
    }
    for (const id of GENERATED_OPERATIONS) {
      expect(OPERATIONS[id].path).toBe(`/v1/commands/${id}`);
      expect(OPERATIONS[id].method).toBe("POST");
      expect(openapi).toContain(`/v1/commands/${id}:`);
    }
    const catalog = readFileSync(join(REPO, "schemas", "catalog", "read-projections.yaml"), "utf8");
    for (const projection of READ_PROJECTIONS) {
      expect(catalog).toContain(`- ${projection}`);
    }
    const errors = readFileSync(join(REPO, "schemas", "catalog", "errors.yaml"), "utf8");
    for (const code of ERROR_CODES) {
      expect(errors).toContain(code);
    }
    expect(ERROR_CODES).toContain("SCOPE_FORBIDDEN");
  });

  it("version-prefixes every resource and refuses what is not a resource", () => {
    expect(PUBLIC_API_VERSION).toBe("v1");
    expect(publicApiPath("runs")).toBe("/v1/runs");
    expect(publicApiPath("approvals?workspace_id=ws_1")).toBe("/v1/approvals?workspace_id=ws_1");
    expect(publicApiPath("messages?thread_id=thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA")).toBe("/v1/messages?thread_id=thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    for (const bad of ["", "/runs", "Runs", "a".repeat(257), "../admin", "runs/../x", "runs x"]) {
      expect(() => publicApiPath(bad)).toThrow(/invalid public API resource/);
    }
  });
});

describe("client", () => {
  it("sends the command envelope for a catalogued operation and returns the result", async () => {
    const { calls, fetchImpl } = fakeFetch((input) => ({ status: 200, body: { command_id: "cmd_x", replayed: false, result: { at: input } } }));
    const client = new QuansioClient("https://q.example/", { kind: "bearer", accessToken: () => "tok" }, fetchImpl);
    const result = await client.command<{ at: string }>("PostMessage", { workspace_id: "ws_1", content: "hi" });
    expect(result.at).toBe("https://q.example/v1/commands/PostMessage");
    const envelope = JSON.parse(calls[0]?.init.body as string) as { command_id: string; params: unknown };
    expect(envelope.command_id).toMatch(/^cmd_[0-9A-HJKMNP-TV-Z]{26}$/);
    expect(envelope.params).toEqual({ workspace_id: "ws_1", content: "hi" });
    expect((calls[0]?.init.headers as Record<string, string>)["authorization"]).toBe("Bearer tok");
    expect((calls[0]?.init.headers as Record<string, string>)["x-quansio-client"]).toBe("sdk-typescript");
    expect(newCommandId()).not.toBe(newCommandId());
    const replayed = await client.commandEnvelope("PostMessage", {}, "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(replayed.command_id).toBe("cmd_x");
    const harness = new QuansioClient("https://q.example", { kind: "trusted-headers", tenantId: "tn_1", userId: "usr_1" }, fetchImpl);
    await harness.get("me");
    expect((calls[2]?.init.headers as Record<string, string>)["x-quansio-tenant"]).toBe("tn_1");
  });

  it("types the §15 error body and knows whether its code is catalogued", async () => {
    const { fetchImpl } = fakeFetch(() => ({ status: 403, body: { code: "SCOPE_FORBIDDEN", message: "no", correlation_id: "c1", retryable: false, details: { kind: "rbac" } } }));
    const client = new QuansioClient("https://q.example", { kind: "bearer", accessToken: () => null }, fetchImpl);
    const error = await client.get("audit").catch((failure: unknown) => failure);
    expect(error).toBeInstanceOf(ApiError);
    if (error instanceof ApiError) {
      expect(error.code).toBe("SCOPE_FORBIDDEN");
      expect(error.isCatalogued).toBe(true);
      expect(error.status).toBe(403);
      expect(error.details).toEqual({ kind: "rbac" });
    }
    const { fetchImpl: odd } = fakeFetch(() => ({ status: 500, body: { code: "MADE_UP", message: "?", correlation_id: "", retryable: true } }));
    const other = await new QuansioClient("https://q.example", { kind: "bearer", accessToken: () => null }, odd).get("me").catch((failure: unknown) => failure);
    expect(other instanceof ApiError && other.isCatalogued).toBe(false);
  });
});
