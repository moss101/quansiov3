/**
 * Plan / WorkGraph E2E (APP-005) against the real `quansio-server` on a scratch database:
 * the graph view is the canonical revision (graph projection E2E), two pages editing the
 * same revision produce a conflict the second rebases rather than an overwrite (revision
 * conflict test), and a delegation shows up as a handoff with the run parked (agent
 * handoff UI test). The page is the bridge's handlers plus the work reducer and the edit
 * draft the renderer uses.
 */

import { eventually } from "./settle.js";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  ApiError,
  DesktopBridge,
  SessionFile,
  UpdateHooks,
  decodeWorkGraph,
  initialWorkState,
  matchesCanonical,
  needsTwoProposals,
  newDraft,
  proposal,
  rebase,
  resolveLocalIds,
  withIntent,
  workReducer,
  type EditDraft,
  type SocketLike,
  type StreamFrame,
  type WorkGraphView,
  type WorkState,
} from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0NNNNN";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0NNNNN";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0NNNNN";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0NNNNN";
const DELEGATE_SCENARIO = "[[scenario:delegate]]";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", USER, "--workspace", WORKSPACE, "--node", NODE],
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

/** A page: the bridge's handlers plus the work reducer fed with everything main pushed. */
class Page {
  readonly pushed: { channel: string; payload: unknown }[] = [];
  readonly bridge: DesktopBridge;
  readonly handlers: ReturnType<DesktopBridge["handlers"]>;
  state: WorkState;
  #fed = 0;

  constructor(origin: string, userData: string, consumer: string) {
    this.bridge = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer,
      push: (channel, payload) => this.pushed.push({ channel, payload }),
    });
    this.handlers = this.bridge.handlers();
    this.state = initialWorkState(WORKSPACE);
  }

  async open(): Promise<void> {
    await this.handlers["stream.subscribe"]({});
    await waitFor(() => (this.pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream open");
  }

  feed(): void {
    for (; this.#fed < this.pushed.length; this.#fed += 1) {
      const entry = this.pushed[this.#fed];
      if (entry?.channel === "stream.frame") {
        this.state = workReducer(this.state, { type: "frame", frame: entry.payload as StreamFrame });
      }
    }
  }

  async load(): Promise<WorkGraphView> {
    const graph = decodeWorkGraph(await this.handlers["work.graph"]({}));
    this.state = workReducer(this.state, { type: "graph.loaded", graph });
    return graph;
  }

  /** What the renderer's submit does: nodes first when edges name new nodes. */
  async propose(draft: EditDraft): Promise<WorkGraphView> {
    if (!needsTwoProposals(draft)) {
      await this.handlers["work.propose"](proposal(draft));
      return this.load();
    }
    const before = this.state.graph;
    await this.handlers["work.propose"](proposal(draft, "nodes"));
    const after = await this.load();
    const localIds = resolveLocalIds(draft, before, after);
    await this.handlers["work.propose"](proposal({ ...draft, baseRevision: after.revision }, "edges", localIds));
    return this.load();
  }

  close(): void {
    this.bridge.close();
  }
}

describe.skipIf(blocked !== null)("desktop work graph E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_work_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}`, QUANSIO_TRUST_SCOPE_HEADERS: "1" },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-work-"));
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

  it("shows the canonical revision, resolves a concurrent edit as a rebase, and shows a handoff", async () => {
    const alice = new Page(origin, userData, "desktop:work-a");
    const bob = new Page(origin, userData, "desktop:work-b");
    await alice.open();

    // Graph projection: an objective through CreateObjective, then a plan under it; the
    // rendered view equals the canonical revision after every step.
    const seeded = await alice.load();
    expect(seeded.nodes.map((node) => node.id)).toEqual([NODE]);
    const created = (await alice.handlers["work.objective"]({ title: "Launch the report", description: "for the board" })) as { objective_id: string; thread_id: string; revision: number };
    expect(created.objective_id).toMatch(/^wn_/);
    expect(created.thread_id).toMatch(/^thr_/);
    await waitFor(() => (alice.pushed.some((p) => p.channel === "stream.frame" && (p.payload as { kind: string }).kind === "event" && ((p.payload as { event: { type?: string } }).event.type ?? "").startsWith("work.")) ? true : undefined), "a work event");
    alice.feed();
    expect(alice.state.stale).toBe(true);
    const withObjective = await alice.load();
    expect(withObjective.revision).toBe(created.revision);
    expect(matchesCanonical(alice.state, withObjective)).toBe(true);
    const objective = withObjective.nodes.find((node) => node.id === created.objective_id);
    expect(objective?.kind).toBe("objective");
    expect(objective?.threadId).toBe(created.thread_id);
    expect(objective?.contract.humanSignoffRequired).toBe(true);

    let draft = newDraft(WORKSPACE, withObjective.revision, "lay out the plan");
    draft = withIntent(draft, { kind: "add_node", localId: "gather", nodeKind: "task", title: "gather data", description: null, parentId: created.objective_id, priority: 1, contract: null });
    draft = withIntent(draft, { kind: "add_node", localId: "write", nodeKind: "task", title: "write report", description: null, parentId: created.objective_id, priority: 1, contract: null });
    draft = withIntent(draft, { kind: "add_edge", localId: "dep", fromNodeId: "write", toNodeId: "gather", edgeKind: "depends_on" });
    const planned = await alice.propose(draft);
    const gather = planned.nodes.find((node) => node.title === "gather data");
    const write = planned.nodes.find((node) => node.title === "write report");
    expect(gather?.parentId).toBe(created.objective_id);
    expect(planned.edges.some((edge) => edge.kind === "depends_on" && edge.fromNodeId === write?.id && edge.toNodeId === gather?.id)).toBe(true);
    expect(planned.blockers.some((blocker) => blocker.nodeId === write?.id && blocker.kind === "prerequisite" && blocker.ref === gather?.id)).toBe(true);
    expect(matchesCanonical(alice.state, decodeWorkGraph(await alice.handlers["work.graph"]({})))).toBe(true);

    // Revision conflict: Bob loads the same revision; Alice edits first; Bob's edit is
    // refused with CONFLICT_REVISION and nothing of his is written. He rebases and succeeds.
    const bobView = await bob.load();
    expect(bobView.revision).toBe(planned.revision);
    const aliceEdit = withIntent(newDraft(WORKSPACE, planned.revision), { kind: "add_node", localId: "review", nodeKind: "milestone", title: "review", description: null, parentId: created.objective_id, priority: 1, contract: null });
    const afterAlice = await alice.propose(aliceEdit);
    expect(afterAlice.revision).toBeGreaterThan(planned.revision);
    let bobDraft = withIntent(newDraft(WORKSPACE, bobView.revision), { kind: "add_node", localId: "publish", nodeKind: "task", title: "publish", description: null, parentId: created.objective_id, priority: 1, contract: null });
    bobDraft = withIntent(bobDraft, { kind: "transition", nodeId: gather?.id ?? "", to: "ready", expectedRevision: gather?.revision ?? 1 });
    let conflict: unknown = null;
    try {
      await bob.propose(bobDraft);
    } catch (error) {
      conflict = error;
    }
    expect(conflict).toBeInstanceOf(ApiError);
    expect((conflict as ApiError).code).toBe("CONFLICT_REVISION");
    const untouched = await bob.load();
    expect(untouched.revision).toBe(afterAlice.revision);
    expect(untouched.nodes.some((node) => node.title === "publish")).toBe(false);
    const rebased = rebase(bobDraft, untouched);
    expect(rebased.dropped).toEqual([]);
    expect(rebased.draft.baseRevision).toBe(untouched.revision);
    const afterBob = await bob.propose(rebased.draft);
    expect(afterBob.nodes.some((node) => node.title === "publish")).toBe(true);
    expect(afterBob.nodes.find((node) => node.id === gather?.id)?.status).toBe("ready");
    expect(afterBob.nodes.some((node) => node.title === "review")).toBe(true);
    // Alice's view learns of Bob's edit through the durable events, not by polling.
    await waitFor(() => {
      alice.feed();
      return alice.state.stale ? true : undefined;
    }, "alice sees the change");
    const aliceNow = await alice.load();
    expect(aliceNow.revision).toBe(afterBob.revision);

    // Agent handoff UI: a delegation makes a worker and a handoff edge; the run parks.
    const posted = (await alice.handlers["messages.post"]({ threadId: null, content: `split this ${DELEGATE_SCENARIO}` })) as { run_id: string };
    // The delegating turn runs after the post is accepted: wait for the worker it makes.
    const withHandoff = await eventually(async () => {
      alice.feed();
      const view = await alice.load();
      return view.agents.some((agent) => agent.kind === "worker") ? view : undefined;
    }, "the delegated worker");
    const worker = withHandoff.agents.find((agent) => agent.kind === "worker");
    const teammate = withHandoff.agents.find((agent) => agent.kind === "teammate");
    expect(worker?.parentId).toBe(teammate?.id);
    expect(worker?.workNodeId).toBe(NODE);
    const handoff = withHandoff.handoffs.find((entry) => entry.childAgentThreadId === worker?.id);
    expect(handoff?.parentAgentThreadId).toBe(teammate?.id);
    expect(handoff?.joinedAt).toBeNull();
    const node = withHandoff.nodes.find((entry) => entry.id === NODE);
    expect(node?.run?.id).toBe(posted.run_id);
    expect(node?.run?.status).toBe("WAITING_CHILD");
    expect(withHandoff.blockers.find((blocker) => blocker.nodeId === NODE)).toMatchObject({ kind: "run", status: "WAITING_CHILD" });

    alice.close();
    bob.close();
  }, 90_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
