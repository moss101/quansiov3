import { describe, expect, it } from "vitest";

import { DEFAULT_SECRET_CANARY } from "../src/diagnostics/bundle.js";
import { FrameRing, mergeExport, newTraceparent, traceIdOf } from "../src/diagnostics/trace.js";
import { ServerClient } from "../src/client/server.js";

interface Seen {
  readonly url: string;
  readonly headers: Record<string, string>;
}

function recordingFetch(responses: readonly { status: number; body: unknown }[]): {
  seen: Seen[];
  fetchImpl: (input: string, init?: RequestInit) => Promise<Response>;
} {
  const seen: Seen[] = [];
  let index = 0;
  return {
    seen,
    fetchImpl: (input, init) => {
      seen.push({ url: input, headers: { ...(init?.headers as Record<string, string>) } });
      const next = responses[Math.min(index, responses.length - 1)] ?? { status: 200, body: {} };
      index += 1;
      return Promise.resolve(new Response(JSON.stringify(next.body), { status: next.status }));
    },
  };
}

describe("desktop trace context (OPS-003)", () => {
  it("starts a valid W3C trace per call and refuses malformed ones", () => {
    const first = newTraceparent();
    const second = newTraceparent();
    expect(traceIdOf(first)).toMatch(/^[0-9a-f]{32}$/);
    expect(traceIdOf(first)).not.toBe(traceIdOf(second));
    for (const bad of ["", "garbage", `01-${"a".repeat(32)}-${"b".repeat(16)}-01`, `00-${"0".repeat(32)}-${"b".repeat(16)}-01`, `00-${"A".repeat(32)}-${"b".repeat(16)}-01`]) {
      expect(traceIdOf(bad)).toBeNull();
    }
  });

  it("sends a traceparent with every command and records a redacted frame per call", async () => {
    const server = recordingFetch([{ status: 200, body: { command_id: "c", replayed: false, result: {} } }]);
    const client = new ServerClient({
      origin: "http://127.0.0.1:8080",
      credential: { kind: "trusted-headers", scope: { tenantId: "tn_1", userId: "usr_1" } },
      fetch: server.fetchImpl,
    });
    await client.invoke("PostMessage", "cmd_1", { workspace_id: "ws_1", content: `secret ${DEFAULT_SECRET_CANARY}` });
    await client.invoke("PostMessage", "cmd_2", { workspace_id: "ws_1", content: "again" });
    const sent = server.seen.map((call) => traceIdOf(call.headers.traceparent));
    expect(sent.every((id) => id !== null)).toBe(true);
    expect(sent[0]).not.toBe(sent[1]);
    const frames = client.diagnosticFrames();
    expect(frames.map((frame) => frame.traceId)).toEqual(sent);
    expect(frames[0]?.msg).toBe("POST /v1/commands/PostMessage -> 200");
    expect(JSON.stringify(frames)).not.toContain(DEFAULT_SECRET_CANARY);
  });

  it("keeps the same trace across an expired-session retry", async () => {
    const server = recordingFetch([
      { status: 401, body: { code: "AUTH_EXPIRED", message: "expired", correlation_id: "c", retryable: true } },
      { status: 200, body: { command_id: "c", replayed: false, result: {} } },
    ]);
    const client = new ServerClient({
      origin: "http://127.0.0.1:8080",
      credential: { kind: "session", accessToken: () => "old", refresh: () => Promise.resolve("new") },
      fetch: server.fetchImpl,
    });
    await client.invoke("PostMessage", "cmd_1", { workspace_id: "ws_1", content: "x" });
    expect(server.seen).toHaveLength(2);
    expect(server.seen[0]?.headers.traceparent).toBe(server.seen[1]?.headers.traceparent);
  });

  it("exports the server bundle with the desktop frames, redacted and bounded", async () => {
    const bundle = { tenant_id: "tn_1", logs: [{ event: "span", note: DEFAULT_SECRET_CANARY }], truncated: false };
    const server = recordingFetch([
      { status: 200, body: { command_id: "c", replayed: false, result: {} } },
      { status: 200, body: bundle },
    ]);
    const client = new ServerClient({
      origin: "http://127.0.0.1:8080",
      credential: { kind: "trusted-headers", scope: { tenantId: "tn_1", userId: "usr_1" } },
      fetch: server.fetchImpl,
    });
    await client.invoke("PostMessage", "cmd_1", { workspace_id: "ws_1", content: "x" });
    const exported = await client.exportDiagnostics();
    expect(server.seen[1]?.url).toBe("http://127.0.0.1:8080/v1/diagnostics/bundle");
    expect((exported.server as { tenant_id: string }).tenant_id).toBe("tn_1");
    expect(exported.desktop.length).toBeGreaterThanOrEqual(1);
    expect(JSON.stringify(exported)).not.toContain(DEFAULT_SECRET_CANARY);

    // A member cannot read the server bundle: the refusal is recorded, the desktop part remains.
    const refusing = recordingFetch([
      { status: 403, body: { code: "SCOPE_FORBIDDEN", message: "no", correlation_id: "c", retryable: false } },
    ]);
    const member = new ServerClient({
      origin: "http://127.0.0.1:8080",
      credential: { kind: "trusted-headers", scope: { tenantId: "tn_1", userId: "usr_2" } },
      fetch: refusing.fetchImpl,
    });
    const partial = await member.exportDiagnostics();
    expect(partial.server).toEqual({ unavailable: "SCOPE_FORBIDDEN" });

    const ring = new FrameRing(500);
    for (let index = 0; index < 400; index += 1) {
      ring.record("a".repeat(32), "GET", `http://h/v1/runs/run_${String(index)}?token=${DEFAULT_SECRET_CANARY}`, 200);
    }
    const bounded = mergeExport({}, ring.frames(), 4_000);
    expect(bounded.truncated).toBe(true);
    expect(JSON.stringify(bounded).length).toBeLessThanOrEqual(4_200);
    expect(JSON.stringify(bounded)).not.toContain("token=");
  });
});
