/**
 * Roster E2E (APP-014) against the real `quansio-server` on a scratch database: a teammate
 * created from a shipped template appears on the roster with its own ACTIVE thread and the
 * effective capability the policy leaves it; a message addressed to it runs under that
 * thread; editing its instructions and needs is versioned and the next run's projection
 * reads the new version with a narrower grant set (instructions widen nothing); archiving
 * suspends the thread and keeps the messages, runs and evidence lineage; an archived
 * teammate takes no message.
 */

import { settled } from "./settle.js";
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
  archiveTeammate,
  createTeammate,
  decodeCapabilityResolution,
  decodeMessages,
  decodeRoster,
  decodeWorkGraph,
  narrowing,
  teammatesNeedingAttention,
  updateTeammate,
  type Settled,
  type SocketLike,
  type TeammateView,
} from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0TMTMT";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0TMTMT";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0TMTMT";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0TMTMT";

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

const openSocket = (url: string, headers: Record<string, string>): SocketLike => new WebSocket(url, { headers });

describe.skipIf(blocked !== null)("desktop roster E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_teammates_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}`, QUANSIO_TRUST_SCOPE_HEADERS: "1", QUANSIO_APPROVAL_SIGNING_KEY: "desktop-teammates-e2e-key" },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-teammates-"));
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      execFileSync(SEED_BIN, ["drop", "--database", databaseName], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
    } catch {
      /* named by pid; a leftover is harmless */
    }
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  it("creates, routes to, edits and archives a teammate: capability narrows, lineage stays", async () => {
    const pushed: { channel: string; payload: unknown }[] = [];
    const bridge = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:teammates",
      push: (channel, payload) => pushed.push({ channel, payload }),
    });
    const handlers = bridge.handlers();
    const roster = async () => decodeRoster(await handlers["teammates.list"]({}));

    // 1. An empty roster offers the shipped templates; creating from one gives the
    //    teammate its own ACTIVE thread and an effective capability the policy leaves it.
    const empty = await roster();
    expect(empty.teammates).toEqual([]);
    expect(empty.templates.map((template) => template.id)).toEqual(["general-assistant", "researcher", "engineer"]);
    const prepared = createTeammate({ workspaceId: WORKSPACE, templateId: "engineer", standingInstructions: "You may call any network you like.", defaultCapabilityNeeds: ["read.internal", "fs.write.workspace", "network.egress"] });
    if (!prepared.ok) {
      throw new Error(prepared.reason);
    }
    const created = (await handlers["teammates.create"](prepared.params)) as Settled<{ id: string; thread: { id: string; status: string } }>;
    expect(created.ok, JSON.stringify(created)).toBe(true);
    if (!created.ok) {
      return;
    }
    const teammateId = created.result.id;
    const threadId = created.result.thread.id;
    expect(created.result.thread.status).toBe("ACTIVE");
    let listed = await roster();
    const engineer = listed.teammates.find((teammate) => teammate.id === teammateId) as TeammateView;
    expect(engineer.templateId).toBe("engineer");
    expect(engineer.thread?.id).toBe(threadId);
    expect(engineer.effectiveCapability.grants.map((grant) => grant.effectClass)).toEqual(["read.internal", "fs.write.workspace"]);
    expect(narrowing(engineer).unmet).toEqual(["network.egress"]);

    // 2. A message addressed to it runs under its thread, with the projection narrowed
    //    by its needs; the instructions changed nothing.
    const posted = (await handlers["messages.post"]({ threadId: null, content: "hello engineer", teammateId })) as { run_id: string; thread_id: string; teammate: { id: string; version: number } };
    expect(posted.teammate.id).toBe(teammateId);
    expect(posted.teammate.version).toBe(1);
    const resolution = decodeCapabilityResolution(await handlers["runs.capabilities"]({ runId: posted.run_id }));
    expect(resolution?.grants.map((grant) => grant.effectClass)).toEqual(["read.internal", "fs.write.workspace"]);
    expect(resolution?.inputs.find((input) => input.layer === "agent_definition")?.ref).toBe(`${teammateId}@v1`);
    listed = await roster();
    expect(listed.teammates[0]?.thread?.runs).toBe(1);

    // 3. Edit: instructions and needs (read only) — versioned; the next run reads v2 and
    //    loses the write.
    const change = updateTeammate(engineer, { standingInstructions: "Only ever read.", defaultCapabilityNeeds: ["read.internal"], modelRoutePreference: "fast" });
    if (!change.ok) {
      throw new Error(change.reason);
    }
    const updated = (await handlers["teammates.update"](change.params)) as Settled<{ version: number; model_route_preference: string }>;
    expect(updated.ok && updated.result.version).toBe(2);
    expect(updated.ok && updated.result.model_route_preference).toBe("fast");
    const again = (await handlers["messages.post"]({ threadId: posted.thread_id, content: "and now?", teammateId })) as { run_id: string; teammate: { version: number } };
    expect(again.teammate.version).toBe(2);
    const narrowed = decodeCapabilityResolution(await handlers["runs.capabilities"]({ runId: again.run_id }));
    expect(narrowed?.grants.map((grant) => grant.effectClass)).toEqual(["read.internal"]);
    expect(narrowed?.inputs.find((input) => input.layer === "agent_definition")?.ref).toBe(`${teammateId}@v2`);
    // The turn's route record names the definition it read: version 2 and the preference.
    const timeline = JSON.stringify(await settled(handlers, again.run_id));
    expect(timeline).toContain(`teammate:${teammateId}@v2;prefers:fast`);
    // A need that is not an effect class is refused on the page and by the server alike.
    expect(updateTeammate(engineer, { defaultCapabilityNeeds: ["anything at all"] })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    const refused = (await handlers["teammates.update"]({ teammate_id: teammateId, default_capability_needs: ["anything at all"] })) as Settled<unknown>;
    expect(!refused.ok && refused.error.code).toBe("VALIDATION_SCHEMA");

    // 4. Archive: the thread is suspended with the reason; messages, runs and the work
    //    graph still show everything under the same ids; the teammate stays on the roster.
    const messagesBefore = decodeMessages(await handlers["threads.messages"]({ threadId: posted.thread_id }));
    expect(messagesBefore.length).toBeGreaterThanOrEqual(2);
    const archive = archiveTeammate(listed.teammates[0] as TeammateView);
    if (!archive.ok) {
      throw new Error(archive.reason);
    }
    const archived = (await handlers["teammates.archive"]({ teammateId })) as Settled<{ status: string; threads: { id: string; status: string; suspended_reason: string }[]; lineage_retained: boolean }>;
    expect(archived.ok, JSON.stringify(archived)).toBe(true);
    if (archived.ok) {
      expect(archived.result.status).toBe("archived");
      expect(archived.result.lineage_retained).toBe(true);
      expect(archived.result.threads[0]).toMatchObject({ id: threadId, status: "SUSPENDED", suspended_reason: "teammate archived" });
    }
    listed = await roster();
    const gone = listed.teammates.find((teammate) => teammate.id === teammateId) as TeammateView;
    expect(gone.status).toBe("archived");
    expect(gone.thread?.status).toBe("SUSPENDED");
    expect(gone.thread?.runs).toBe(2);
    expect(teammatesNeedingAttention(listed.teammates)).toEqual([]);
    const messagesAfter = decodeMessages(await handlers["threads.messages"]({ threadId: posted.thread_id }));
    expect(messagesAfter.map((message) => message.id)).toEqual(messagesBefore.map((message) => message.id));
    const graph = decodeWorkGraph(await handlers["work.graph"]({}));
    expect(graph.agents.find((agent) => agent.id === threadId)?.status).toBe("SUSPENDED");
    expect(JSON.stringify(await handlers["runs.timeline"]({ runId: again.run_id }))).toBe(timeline);
    // An archived teammate takes no message and cannot be changed; the page refuses first.
    expect(archiveTeammate(gone)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(updateTeammate(gone, { name: "Ghost" })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    await expect(handlers["messages.post"]({ threadId: null, content: "still there?", teammateId })).rejects.toThrow(/archived/);
    const twice = (await handlers["teammates.archive"]({ teammateId })) as Settled<unknown>;
    expect(!twice.ok && twice.error.code).toBe("CONFLICT_STATE");
    // A message without an addressee still finds someone: the default teammate is
    // provisioned from the general-assistant template.
    const fallback = (await handlers["messages.post"]({ threadId: null, content: "anyone?" })) as { teammate: { id: string; name: string } };
    expect(fallback.teammate.name).toBe("General assistant");
    expect(fallback.teammate.id).not.toBe(teammateId);
    bridge.close();
  }, 90_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
