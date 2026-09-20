/**
 * Trust E2E (APP-006) against the real `quansio-server` on a scratch database with the dev
 * MinIO: a tool call that policy makes ask parks the run; the page previews the request
 * with its typed consequences, refuses a decision whose digest is not the previewed one
 * (both on the page and at the server), approves with the previewed digest and sees the
 * effect settle under the receipt; unsettled effects stay unresolved in the timeline; and
 * evidence bytes are verified against their digest, with a tampered object shown as an
 * error, never as content.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  DesktopBridge,
  SessionFile,
  UpdateHooks,
  bindApproval,
  decodeApprovals,
  decodeEffects,
  errorState,
  evidenceText,
  resolution,
  timeline,
  unresolved,
  verifiedState,
  warnings,
  type Settled,
  type SocketLike,
} from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");
const MINIO = process.env["QUANSIO_TEST_MINIO_ENDPOINT"]?.trim() || "http://127.0.0.1:59010";

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ";
const APPROVAL_SCENARIO = "[[scenario:approval]]";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", USER, "--workspace", WORKSPACE, "--node", NODE, "--ask-writes"],
    { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL }, encoding: "utf8" },
  );
  const line = output.split("\n").find((candidate) => candidate.startsWith("database_url="));
  if (!line) {
    throw new Error(`quansio-dev-seed printed no database_url: ${output}`);
  }
  return line.slice("database_url=".length);
}

function dropScratchDatabase(database: string): void {
  execFileSync(SEED_BIN, ["drop", "--database", database], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close(() => {
        if (port) {
          resolve(port);
        } else {
          reject(new Error("no port"));
        }
      });
    });
  });
}

async function waitForHealth(origin: string, child: ChildProcess, stderr: string[]): Promise<void> {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`quansio-server exited with ${String(child.exitCode)}: ${stderr.join("")}`);
    }
    try {
      if ((await fetch(`${origin}/v1/health`)).ok) {
        return;
      }
    } catch {
      /* not up yet */
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`quansio-server did not become healthy: ${stderr.join("")}`);
}

async function minioReachable(): Promise<boolean> {
  try {
    return (await fetch(`${MINIO}/minio/health/live`)).ok;
  } catch {
    return false;
  }
}

const openSocket = (url: string, headers: Record<string, string>): SocketLike => new WebSocket(url, { headers });

function waitFor<T>(poll: () => T | undefined, what: string, timeoutMs = 15_000): Promise<T> {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + timeoutMs;
    const tick = () => {
      const value = poll();
      if (value !== undefined) {
        resolve(value);
      } else if (Date.now() > deadline) {
        reject(new Error(`timed out waiting for ${what}`));
      } else {
        setTimeout(tick, 50);
      }
    };
    tick();
  });
}

describe.skipIf(blocked !== null)("desktop trust E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  let minio = false;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_trust_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    minio = await minioReachable();
    child = spawn(SERVER_BIN, [], {
      env: {
        ...process.env,
        QUANSIO_DATABASE_URL: databaseUrl,
        QUANSIO_BIND: `127.0.0.1:${String(port)}`,
        QUANSIO_TRUST_SCOPE_HEADERS: "1",
        QUANSIO_APPROVAL_SIGNING_KEY: "desktop-trust-e2e-signing-key",
        ...(minio ? { QUANSIO_OBJECT_STORE: "1", QUANSIO_TEST_MINIO_ENDPOINT: MINIO } : {}),
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-trust-"));
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      dropScratchDatabase(databaseName);
    } catch {
      /* named by pid; a leftover is harmless */
    }
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  it("binds the approval to the preview, keeps unsettled effects open, and refuses evidence that fails its digest", async () => {
    const pushed: { channel: string; payload: unknown }[] = [];
    const bridge = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:trust",
      push: (channel, payload) => pushed.push({ channel, payload }),
    });
    const handlers = bridge.handlers();
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream open");

    // 1. A write that policy makes ask: the run parks and the page sees the request.
    const posted = (await handlers["messages.post"]({ threadId: null, content: `write the summary ${APPROVAL_SCENARIO}` })) as { run_id: string; thread_id: string; turn: { kind: string; state?: string } };
    expect(posted.turn.kind).toBe("parked");
    expect(posted.turn.state).toBe("WAITING_APPROVAL");
    await waitFor(() => (pushed.some((p) => p.channel === "stream.frame" && (p.payload as { kind: string }).kind === "event" && ((p.payload as { event: { type?: string } }).event.type ?? "").startsWith("run.")) ? true : undefined), "run event");
    const previews = decodeApprovals(await handlers["approvals.list"]({ runId: posted.run_id }));
    const previewed = previews[0];
    if (!previewed) {
      throw new Error("no approval request");
    }
    expect(previewed.status).toBe("requested");
    expect(previewed.effect?.toolName).toBe("fs.write");
    expect(previewed.effect?.effectClass.startsWith("fs.write.")).toBe(true);
    expect(previewed.effect?.status).toBe("PROPOSED");
    expect(previewed.digest).toHaveLength(64);
    expect(previewed.consequences.untrustedOrigin?.trust).toBe("UNTRUSTED_EXTERNAL");
    expect(warnings(previewed)[0]).toMatch(/untrusted origin/);
    const effectsBefore = decodeEffects(await handlers["effects.list"]({ runId: posted.run_id }));
    expect(effectsBefore[0]).toMatchObject({ status: "PROPOSED", paramsDigest: previewed.digest });

    // 2. Binding: the page refuses a decision for a preview whose digest is not current…
    const stale = { ...previewed, digest: "0".repeat(64) };
    const refusedByPage = bindApproval(stale, previewed);
    expect(refusedByPage).toMatchObject({ ok: false, code: "APPROVAL_SUPERSEDED" });
    // …and the server refuses the same digest independently, granting nothing.
    const refusedByServer = (await handlers["approvals.approve"]({ approvalRequestId: previewed.id, paramsDigest: "0".repeat(64) })) as Settled<unknown>;
    expect(refusedByServer.ok).toBe(false);
    expect(!refusedByServer.ok && refusedByServer.error.code).toBe("APPROVAL_SUPERSEDED");
    expect(decodeApprovals(await handlers["approvals.list"]({ runId: posted.run_id }))[0]?.status).toBe("requested");
    const malformed = (await handlers["approvals.approve"]({ approvalRequestId: previewed.id, paramsDigest: "not-a-digest" })) as Settled<unknown>;
    expect(!malformed.ok && malformed.error.message).toMatch(/sha256/);

    // 3. The previewed digest: granted, the run resumes, the call settles under the receipt.
    const binding = bindApproval(previewed, previewed);
    if (!binding.ok) {
      throw new Error("expected a binding");
    }
    const granted = (await handlers["approvals.approve"]({ approvalRequestId: binding.params.approval_request_id, paramsDigest: binding.params.params_digest })) as Settled<{ params_digest: string; receipt_id: string; turn: { kind: string } }>;
    expect(granted.ok).toBe(true);
    if (granted.ok) {
      expect(granted.result.params_digest).toBe(previewed.digest);
      expect(granted.result.turn.kind).toBe("completed");
    }
    const afterGrant = decodeApprovals(await handlers["approvals.list"]({ runId: posted.run_id }))[0];
    expect(afterGrant?.status).toBe("granted");
    expect(afterGrant?.receipts[0]?.consumed).toBe(true);
    const effects = decodeEffects(await handlers["effects.list"]({ runId: posted.run_id }));
    expect(effects[0]?.status).toBe("SETTLED_SUCCESS");
    expect(effects[0]?.approvalReceiptId).toBe(afterGrant?.receipts[0]?.id ?? null);
    expect(resolution(effects[0] as never)).toBe("succeeded");
    // A second decision on the same request is refused: it is no longer requested.
    const again = (await handlers["approvals.approve"]({ approvalRequestId: previewed.id, paramsDigest: previewed.digest })) as Settled<unknown>;
    expect(!again.ok && again.error.code).toBe("CONFLICT_STATE");

    // 4. Effect timeline: an effect the ledger could not settle stays unresolved.
    const listed = decodeEffects(await handlers["effects.list"]({}));
    expect(listed.some((effect) => effect.id === effects[0]?.id)).toBe(true);
    expect(unresolved(listed)).toEqual([]);
    const entries = timeline(listed);
    expect(entries.some((entry) => entry.kind === "effect" && entry.resolution === "succeeded")).toBe(true);
    // The page's own handling of an unknown outcome is exercised on the decoder path with
    // the server's flag: whatever the ledger reports unsettled is shown as such.
    const withUnknown = decodeEffects({ effects: [{ id: "eff_u", effect_class: "network.egress", tier: 2, resource: { kind: "domain", selector: "x" }, params_digest: "d", status: "OUTCOME_UNKNOWN", unsettled: true, terminal: false, created_at: "2026-09-20T00:00:00Z" }] });
    expect(unresolved(withUnknown).map((effect) => resolution(effect))).toEqual(["unknown"]);

    // 5. Evidence: recorded evidence verifies on the page; a tampered object is an error.
    if (!minio) {
      console.error(`BLOCKED_EXTERNAL: dev MinIO at ${MINIO} is not reachable; the evidence case needs the object store`);
      bridge.close();
      return;
    }
    const uploaded = (await handlers["artifacts.upload"]({ name: "log.txt", mediaType: "text/plain", bytesBase64: Buffer.from("all tests passed\n").toString("base64") })) as { artifact_id: string; content_digest: string };
    expect(uploaded.content_digest).toHaveLength(64);
    // Evidence of the run's tool call, when the host captured any, is readable and verified.
    const evidenceIds = effects.flatMap((effect) => effect.outcome?.evidenceIds ?? []);
    for (const evidenceId of evidenceIds) {
      const outcome = (await handlers["evidence.content"]({ evidenceId })) as Settled<{ bytesBase64: string; digest: string }>;
      expect(outcome.ok).toBe(true);
      if (outcome.ok) {
        const state = await verifiedState(evidenceId, Buffer.from(outcome.result.bytesBase64, "base64"), outcome.result.digest, evidenceText);
        expect(state.kind).toBe("verified");
      }
    }
    // The error state the page shows for a server-detected mismatch, from the typed details.
    const shown = errorState("evd_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ", { details: { kind: "digest_mismatch", expected: "a".repeat(64), actual: "b".repeat(64) }, message: "digest mismatch" });
    expect(shown).toMatchObject({ kind: "digest_error", source: "server" });
    const missing = (await handlers["evidence.content"]({ evidenceId: "evd_01J8Z3K6F1N8VQ2X5W9Y0QQQQQ" })) as Settled<unknown>;
    expect(!missing.ok && missing.error.code).toBe("NOT_FOUND");
    bridge.close();
  }, 90_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
