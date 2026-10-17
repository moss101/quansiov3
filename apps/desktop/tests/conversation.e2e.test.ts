/**
 * Conversation E2E (APP-004) against the real `quansio-server` on a scratch database:
 * the streaming reply reaches the page, a reload rebuilds the same thread from the
 * projections, a turn is cancelled and stops, an attachment is a real binary that
 * round-trips through the object store, and an agent question is answered in the thread.
 *
 * The page is stood in for by the bridge handlers plus the reducer the renderer uses; the
 * DOM is not the contract, the bridge and the state are. Scratch database and tenant come
 * from `quansio-dev-seed` (Rust): the desktop never writes a canonical table.
 *
 * Environment: `QUANSIO_TEST_POSTGRES_URL`, `target/debug/quansio-server` and
 * `quansio-dev-seed`; the attachment case also needs the dev MinIO
 * (`QUANSIO_TEST_MINIO_ENDPOINT`, default `http://127.0.0.1:59010`). Without them the suite
 * reports `BLOCKED_EXTERNAL` rather than passing vacuously.
 */

import { prepareWorker, workerBinMissing, type WorkerHandle } from "./worker.js";
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
  cancellableRun,
  conversationReducer,
  decodeMessages,
  decodeQuestions,
  decodeTimeline,
  initialConversation,
  openQuestions,
  postedIdentities,
  type ConversationState,
  type SocketLike,
  type StreamFrame,
} from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");
const MINIO = process.env["QUANSIO_TEST_MINIO_ENDPOINT"]?.trim() || "http://127.0.0.1:59010";

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const QUESTION_SCENARIO = "[[scenario:question]]";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN) || workerBinMissing()
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN}, ${SEED_BIN} and the qworkerd binary are needed; build them with cargo build -p quansio-server --bins && cargo build -p quansio-qworkerd`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", USER, "--workspace", WORKSPACE, "--node", NODE, "--worker"],
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

/** A page: the bridge's handlers and the reducer fed with everything main pushed. */
class Page {
  readonly pushed: { channel: string; payload: unknown }[] = [];
  readonly bridge: DesktopBridge;
  readonly handlers: ReturnType<DesktopBridge["handlers"]>;
  state: ConversationState;
  #fed = 0;

  constructor(origin: string, userData: string, consumer: string, threadId: string | null = null) {
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
    this.state = initialConversation(WORKSPACE, threadId);
  }

  async open(): Promise<void> {
    await this.handlers["stream.subscribe"]({});
    await waitFor(() => (this.pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream open");
  }

  /** Apply every frame pushed since the last call, in order. */
  feed(): void {
    for (; this.#fed < this.pushed.length; this.#fed += 1) {
      const entry = this.pushed[this.#fed];
      if (entry?.channel === "stream.frame") {
        this.state = conversationReducer(this.state, { type: "frame", frame: entry.payload as StreamFrame });
      }
    }
  }

  /** What the renderer's effects do: re-read whatever an event marked stale. */
  async refresh(): Promise<void> {
    this.feed();
    if (this.state.threadId && this.state.stale.messages) {
      const messages = decodeMessages(await this.handlers["threads.messages"]({ threadId: this.state.threadId }));
      this.state = conversationReducer(this.state, { type: "messages.loaded", threadId: this.state.threadId, messages });
    }
    if (this.state.threadId && this.state.stale.questions) {
      const questions = decodeQuestions(await this.handlers["questions.list"]({ threadId: this.state.threadId }));
      this.state = conversationReducer(this.state, { type: "questions.loaded", questions });
    }
  }

  async post(content: string, attachments: string[] = []): Promise<{ threadId: string; runId: string; messageId: string }> {
    this.state = conversationReducer(this.state, { type: "composer.text", text: content });
    this.state = conversationReducer(this.state, { type: "composer.posting" });
    const identities = postedIdentities(await this.handlers["messages.post"]({ threadId: this.state.threadId, content, attachments }));
    this.state = conversationReducer(this.state, { type: "composer.posted", ...identities });
    return identities;
  }

  frames(): StreamFrame[] {
    return this.pushed.filter((p) => p.channel === "stream.frame").map((p) => p.payload as StreamFrame);
  }

  close(): void {
    this.bridge.close();
  }
}

describe.skipIf(blocked !== null)("desktop conversation E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  let minio = false;
  let worker: WorkerHandle;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_conv_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    minio = await minioReachable();
    worker = prepareWorker(TENANT);
    child = spawn(SERVER_BIN, [], {
      env: {
        ...process.env,
        QUANSIO_DATABASE_URL: databaseUrl,
        QUANSIO_BIND: `127.0.0.1:${String(port)}`,
        QUANSIO_TRUST_SCOPE_HEADERS: "1",
        QUANSIO_WORKER_SOCKET: worker.socket,
        // Attachments go to the dev MinIO when it is up; otherwise uploads fail closed.
        ...(minio ? { QUANSIO_OBJECT_STORE: "1", QUANSIO_TEST_MINIO_ENDPOINT: MINIO } : {}),
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    await worker.attach(stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-conv-"));
  }, 90_000);

  afterAll(async () => {
    worker.stop();
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      dropScratchDatabase(databaseName);
    } catch {
      /* named by pid; a leftover is harmless */
    }
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  it("streams the reply, lands it as a durable message, and a reload rebuilds the same thread with its inspector", async () => {
    const page = new Page(origin, userData, "desktop:conv");
    await page.open();
    const posted = await page.post("hello from the desktop");
    expect(posted.runId).toMatch(/^run_/);
    expect(page.state.threadId).toBe(posted.threadId);

    // The live frames of this run reached the page: progress, then the model delta.
    await waitFor(() => (page.frames().some((f) => f.kind === "live" && f.frame["kind"] === "model.delta" && f.frame["run_id"] === posted.runId) ? true : undefined), "model.delta");
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "thread.message_posted" && (f.event["payload"] as { author_kind?: string }).author_kind === "agent") ? true : undefined), "agent reply");
    page.feed();
    const liveTexts = page.frames().filter((f) => f.kind === "live" && f.frame["kind"] === "model.delta").map((f) => (f.kind === "live" ? String(f.frame["text"]) : ""));
    expect(liveTexts.length).toBeGreaterThan(0);
    expect(page.state.runs[posted.runId]?.cards.some((card) => card.kind === "tool_call")).toBe(true);

    // The durable reply replaces the streamed preview.
    await waitFor(() => (page.state.stale.messages ? true : undefined), "stale messages");
    await page.refresh();
    const reply = page.state.messages.find((message) => message.authorKind === "agent" && message.runId === posted.runId);
    expect(reply?.text).toBe("done");
    expect(page.state.runs[posted.runId]?.streaming).toBe("");
    const threads = (await page.handlers["threads.list"]({})) as { threads: { id: string; message_count: number }[] };
    expect(threads.threads.find((thread) => thread.id === posted.threadId)?.message_count).toBe(2);

    // Reload: a fresh page over the same session file rebuilds the thread from projections.
    page.close();
    const restored = (await new Page(origin, userData, "desktop:conv-reload").handlers["session.activeRun"]({})) as { activeRun: { threadId: string; runId: string } | null };
    expect(restored.activeRun?.threadId).toBe(posted.threadId);
    const reloaded = new Page(origin, userData, "desktop:conv-reloaded", restored.activeRun?.threadId ?? null);
    await reloaded.refresh();
    expect(reloaded.state.messages.map((message) => [message.authorKind, message.text])).toEqual(page.state.messages.map((message) => [message.authorKind, message.text]));
    const inspection = decodeTimeline(await reloaded.handlers["runs.timeline"]({ runId: posted.runId }));
    expect(inspection.workNodeId).toBe(NODE);
    expect(inspection.turns).toHaveLength(1);
    expect(inspection.turns[0]?.modelRoute).toBe("in-process/conformance-stub");
    expect(inspection.turns[0]?.tools.map((tool) => [tool.toolName, tool.status])).toEqual([["fs.read", "completed"]]);
    expect(inspection.turns[0]?.tools[0]?.effectStatus).not.toBeNull();
    reloaded.close();
  }, 60_000);

  it("asks a question in the thread, waits, and continues from the answer", async () => {
    const page = new Page(origin, userData, "desktop:conv-question");
    await page.open();
    const posted = await page.post(`which one? ${QUESTION_SCENARIO}`);
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "question.asked") ? true : undefined), "question.asked");
    await page.refresh();
    expect(page.state.runs[posted.runId]?.status).toBe("WAITING_QUESTION");
    const question = openQuestions(page.state)[0];
    if (!question) {
      throw new Error("no open question");
    }
    expect(question.options).toEqual(["alpha", "beta"]);

    await page.handlers["questions.answer"]({ questionId: question.id, value: "beta" });
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "question.answered") ? true : undefined), "question.answered");
    // The reply is durable only once the agent's message is posted; the parked turn's own
    // `turn.completed` came earlier and is not it.
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "thread.message_posted" && (f.event["payload"] as { author_kind?: string }).author_kind === "agent") ? true : undefined), "agent reply");
    await page.refresh();
    expect(openQuestions(page.state)).toEqual([]);
    expect(page.state.questions[0]?.answer).toBe("beta");
    const reply = page.state.messages.find((message) => message.authorKind === "agent" && message.runId === posted.runId);
    expect(reply?.text).toBe("done: you chose beta");
    const inspection = decodeTimeline(await page.handlers["runs.timeline"]({ runId: posted.runId }));
    expect(inspection.turns.map((turn) => turn.inputKind)).toEqual(["message", "wake"]);
    expect(inspection.turns[0]?.question?.status).toBe("answered");
    page.close();
  }, 60_000);

  it("cancels a turn and the run stops: the page loses its cancel, the question is withdrawn, a late answer is refused", async () => {
    const page = new Page(origin, userData, "desktop:conv-cancel");
    await page.open();
    const posted = await page.post(`stop me ${QUESTION_SCENARIO}`);
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "question.asked") ? true : undefined), "question.asked");
    await page.refresh();
    expect(cancellableRun(page.state)?.runId).toBe(posted.runId);
    const question = openQuestions(page.state)[0];
    if (!question) {
      throw new Error("no open question");
    }

    const cancelled = (await page.handlers["turns.cancel"]({ runId: posted.runId })) as { status: string; cancelled_questions: string[] };
    expect(cancelled.status).toBe("CANCELLED");
    expect(cancelled.cancelled_questions).toEqual([question.id]);
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "run.cancelled") ? true : undefined), "run.cancelled");
    // The run is cancelled first and its question withdrawn next; the page re-reads the
    // questions once `question.cancelled` lands, so wait for it before checking them.
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "question.cancelled") ? true : undefined), "question.cancelled");
    await page.refresh();
    expect(page.state.runs[posted.runId]?.status).toBe("CANCELLED");
    expect(cancellableRun(page.state)).toBeNull();
    expect(openQuestions(page.state)).toEqual([]);
    expect(page.state.questions[0]?.status).toBe("cancelled");
    await expect(page.handlers["questions.answer"]({ questionId: question.id, value: "alpha" })).rejects.toMatchObject({ code: "CONFLICT_STATE" });
    const run = await page.bridge.server.run(posted.runId);
    expect(run["status"]).toBe("CANCELLED");
    page.close();
  }, 60_000);

  it("uploads a real binary as an artifact and attaches it to a message the inspector shows", async () => {
    if (!minio) {
      console.error(`BLOCKED_EXTERNAL: dev MinIO at ${MINIO} is not reachable; the attachment case needs it`);
      return;
    }
    const page = new Page(origin, userData, "desktop:conv-attach");
    await page.open();
    // Every byte value, not valid UTF-8, and a PNG signature up front: bytes, not text.
    const bytes = new Uint8Array(3000);
    const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = signature[index] ?? index % 256;
    }
    const uploaded = (await page.handlers["artifacts.upload"]({
      name: "chart.png",
      mediaType: "image/png",
      bytesBase64: Buffer.from(bytes).toString("base64"),
      threadId: null,
    })) as { artifact_id: string; size_bytes: number; kind: string; content_digest: string };
    expect(uploaded.artifact_id).toMatch(/^art_/);
    expect(uploaded.size_bytes).toBe(bytes.length);
    expect(uploaded.kind).toBe("image");

    const posted = await page.post("see the attached chart", [uploaded.artifact_id]);
    await waitFor(() => (page.frames().some((f) => f.kind === "event" && f.event["type"] === "thread.message_posted" && (f.event["payload"] as { author_kind?: string }).author_kind === "agent") ? true : undefined), "agent reply");
    await page.refresh();
    expect(page.state.messages[0]?.attachments).toEqual([uploaded.artifact_id]);
    const inspection = decodeTimeline(await page.handlers["runs.timeline"]({ runId: posted.runId }));
    expect(inspection.attachments).toEqual([uploaded.artifact_id]);
    const artifacts = (await page.bridge.server.artifacts(WORKSPACE)) as { artifacts: { id: string; size_bytes: number; content_digest: string }[] };
    const listed = artifacts.artifacts.find((artifact) => artifact.id === uploaded.artifact_id);
    expect(listed?.size_bytes).toBe(bytes.length);
    expect(listed?.content_digest).toBe(uploaded.content_digest);

    // A foreign artifact id is refused, and an oversized upload never leaves the bridge.
    await expect(page.handlers["messages.post"]({ threadId: posted.threadId, content: "x", attachments: ["art_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ"] })).rejects.toBeInstanceOf(ApiError);
    await expect(page.handlers["artifacts.upload"]({ name: "big", mediaType: "application/octet-stream", bytesBase64: "A".repeat(12 * 1024 * 1024) })).rejects.toThrow(/exceeds/);
    page.close();
  }, 60_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
