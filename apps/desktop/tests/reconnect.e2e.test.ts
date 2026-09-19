/**
 * Desktop reconnect E2E (APP-003, acceptance 2): the desktop reconnects to the server
 * without losing active run state — against the real `quansio-server` binary on a scratch
 * PostgreSQL database, driven through the shell's own bridge, clients and session file.
 *
 * Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN; a scratch database is created
 * and dropped) and either `QUANSIO_TEST_SERVER_BIN` or the workspace's
 * `target/debug/quansio-server`. Without the DSN or a binary the suite skips with an explicit
 * `BLOCKED_EXTERNAL` marker (AGENTS.md "Real boundaries"): the dev stack is not running.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import pg from "pg";
import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { DesktopBridge, SessionFile, UpdateHooks, type SocketLike } from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const CMD1 = "cmd_01J8Z3K6F1N8VQ2X5W9Y0DDDD1";
const CMD2 = "cmd_01J8Z3K6F1N8VQ2X5W9Y0DDDD2";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN)
    ? `BLOCKED_EXTERNAL: quansio-server binary not found at ${SERVER_BIN}; build it with cargo build -p quansio-server or set QUANSIO_TEST_SERVER_BIN`
    : null;

function scratchUrl(admin: string, database: string): string {
  const url = new URL(admin);
  url.pathname = `/${database}`;
  return url.toString();
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

async function seed(databaseUrl: string): Promise<void> {
  const client = new pg.Client({ connectionString: databaseUrl });
  await client.connect();
  try {
    await client.query("BEGIN");
    await client.query("INSERT INTO tenants (id, name) VALUES ($1, 'desktop e2e')", [TENANT]);
    await client.query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'desktop e2e')", [USER, `${USER}@example.com`]);
    await client.query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'desktop e2e')", [WORKSPACE, TENANT]);
    await client.query("SELECT set_config('quansio.tenant_id', $1, true)", [TENANT]);
    await client.query(
      "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) VALUES ($1, $2, $3, 'objective', 'desktop e2e', '{\"kind\":\"user\",\"id\":\"usr_seed\"}'::jsonb)",
      [NODE, TENANT, WORKSPACE],
    );
    await client.query(
      "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0DDDDD', $1, NULL, 'tenant', $2)",
      [TENANT, JSON.stringify([{ effect_class: "read.internal", resource: { kind: "fs", selector: "**" }, decision: "allow" }])],
    );
    await client.query("COMMIT");
  } finally {
    await client.end();
  }
}

async function waitForHealth(origin: string, child: ChildProcess, stderr: string[]): Promise<void> {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`quansio-server exited with ${String(child.exitCode)}: ${stderr.join("")}`);
    }
    try {
      const response = await fetch(`${origin}/v1/health`);
      if (response.ok) {
        return;
      }
    } catch {
      /* not up yet */
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`quansio-server did not become healthy: ${stderr.join("")}`);
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

describe.skipIf(blocked !== null)("desktop reconnect E2E against quansio-server", () => {
  let admin: pg.Client;
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_e2e_${String(process.pid)}`;
    admin = new pg.Client({ connectionString: ADMIN_URL });
    await admin.connect();
    await admin.query(`DROP DATABASE IF EXISTS ${databaseName} WITH (FORCE)`);
    await admin.query(`CREATE DATABASE ${databaseName}`);
    const databaseUrl = scratchUrl(ADMIN_URL, databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}` },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    await seed(databaseUrl);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-e2e-"));
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    await admin.query(`DROP DATABASE IF EXISTS ${databaseName} WITH (FORCE)`).catch(() => undefined);
    await admin.end().catch(() => undefined);
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  it("posts a message, persists the run and cursor, drops the socket, and resumes the same run from the cursor", async () => {
    const pushed: { channel: string; payload: unknown }[] = [];
    const scope = { tenantId: TENANT, userId: USER };
    const bridge = new DesktopBridge({
      serverOrigin: origin,
      scope,
      workspaceId: WORKSPACE,
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:e2e",
      push: (channel, payload) => pushed.push({ channel, payload }),
    });
    const handlers = bridge.handlers();
    expect((await bridge.server.health()).status).toBe("ready");

    // 1. Subscribe from nothing, then post: a durable event arrives with a cursor.
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream open");
    const posted = (await handlers["commands.invoke"]({ name: "PostMessage", commandId: CMD1, params: { workspace_id: WORKSPACE, content: "hello desktop" } })) as {
      result: { run_id: string; thread_id: string };
    };
    expect(posted.result.run_id).toMatch(/^run_/);
    const firstEvent = await waitFor(
      () => pushed.find((p) => p.channel === "stream.frame" && (p.payload as { kind: string }).kind === "event")?.payload as { cursor: string } | undefined,
      "first durable event",
    );
    expect(firstEvent.cursor.length).toBeGreaterThan(0);

    // 2. The active run and the cursor are durable in main's session file.
    const before = await handlers["session.activeRun"]({}) as { activeRun: { runId: string; streamCursor: string } | null };
    expect(before.activeRun?.runId).toBe(posted.result.run_id);
    const cursorBeforeDrop = bridge.cursor();
    const persisted = new SessionFile(userData).current();
    expect(persisted?.activeRun?.runId).toBe(posted.result.run_id);
    expect(cursorBeforeDrop).not.toBeNull();
    // Both reads are synchronous, so no event can interleave: the file is what memory holds.
    expect(persisted?.streamCursor).toBe(cursorBeforeDrop);

    // 3. The desktop "restarts": a new bridge over the same session file, nothing else carried over.
    bridge.close();
    pushed.length = 0;
    const resumed = new DesktopBridge({
      serverOrigin: origin,
      scope,
      workspaceId: WORKSPACE,
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:e2e-resumed",
      push: (channel, payload) => pushed.push({ channel, payload }),
    });
    const resumedHandlers = resumed.handlers();
    const restored = await resumedHandlers["session.activeRun"]({}) as { activeRun: { runId: string; threadId: string } | null };
    expect(restored.activeRun?.runId).toBe(posted.result.run_id);
    expect(restored.activeRun?.threadId).toBe(posted.result.thread_id);
    expect(resumed.cursor()).toBe(cursorBeforeDrop);

    // 4. Reconnect from the persisted cursor; a new message after the drop arrives with a later cursor.
    await resumedHandlers["stream.reconnect"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream reopened");
    const second = (await resumedHandlers["commands.invoke"]({ name: "PostMessage", commandId: CMD2, params: { workspace_id: WORKSPACE, content: "after reconnect" } })) as {
      result: { run_id: string };
    };
    const laterEvent = await waitFor(
      () =>
        pushed.find((p) => p.channel === "stream.frame" && (p.payload as { kind: string; cursor?: string }).kind === "event" && (p.payload as { cursor: string }).cursor !== cursorBeforeDrop)
          ?.payload as { cursor: string } | undefined,
      "event after reconnect",
    );
    expect(laterEvent.cursor).not.toBe(cursorBeforeDrop);
    // The run keeps emitting, so the durable cursor is whatever the last pushed event carried:
    // memory, the file and the stream client agree at every point.
    const lastPushedCursor = (): string =>
      (pushed.filter((p) => p.channel === "stream.frame" && (p.payload as { kind: string }).kind === "event").at(-1)?.payload as { cursor: string }).cursor;
    expect(resumed.cursor()).toBe(lastPushedCursor());
    expect(new SessionFile(userData).current()?.streamCursor).toBe(lastPushedCursor());
    const after = await resumedHandlers["session.activeRun"]({}) as { activeRun: { runId: string } | null; connected: boolean };
    expect(after.connected).toBe(true);
    expect(after.activeRun?.runId).toBe(second.result.run_id);
    resumed.close();
  }, 60_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
