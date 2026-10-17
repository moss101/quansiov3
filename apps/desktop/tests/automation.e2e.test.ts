/**
 * Automation E2E (APP-010) against the real `quansio-server` binary with its background
 * loop running (`QUANSIO_SCHEDULER_INTERVAL_MS=200`), on a scratch PostgreSQL database seeded
 * by `quansio-dev-seed` (an owner and an editor), driven through the desktop bridge.
 *
 * * notification preference E2E — the editor's choice of channels for mentions decides what
 *   the server makes and delivers: in-app only makes a row and no native notification; in
 *   app + desktop + email makes the desktop bridge show a native notification and sends an
 *   email through the configured mail API (a local HTTP capture here, bearer token and
 *   all); the owner never receives the editor's notification events;
 * * schedule restart test — a one-second routine fires through the loop; the server process
 *   is killed and a new one started on the same database; the routine keeps firing, and no
 *   window and no run ever appears twice among its fires.
 *
 * Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN) and the workspace's
 * `target/debug/quansio-server` + `quansio-dev-seed`. Without them the suite skips with an
 * explicit `BLOCKED_EXTERNAL` marker (AGENTS.md "Real boundaries").
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer as createHttpServer, type Server } from "node:http";
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
  decodeNotifications,
  decodeRoutines,
  summarizeFires,
  type DesktopNotice,
  type SocketLike,
} from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0R0E2E";
const OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0R0E21";
const EDITOR = "usr_01J8Z3K6F1N8VQ2X5W9Y0R0E22";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0R0E2E";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0R0E2E";
const MAIL_TOKEN = "mail-api-test-token";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", OWNER, "--workspace", WORKSPACE, "--node", NODE, "--member", `${EDITOR}:member:editor`],
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
  const deadline = Date.now() + 120_000;
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

async function waitFor<T>(poll: () => Promise<T | undefined> | T | undefined, what: string, timeoutMs = 30_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await poll();
    if (value !== undefined) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

interface Pushed {
  readonly channel: string;
  readonly payload: unknown;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: Record<string, unknown>;
  readonly error?: { readonly code: string; readonly message: string };
}

interface Mail {
  readonly authorization: string | undefined;
  readonly body: { to: string; subject: string; text: string };
}

describe.skipIf(blocked !== null)("desktop automation E2E against quansio-server", () => {
  let databaseName: string;
  let databaseUrl: string;
  let child: ChildProcess;
  let origin: string;
  let mailServer: Server;
  let mailUrl: string;
  const mails: Mail[] = [];
  const dirs: string[] = [];
  const stderr: string[] = [];

  // The scratch tenant gets the shortest interval the schema allows (10 s) through the same
  // config/automation.yaml the server reads, as a tenant override; the plans' own minimums stay.
  function automationConfigFile(): string {
    const shipped = readFileSync(join(WORKSPACE_ROOT, "config", "automation.yaml"), "utf8");
    const replaced = shipped.replace(/^tenant_overrides: \{\}$/m, `tenant_overrides:\n  ${TENANT}:\n    min_interval_seconds: 10`);
    if (replaced === shipped) {
      throw new Error("config/automation.yaml no longer has an empty tenant_overrides line to extend");
    }
    const dir = mkdtempSync(join(tmpdir(), "quansio-auto-config-"));
    dirs.push(dir);
    const file = join(dir, "automation.yaml");
    writeFileSync(file, replaced);
    return file;
  }

  async function startServer(): Promise<void> {
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: {
        ...process.env,
        QUANSIO_DATABASE_URL: databaseUrl,
        QUANSIO_BIND: `127.0.0.1:${String(port)}`,
        QUANSIO_TRUST_SCOPE_HEADERS: "1",
        QUANSIO_SCHEDULER_INTERVAL_MS: "200",
        QUANSIO_AUTOMATION_CONFIG: automationConfigFile(),
        QUANSIO_EMAIL_ENDPOINT: mailUrl,
        QUANSIO_EMAIL_TOKEN: MAIL_TOKEN,
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
  }

  async function stopServer(): Promise<void> {
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill("SIGKILL");
    await exited;
  }

  beforeAll(async () => {
    mailServer = createHttpServer((request, response) => {
      let body = "";
      request.on("data", (chunk: Buffer) => {
        body += chunk.toString();
      });
      request.on("end", () => {
        mails.push({ authorization: request.headers.authorization, body: JSON.parse(body) as Mail["body"] });
        response.writeHead(202).end();
      });
    });
    await new Promise<void>((resolve) => mailServer.listen(0, "127.0.0.1", resolve));
    const address = mailServer.address();
    mailUrl = `http://127.0.0.1:${String(typeof address === "object" && address ? address.port : 0)}/mail`;
    databaseName = `quansio_desktop_auto_e2e_${String(process.pid)}`;
    databaseUrl = seedScratchDatabase(databaseName);
    await startServer();
  }, 150_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    mailServer.close();
    try {
      dropScratchDatabase(databaseName);
    } catch {
      /* the scratch database is named by pid; a leftover is harmless */
    }
    for (const dir of dirs) {
      rmSync(dir, { recursive: true, force: true });
    }
  }, 30_000);

  async function client(userId: string, consumer: string, notices: DesktopNotice[] = []): Promise<{ bridge: DesktopBridge; handlers: ReturnType<DesktopBridge["handlers"]>; pushed: Pushed[] }> {
    const dir = mkdtempSync(join(tmpdir(), "quansio-desktop-auto-"));
    dirs.push(dir);
    const pushed: Pushed[] = [];
    const bridge = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId }, workspaceId: WORKSPACE },
      session: new SessionFile(dir),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer,
      push: (channel, payload) => pushed.push({ channel, payload }),
      notifyNative: (notice) => notices.push(notice),
    });
    const handlers = bridge.handlers();
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), `${consumer} stream open`);
    return { bridge, handlers, pushed };
  }

  function notificationEvents(pushed: readonly Pushed[]): Record<string, unknown>[] {
    return pushed
      .filter((p) => p.channel === "stream.frame")
      .map((p) => p.payload as { kind?: string; event?: Record<string, unknown> })
      .filter((frame) => frame.kind === "event" && typeof frame.event?.["type"] === "string" && (frame.event["type"]).startsWith("notification."))
      .map((frame) => frame.event as Record<string, unknown>);
  }

  it("delivers a mention on exactly the channels the person chose", async () => {
    const owner = await client(OWNER, "desktop:auto-owner");
    const notices: DesktopNotice[] = [];
    const editor = await client(EDITOR, "desktop:auto-editor", notices);
    const created = (await owner.handlers["threads.create"]({ kind: "group", title: "launch", participants: [{ kind: "user", id: EDITOR, role: "member" }] })) as SettledPayload;
    expect(created.ok, JSON.stringify(created)).toBe(true);
    const thread = created.result?.["thread_id"] as string;
    const mention = () => owner.handlers["messages.post"]({ threadId: thread, content: "@editor look", mentions: [{ kind: "user", id: EDITOR }] });
    const list = async () => decodeNotifications(await editor.handlers["notifications.list"]({}));

    // In-app only: a row, no native notification, no mail.
    const pref = (await editor.handlers["notifications.preference"]({ kind: "mention", channels: ["in_app"] })) as SettledPayload;
    expect(pref.ok, JSON.stringify(pref)).toBe(true);
    await mention();
    const first = await waitFor(async () => (await list()).items.find((item) => item.kind === "mention"), "the in-app mention");
    expect(first.channels).toEqual(["in_app"]);
    expect(first.title).toBe("You were mentioned in launch");
    await waitFor(() => notificationEvents(editor.pushed).find((event) => event["type"] === "notification.created"), "notification.created on the editor's stream");
    expect(notices).toEqual([]);
    expect(mails).toEqual([]);

    // In-app, desktop and email.
    await editor.handlers["notifications.preference"]({ kind: "mention", channels: ["in_app", "desktop", "email"] });
    await mention();
    const notice = await waitFor(() => notices[0], "a native desktop notification");
    expect(notice.title).toBe("You were mentioned in launch");
    const mail = await waitFor(() => mails[0], "an email through the mail API");
    expect(mail.authorization).toBe(`Bearer ${MAIL_TOKEN}`);
    // The seed stores addresses the way sign-in normalizes them (lower case).
    expect(mail.body.to).toBe(`${EDITOR}@example.com`.toLowerCase());
    expect(mail.body.subject).toBe("You were mentioned in launch");
    const second = await waitFor(async () => {
      const items = (await list()).items.filter((item) => item.kind === "mention");
      return items.length === 2 && items[0]?.delivered.includes("email") ? items[0] : undefined;
    }, "email recorded as delivered");
    expect(second.channels).toEqual(["in_app", "desktop", "email"]);
    expect(second.delivered).toEqual(["in_app", "desktop", "email"]);

    // The owner receives none of the editor's notification events.
    expect(notificationEvents(owner.pushed)).toEqual([]);
    expect(decodeNotifications(await owner.handlers["notifications.list"]({})).items).toEqual([]);

    // Read.
    const before = (await list()).unread;
    const read = (await editor.handlers["notifications.read"]({ notificationId: first.id })) as SettledPayload;
    expect(read.ok, JSON.stringify(read)).toBe(true);
    expect((await list()).unread).toBe(before - 1);
    owner.bridge.close();
    editor.bridge.close();
  }, 90_000);

  it("keeps firing a routine across a server restart without firing a window twice", async () => {
    let owner = await client(OWNER, "desktop:auto-routine");
    const created = (await owner.handlers["routines.command"]({ name: "CreateRoutine", params: { title: "Heartbeat", prompt: "say hello", trigger: { kind: "interval", spec: "10s" }, absence_policy: "catch_up_once" } })) as SettledPayload;
    expect(created.ok, JSON.stringify(created)).toBe(true);
    const routineId = created.result?.["id"] as string;
    const fires = async () => decodeRoutines(await owner.handlers["routines.list"]({})).find((routine) => routine.id === routineId)?.fires ?? [];
    await waitFor(async () => ((await fires()).filter((fire) => fire.status === "fired").length >= 2 ? true : undefined), "two fires before the restart", 60_000);

    // Kill the server mid-flight and start a new process on the same database.
    owner.bridge.close();
    await stopServer();
    await startServer();
    owner = await client(OWNER, "desktop:auto-routine-2");
    const before = (await fires()).length;
    await waitFor(async () => ((await fires()).filter((fire) => fire.status === "fired").length >= before + 2 ? true : undefined), "two fires after the restart", 60_000);
    const now = (await owner.handlers["routines.command"]({ name: "TriggerRoutineNow", params: { routine_id: routineId } })) as SettledPayload;
    expect(now.ok, JSON.stringify(now)).toBe(true);
    expect(now.result?.["source"]).toBe("manual");
    const paused = (await owner.handlers["routines.command"]({ name: "PauseRoutine", params: { routine_id: routineId } })) as SettledPayload;
    expect(paused.ok, JSON.stringify(paused)).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 1500));
    const all = await fires();
    const summary = summarizeFires(all);
    expect(summary.duplicateWindows).toBe(0);
    expect(summary.duplicateRuns).toBe(0);
    expect(new Set(all.map((fire) => fire.fireKey)).size).toBe(all.length);
    expect(all.filter((fire) => fire.status === "fired").every((fire) => fire.runId !== null && fire.threadId !== null)).toBe(true);
    expect(all.some((fire) => fire.source === "manual" && fire.status === "fired")).toBe(true);
    // Every fire posted into the routine's one thread, and each fire's run answered there.
    const threads = new Set(all.map((fire) => fire.threadId).filter((thread) => thread !== null));
    expect(threads.size).toBe(1);
    owner.bridge.close();
  }, 120_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
