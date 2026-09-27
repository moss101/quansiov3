/**
 * Collaboration E2E (APP-009) against the real `quansio-server` binary on a scratch
 * PostgreSQL database seeded by `quansio-dev-seed` (an owner and an editor), driven through
 * two desktop bridges — two clients, two people, two live streams.
 *
 * * multi-client ordering — both people post into one group thread at once; every post is
 *   accepted, both clients' streams deliver `thread.message_posted` in the same `seq`
 *   order, and both read the same conversation;
 * * membership revoke — once the owner removes the editor, the editor's stream carries
 *   their removal and nothing more of the thread while the owner keeps posting, and the
 *   editor's reads and posts are refused;
 * * handoff — a conversation with one teammate is handed to another: the notice is in the
 *   thread, the next un-addressed message goes to the new teammate.
 *
 * Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN) and the workspace's
 * `target/debug/quansio-server` + `quansio-dev-seed`. Without them the suite skips with an
 * explicit `BLOCKED_EXTERNAL` marker (AGENTS.md "Real boundaries").
 */

import { answered, settled } from "./settle.js";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { DesktopBridge, SessionFile, UpdateHooks, decodeThreadMessages, decodeThreadView, handOffParams, participantParams, type SocketLike } from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0C0EA9";
const OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0C0EA9";
const EDITOR = "usr_01J8Z3K6F1N8VQ2X5W9Y0C0EA8";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0C0EA9";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0C0EA9";

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

function waitFor<T>(poll: () => T | undefined, what: string, timeoutMs = 20_000): Promise<T> {
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

const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

interface Pushed {
  readonly channel: string;
  readonly payload: unknown;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: Record<string, unknown>;
  readonly error?: { readonly code: string; readonly message: string };
}

/** The durable events a client received for one thread, in arrival order. */
function threadEvents(pushed: readonly Pushed[], threadId: string): Record<string, unknown>[] {
  return pushed
    .filter((p) => p.channel === "stream.frame")
    .map((p) => p.payload as { kind?: string; event?: Record<string, unknown> })
    .filter((frame) => frame.kind === "event" && frame.event?.["aggregate_type"] === "thread" && frame.event["aggregate_id"] === threadId)
    .map((frame) => frame.event as Record<string, unknown>);
}

function posted(events: readonly Record<string, unknown>[]): number[] {
  return events.filter((event) => event["type"] === "thread.message_posted").map((event) => (event["payload"] as { seq: number }).seq);
}

describe.skipIf(blocked !== null)("desktop collaboration E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  const dirs: string[] = [];
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_collab_e2e_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}`, QUANSIO_TRUST_SCOPE_HEADERS: "1" },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      dropScratchDatabase(databaseName);
    } catch {
      /* the scratch database is named by pid; a leftover is harmless */
    }
    for (const dir of dirs) {
      rmSync(dir, { recursive: true, force: true });
    }
  }, 30_000);

  async function client(userId: string, consumer: string): Promise<{ bridge: DesktopBridge; handlers: ReturnType<DesktopBridge["handlers"]>; pushed: Pushed[] }> {
    const dir = mkdtempSync(join(tmpdir(), "quansio-desktop-collab-"));
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
    });
    const handlers = bridge.handlers();
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), `${consumer} stream open`);
    return { bridge, handlers, pushed };
  }

  it("keeps one order for two clients posting at once, then revokes the removed one", async () => {
    const owner = await client(OWNER, "desktop:collab-owner");
    const editor = await client(EDITOR, "desktop:collab-editor");
    const created = (await owner.handlers["threads.create"]({ kind: "group", title: "launch", participants: [{ kind: "user", id: EDITOR, role: "member" }] })) as SettledPayload;
    expect(created.ok, JSON.stringify(created)).toBe(true);
    const thread = created.result?.["thread_id"] as string;

    // Both people post at once.
    const posts = await Promise.all(
      Array.from({ length: 6 }, (_, n) =>
        (n % 2 === 0 ? owner : editor).handlers["messages.post"]({ threadId: thread, content: `message ${String(n)}` }) as Promise<{ message_id: string; run_id: string }>,
      ),
    );
    expect(new Set(posts.map((post) => post.message_id)).size).toBe(6);
    // Every post's turn answers after the post is accepted; the thread is read once each has.
    await Promise.all(posts.map((post) => answered(owner.handlers, post.run_id, post.message_id)));
    const viewOwner = decodeThreadMessages(await owner.handlers["threads.messages"]({ threadId: thread }));
    const viewEditor = decodeThreadMessages(await editor.handlers["threads.messages"]({ threadId: thread }));
    expect(viewOwner.map((m) => m.id)).toEqual(viewEditor.map((m) => m.id));
    expect(viewOwner.map((m) => m.seq)).toEqual(viewOwner.map((_, index) => index + 1));
    // Both streams carry every post, in the same seq order.
    const total = viewOwner.length;
    const ownerOrder = await waitFor(() => (posted(threadEvents(owner.pushed, thread)).length === total ? posted(threadEvents(owner.pushed, thread)) : undefined), "owner stream");
    const editorOrder = await waitFor(() => (posted(threadEvents(editor.pushed, thread)).length === total ? posted(threadEvents(editor.pushed, thread)) : undefined), "editor stream");
    expect(ownerOrder).toEqual(viewOwner.map((m) => m.seq));
    expect(editorOrder).toEqual(ownerOrder);

    // The owner removes the editor: the editor hears it, and nothing of the thread after.
    const removed = (await owner.handlers["participants.remove"](participantParams(thread, "user", EDITOR))) as SettledPayload;
    expect(removed.ok, JSON.stringify(removed)).toBe(true);
    await waitFor(() => threadEvents(editor.pushed, thread).find((event) => event["type"] === "thread.participant_removed"), "the editor's removal on their stream");
    const heardBefore = threadEvents(editor.pushed, thread).length;
    const after = (await owner.handlers["messages.post"]({ threadId: thread, content: "after the removal" })) as { message_id: string };
    await waitFor(() => threadEvents(owner.pushed, thread).find((event) => (event["payload"] as { message_id?: string }).message_id === after.message_id), "the owner's post on the owner's stream");
    // The editor's stream is live (other events keep arriving) but carries nothing more of the thread.
    await pause(750);
    expect(threadEvents(editor.pushed, thread).length).toBe(heardBefore);
    expect(threadEvents(editor.pushed, thread).at(-1)?.["type"]).toBe("thread.participant_removed");
    const refused = (await editor.handlers["threads.view"]({ threadId: thread })) as SettledPayload;
    expect(refused.ok).toBe(false);
    expect(refused.error?.code).toBe("SCOPE_FORBIDDEN");
    await expect(editor.handlers["threads.messages"]({ threadId: thread })).rejects.toThrow(/SCOPE_FORBIDDEN|protected/);
    await expect(editor.handlers["messages.post"]({ threadId: thread, content: "still here?" })).rejects.toThrow(/SCOPE_FORBIDDEN|protected/);
    const list = (await editor.handlers["threads.list"]({})) as { threads: { id: string }[] };
    expect(list.threads.some((entry) => entry.id === thread)).toBe(false);
    owner.bridge.close();
    editor.bridge.close();
  }, 90_000);

  it("hands a conversation from one teammate to another", async () => {
    const owner = await client(OWNER, "desktop:collab-handoff");
    const atlas = (await owner.handlers["teammates.create"]({ template_id: "engineer", name: "Atlas" })) as SettledPayload;
    expect(atlas.ok, JSON.stringify(atlas)).toBe(true);
    const nova = (await owner.handlers["teammates.create"]({ template_id: "researcher", name: "Nova" })) as SettledPayload;
    expect(nova.ok, JSON.stringify(nova)).toBe(true);
    const atlasId = atlas.result?.["id"] as string;
    const novaId = nova.result?.["id"] as string;

    const first = (await owner.handlers["messages.post"]({ content: "draft the plan", teammateId: atlasId })) as { thread_id: string; run_id: string; teammate: { id: string } };
    expect(first.teammate.id).toBe(atlasId);
    // A handoff waits for the teammate's run to finish; its turn runs after the post is accepted.
    await settled(owner.handlers, first.run_id);
    const thread = first.thread_id;
    const before = (await owner.handlers["threads.view"]({ threadId: thread })) as SettledPayload;
    expect(decodeThreadView(before.result).responsibleTeammateId).toBe(atlasId);

    const handed = (await owner.handlers["threads.handoff"](handOffParams(thread, novaId))) as SettledPayload;
    expect(handed.ok, JSON.stringify(handed)).toBe(true);
    await waitFor(() => threadEvents(owner.pushed, thread).find((event) => event["type"] === "thread.handed_off"), "thread.handed_off on the stream");
    const view = decodeThreadView(((await owner.handlers["threads.view"]({ threadId: thread })) as SettledPayload).result);
    expect(view.responsibleTeammateId).toBe(novaId);
    expect(view.participants.find((p) => p.id === atlasId)?.active).toBe(false);
    expect(view.participants.find((p) => p.id === novaId)?.active).toBe(true);
    const messages = decodeThreadMessages(await owner.handlers["threads.messages"]({ threadId: thread }));
    const notice = messages.find((m) => m.authorKind === "system");
    expect(notice?.text).toBe("Atlas handed this thread to Nova.");
    expect(notice?.notice?.["kind"]).toBe("handoff");

    const next = (await owner.handlers["messages.post"]({ threadId: thread, content: "where are we?" })) as { teammate: { id: string } };
    expect(next.teammate.id).toBe(novaId);
    const again = (await owner.handlers["threads.handoff"](handOffParams(thread, novaId))) as SettledPayload;
    expect(again.ok).toBe(false);
    expect(again.error?.code).toBe("CONFLICT_STATE");
    owner.bridge.close();
  }, 90_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
