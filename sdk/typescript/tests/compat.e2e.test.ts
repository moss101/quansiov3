/**
 * SDK compatibility against the current server (APP-013): `quansio-server` is started on a
 * scratch database and the generated operation table is checked against what it serves —
 * every operation id is in `GET /v1/commands`, every projection in
 * `GET /v1/read-projections` — then a session is obtained through the SDK from the magic
 * link on the server's console and a command and a projection go through it; a refused
 * command is a typed, catalogued error. Blocked, not skipped, without the dev stack or
 * the built binaries.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { createServer } from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { ApiError, GENERATED_OPERATIONS, QuansioClient, READ_PROJECTIONS } from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

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

describe.skipIf(blocked !== null)("SDK compatibility against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_sdk_ts_${String(process.pid)}`;
    const output = execFileSync(SEED_BIN, ["create", "--database", databaseName, "--tenant", "tn_01J8Z3K6F1N8VQ2X5W9Y0SDKTS", "--user", "usr_01J8Z3K6F1N8VQ2X5W9Y0SDKTS", "--workspace", "ws_01J8Z3K6F1N8VQ2X5W9Y0SDKTS", "--node", "wn_01J8Z3K6F1N8VQ2X5W9Y0SDKTS"], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL }, encoding: "utf8" });
    const databaseUrl = output.split("\n").find((line) => line.startsWith("database_url="))?.slice("database_url=".length);
    if (!databaseUrl) {
      throw new Error(`no database_url: ${output}`);
    }
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    const env: Record<string, string | undefined> = { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}`, QUANSIO_APPROVAL_SIGNING_KEY: "sdk-ts-e2e-key" };
    delete env["QUANSIO_TRUST_SCOPE_HEADERS"];
    child = spawn(SERVER_BIN, [], { env, stdio: ["ignore", "ignore", "pipe"] });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
      if (child.exitCode !== null) {
        throw new Error(`quansio-server exited: ${stderr.join("")}`);
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
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      execFileSync(SEED_BIN, ["drop", "--database", databaseName], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
    } catch {
      /* named by pid; a leftover is harmless */
    }
  }, 30_000);

  it("the generated operation table is what the current server serves, and a session works through the SDK", async () => {
    const anonymous = new QuansioClient(origin, { kind: "bearer", accessToken: () => null });
    const health = await anonymous.health();
    expect(health.status).toBe("ready");
    // Every operation the SDK knows is a command the server catalogs, and vice versa.
    const served = (await anonymous.get<{ groups: Record<string, string[]> }>("commands")).groups;
    const servedNames = Object.values(served).flat().sort();
    expect([...GENERATED_OPERATIONS].sort()).toEqual(servedNames);
    expect(health.commands).toBe(GENERATED_OPERATIONS.length);
    const projections = await anonymous.get<{ read_projections: string[] }>("read-projections");
    expect([...projections.read_projections].sort()).toEqual([...READ_PROJECTIONS].sort());
    expect(health.read_projections).toBe(READ_PROJECTIONS.length);
    // A session from the magic link on the console, through the SDK.
    const email = "sdk@example.com";
    await anonymous.startMagicLink(email, "SDK");
    const token = await waitFor(() => stderr.join("").split("\n").find((line) => line.includes(`magic-link for ${email}`))?.split("token=")[1]?.trim(), "the magic link");
    const session = await anonymous.completeMagicLink<{ access_token: string; tenant_id: string; workspace_id: string }>(token);
    const client = new QuansioClient(origin, { kind: "bearer", accessToken: () => session.access_token });
    const me = await client.me<{ user: { primary_email: string }; session: { tenant_id: string } }>();
    expect(me.user.primary_email).toBe(email);
    expect(me.session.tenant_id).toBe(session.tenant_id);
    // A command and a projection, and a refusal typed with a catalogued code.
    const posted = await client.command<{ run_id: string; thread_id: string }>("PostMessage", { workspace_id: session.workspace_id, content: "hello from the sdk" });
    expect(posted.run_id).toMatch(/^run_/);
    const messages = await client.get<{ messages: unknown[] }>(`messages?thread_id=${posted.thread_id}`);
    expect(messages.messages.length).toBeGreaterThan(0);
    const refused = await client.command("ApproveEffect", { approval_request_id: "apr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params_digest: "0".repeat(64) }).catch((error: unknown) => error);
    expect(refused).toBeInstanceOf(ApiError);
    if (refused instanceof ApiError) {
      expect(refused.isCatalogued).toBe(true);
    }
    const replay = await client.commandEnvelope("CreateWorkspace", { name: "sdk" }, "cmd_01J8Z3K6F1N8VQ2X5W9Y0SDKRP");
    const again = await client.commandEnvelope("CreateWorkspace", { name: "sdk" }, "cmd_01J8Z3K6F1N8VQ2X5W9Y0SDKRP");
    expect(replay.replayed).toBe(false);
    expect(again.replayed).toBe(true);
    expect(again.result).toEqual(replay.result);
  }, 60_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
