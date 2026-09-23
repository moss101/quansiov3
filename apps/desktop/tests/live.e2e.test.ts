/**
 * Live surface E2E (APP-007): takeover and reconnect against the real `quansio-server`
 * binary on a scratch PostgreSQL database seeded by `quansio-dev-seed --live` (one READY
 * target, a RUNNING run, the run's browser session, an open terminal with a durable
 * cursor, the target's computer-control row), driven through the shell's own bridge.
 *
 * What it proves at the boundary:
 * * takeover E2E — `RequestTakeover` moves the same session id to the user, the run parks
 *   (`WAITING_TAKEOVER`), the projection reports the fence and the reason, the workspace
 *   stream and the target-scoped live subscription both carry `browser.control_changed`
 *   for that id, and the tenant still has exactly one browser session (acceptance 1, 2);
 * * reconnect test — a new bridge over the same session file finds the same session, still
 *   user-held, and the terminal replay resumes from the durable cursor; the handback then
 *   returns the same session to the agent and resumes the run;
 * * a viewer is refused (`SCOPE_FORBIDDEN`) and a handback of what nobody holds is refused
 *   (`CONFLICT_STATE`) — both as data the page shows.
 *
 * Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN) and the workspace's
 * `target/debug/quansio-server` + `quansio-dev-seed`. Without them the suite skips with an
 * explicit `BLOCKED_EXTERNAL` marker (AGENTS.md "Real boundaries").
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { DesktopBridge, SessionFile, TerminalReplay, UpdateHooks, decodeLiveSession, decodeTargets, type SocketLike } from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0E2E07";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0E2E07";
const VIEWER = "usr_01J8Z3K6F1N8VQ2X5W9Y0E2E08";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0E2E07";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0E2E07";
// The fixed ids of `quansio-dev-seed --live` (LIVE_IDS in the seed).
const TARGET = "tgt_01J8Z3K6F1N8VQ2X5W9Y0E2TG1";
const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0E2RN1";
const SESSION = "bsn_01J8Z3K6F1N8VQ2X5W9Y0E2BS1";
const TERMINAL = "tsn_01J8Z3K6F1N8VQ2X5W9Y0E2TS1";
const TERMINAL_CURSOR = 128;

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", USER, "--workspace", WORKSPACE, "--node", NODE, "--live", "--member", `${VIEWER}:member:viewer`],
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

interface Pushed {
  readonly channel: string;
  readonly payload: unknown;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: Record<string, unknown>;
  readonly error?: { readonly code: string; readonly message: string };
}

function eventOf(pushed: Pushed, channel: string, type: string): Record<string, unknown> | undefined {
  if (pushed.channel !== channel) {
    return undefined;
  }
  const record = pushed.payload as { kind?: string; event?: Record<string, unknown> };
  return record.kind === "event" && record.event?.["type"] === type ? record.event : undefined;
}

describe.skipIf(blocked !== null)("desktop live surface E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_live_e2e_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}`, QUANSIO_TRUST_SCOPE_HEADERS: "1" },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-live-e2e-"));
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      dropScratchDatabase(databaseName);
    } catch {
      /* the scratch database is named by pid; a leftover is harmless */
    }
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  function bridgeFor(userId: string, consumer: string, pushed: Pushed[]): DesktopBridge {
    return new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId }, workspaceId: WORKSPACE },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer,
      push: (channel, payload) => pushed.push({ channel, payload }),
    });
  }

  it("takes over the same session, fences the agent, survives a reconnect, and hands back", async () => {
    const pushed: Pushed[] = [];
    const bridge = bridgeFor(USER, "desktop:live-e2e", pushed);
    const handlers = bridge.handlers();
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "workspace stream open");

    // 1. The target list names the one session the agent is driving; the live view agrees.
    const targets = decodeTargets(await handlers["live.targets"]({}));
    expect(targets.map((target) => target.id)).toEqual([TARGET]);
    expect(targets[0]?.browserSessions.map((entry) => entry.id)).toEqual([SESSION]);
    expect(targets[0]?.terminals[0]?.cursor).toBe(TERMINAL_CURSOR);
    const before = decodeLiveSession(await handlers["live.session"]({ sessionId: SESSION }));
    expect(before.sessionId).toBe(SESSION);
    expect(before.controller).toBe("agent");
    expect(before.status).toBe("active");
    expect(before.automation).toEqual({ paused: false, reason: null, agentInputFenced: false });
    expect(before.run?.id).toBe(RUN);
    expect(before.run?.status).toBe("RUNNING");
    expect(before.terminals[0]?.id).toBe(TERMINAL);
    expect(before.computerControl?.holder).toBe("agent");

    // 2. The target-scoped live subscription opens beside the workspace one.
    const attached = (await handlers["live.attach"]({ targetId: TARGET })) as { attached: boolean };
    expect(attached.attached).toBe(true);
    await waitFor(
      () => (pushed.some((p) => p.channel === "live.frame" && (p.payload as { kind?: string; state?: string }).kind === "state" && (p.payload as { state?: string }).state === "open") ? true : undefined),
      "target stream open",
    );

    // 3. A viewer may look but is refused a takeover — as data, with the §15 code.
    const viewerPushed: Pushed[] = [];
    const viewer = bridgeFor(VIEWER, "desktop:live-e2e-viewer", viewerPushed);
    const viewerHandlers = viewer.handlers();
    const seen = decodeLiveSession(await viewerHandlers["live.session"]({ sessionId: SESSION }));
    expect(seen.sessionId).toBe(SESSION);
    const refused = (await viewerHandlers["live.takeover"]({ sessionId: SESSION })) as SettledPayload;
    expect(refused.ok).toBe(false);
    expect(refused.error?.code).toBe("SCOPE_FORBIDDEN");
    viewer.close();

    // 4. The takeover: same id, user holds, run parked, fence reported; both streams carry it.
    const taken = (await handlers["live.takeover"]({ sessionId: SESSION })) as SettledPayload;
    expect(taken.ok, JSON.stringify(taken)).toBe(true);
    const takenSession = taken.result?.["session"] as Record<string, unknown>;
    expect(takenSession["id"]).toBe(SESSION);
    expect(takenSession["control_holder"]).toBe("user");
    expect(takenSession["status"]).toBe("paused_takeover");
    expect((taken.result?.["run"] as Record<string, unknown>)["status"]).toBe("WAITING_TAKEOVER");
    const during = decodeLiveSession(await handlers["live.session"]({ sessionId: SESSION }));
    expect(during.sessionId).toBe(SESSION);
    expect(during.controller).toBe("user");
    expect(during.automation).toEqual({ paused: true, reason: "user_control", agentInputFenced: true });
    expect(during.run?.status).toBe("WAITING_TAKEOVER");
    const onWorkspace = await waitFor(() => pushed.map((p) => eventOf(p, "stream.frame", "browser.control_changed")).find(Boolean), "control change on the workspace stream");
    expect((onWorkspace["payload"] as Record<string, unknown>)["session_id"]).toBe(SESSION);
    expect((onWorkspace["payload"] as Record<string, unknown>)["holder"]).toBe("user");
    const onTarget = await waitFor(() => pushed.map((p) => eventOf(p, "live.frame", "browser.control_changed")).find(Boolean), "control change on the target stream");
    expect((onTarget["payload"] as Record<string, unknown>)["session_id"]).toBe(SESSION);
    await waitFor(() => pushed.map((p) => eventOf(p, "stream.frame", "run.waiting")).find(Boolean), "run.waiting");
    // No hidden replacement: the tenant still has exactly the one session, under the same id.
    const afterTakeover = decodeTargets(await handlers["live.targets"]({}));
    expect(afterTakeover[0]?.browserSessions.map((entry) => `${entry.id}:${entry.controller}:${entry.status}`)).toEqual([`${SESSION}:user:paused_takeover`]);
    // Taking over what the user already holds is refused, not queued.
    const again = (await handlers["live.takeover"]({ sessionId: SESSION })) as SettledPayload;
    expect(again.ok).toBe(false);
    expect(again.error?.code).toBe("CONFLICT_STATE");

    // 5. Reconnect: the desktop "restarts" — a new bridge over the same session file, nothing
    //    else carried over — and finds the same session, still held, and the same cursor.
    const replay = new TerminalReplay(TERMINAL, during.terminals[0]?.cursor ?? 0);
    expect(replay.cursor).toBe(TERMINAL_CURSOR);
    bridge.close();
    pushed.length = 0;
    const resumedPushed: Pushed[] = [];
    const resumed = bridgeFor(USER, "desktop:live-e2e-resumed", resumedPushed);
    const resumedHandlers = resumed.handlers();
    await resumedHandlers["stream.reconnect"]({});
    await waitFor(() => (resumedPushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "workspace stream reopened");
    await resumedHandlers["live.attach"]({ targetId: TARGET });
    await waitFor(
      () => (resumedPushed.some((p) => p.channel === "live.frame" && (p.payload as { state?: string }).state === "open") ? true : undefined),
      "target stream reopened",
    );
    const restored = decodeLiveSession(await resumedHandlers["live.session"]({ sessionId: SESSION }));
    expect(restored.sessionId).toBe(SESSION);
    expect(restored.controller).toBe("user");
    expect(restored.automation.agentInputFenced).toBe(true);
    expect(restored.terminals[0]?.id).toBe(TERMINAL);
    expect(replay.resume(restored.terminals[0]?.cursor ?? 0)).toEqual({ from: TERMINAL_CURSOR, lost: 0 });

    // 6. The handback: same id back to the agent, run resumed, fence lifted; then refused again.
    const back = (await resumedHandlers["live.handback"]({ sessionId: SESSION })) as SettledPayload;
    expect(back.ok, JSON.stringify(back)).toBe(true);
    expect((back.result?.["session"] as Record<string, unknown>)["id"]).toBe(SESSION);
    expect((back.result?.["session"] as Record<string, unknown>)["control_holder"]).toBe("agent");
    expect((back.result?.["run"] as Record<string, unknown>)["status"]).toBe("RUNNING");
    const after = decodeLiveSession(await resumedHandlers["live.session"]({ sessionId: SESSION }));
    expect(after.sessionId).toBe(SESSION);
    expect(after.automation).toEqual({ paused: false, reason: null, agentInputFenced: false });
    expect(after.run?.status).toBe("RUNNING");
    await waitFor(() => resumedPushed.map((p) => eventOf(p, "stream.frame", "run.resumed")).find(Boolean), "run.resumed");
    // The cursor-less target subscription replays the target's history, so the takeover
    // arrives first and the handback after it: the last control change is the agent's.
    const handedBack = await waitFor(
      () =>
        resumedPushed
          .map((p) => eventOf(p, "live.frame", "browser.control_changed"))
          .filter((event): event is Record<string, unknown> => event !== undefined)
          .find((event) => (event["payload"] as Record<string, unknown>)["holder"] === "agent"),
      "handback on the target stream",
    );
    expect((handedBack["payload"] as Record<string, unknown>)["session_id"]).toBe(SESSION);
    const nobody = (await resumedHandlers["live.handback"]({ sessionId: SESSION })) as SettledPayload;
    expect(nobody.ok).toBe(false);
    expect(nobody.error?.code).toBe("CONFLICT_STATE");
    const finalTargets = decodeTargets(await resumedHandlers["live.targets"]({}));
    expect(finalTargets[0]?.browserSessions.map((entry) => entry.id)).toEqual([SESSION]);

    // 7. The native-computer form over the same target: the control row, its generation fence.
    const computer = (await resumedHandlers["live.takeover"]({ targetId: TARGET })) as SettledPayload;
    expect(computer.ok, JSON.stringify(computer)).toBe(true);
    expect((computer.result?.["computer_control"] as Record<string, unknown>)["holder"]).toBe("user");
    expect((computer.result?.["computer_control"] as Record<string, unknown>)["generation"]).toBe(2);
    const computerBack = (await resumedHandlers["live.handback"]({ targetId: TARGET })) as SettledPayload;
    expect(computerBack.ok).toBe(true);
    expect((computerBack.result?.["computer_control"] as Record<string, unknown>)["holder"]).toBe("agent");
    expect((computerBack.result?.["computer_control"] as Record<string, unknown>)["generation"]).toBe(3);
    const detached = (await resumedHandlers["live.detach"]({})) as { attached: boolean };
    expect(detached.attached).toBe(false);
    resumed.close();
  }, 60_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
