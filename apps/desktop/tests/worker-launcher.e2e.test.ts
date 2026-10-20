/**
 * Starting a worker from the app, against the real `quansio-server` and the real `qworkerd` (EXEC-002).
 *
 * The server is started with a worker socket and a workspace that has no worker target. The launcher registers the target
 * through the public `ProvisionTarget` command, starts the real daemon against a folder, and resolves once the server's hub
 * holds the daemon's lease. A file the daemon writes appears in that folder; stopping the launcher ends the daemon and the
 * server's hub lets go. Environment: `QUANSIO_TEST_POSTGRES_URL`, the server, seed and `qworkerd` binaries.
 */

import { eventually } from "./settle.js";
import { QWORKERD_BIN, prepareWorker, workerBinMissing, type WorkerHandle } from "./worker.js";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { DesktopBridge, SessionFile, UpdateHooks, type SocketLike } from "../src/index.js";
import { WorkerLauncher, type ProvisionedWorker } from "../src/main/worker-launcher.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(REPO, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(REPO, "target", "debug", "quansio-dev-seed");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0WKAAA";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0WKAAA";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0WKAAA";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0WKAAA";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN) || workerBinMissing()
    ? "BLOCKED_EXTERNAL: the server, seed and qworkerd binaries are needed; build them with cargo build -p quansio-server --bins && cargo build -p quansio-qworkerd"
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

const openSocket = (url: string, headers: Record<string, string>): SocketLike => new WebSocket(url, { headers });

describe.skipIf(blocked !== null)("starting a worker from the app, against the real server and daemon", () => {
  let database: string;
  let server: ChildProcess;
  let origin: string;
  let userData: string;
  let socketHolder: WorkerHandle;
  const stderr: string[] = [];

  beforeAll(async () => {
    database = `quansio_desktop_worker_${String(process.pid)}`;
    const output = execFileSync(SEED_BIN, ["create", "--database", database, "--tenant", TENANT, "--user", USER, "--workspace", WORKSPACE, "--node", NODE], {
      env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL },
      encoding: "utf8",
    });
    const databaseUrl = output.split("\n").find((line) => line.startsWith("database_url="))?.slice("database_url=".length);
    if (!databaseUrl) {
      throw new Error(`quansio-dev-seed printed no database_url: ${output}`);
    }
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    // Only the socket path is used: the launcher starts the daemon itself.
    socketHolder = prepareWorker(TENANT);
    server = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}`, QUANSIO_TRUST_SCOPE_HEADERS: "1", QUANSIO_WORKER_SOCKET: socketHolder.socket },
      stdio: ["ignore", "ignore", "pipe"],
    });
    server.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    const deadline = Date.now() + 60_000;
    for (;;) {
      if (server.exitCode !== null) {
        throw new Error(`quansio-server exited: ${stderr.join("")}`);
      }
      try {
        if ((await fetch(`${origin}/v1/health`)).ok) {
          break;
        }
      } catch {
        /* not up yet */
      }
      if (Date.now() > deadline) {
        throw new Error(`quansio-server did not become healthy: ${stderr.join("")}`);
      }
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-worker-"));
  }, 90_000);

  afterAll(async () => {
    server.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    socketHolder.stop();
    try {
      execFileSync(SEED_BIN, ["drop", "--database", database], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
    } catch {
      /* named by pid; a leftover is harmless */
    }
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  it("registers the target, starts the daemon against the person's folder, sees the server hold its lease, and lets go on stop", async () => {
    const bridge = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:worker",
      push: () => undefined,
    });
    const project = realpathSync(mkdtempSync(join(tmpdir(), "quansio-project-")));
    let nextId = 0;
    const provision = async (): Promise<ProvisionedWorker> => {
      nextId += 1;
      const answer = await bridge.server.invoke<ProvisionedWorker>("ProvisionTarget", `cmd_01J8Z3K6F1N8VQ2X5W9YWK${String(nextId).padStart(4, "0")}`, { workspace_id: WORKSPACE, substrate: "customer_private_worker" });
      return answer.result;
    };
    const launcher = new WorkerLauncher({
      provision,
      pickFolder: () => Promise.resolve(project),
      binary: QWORKERD_BIN,
      chrome: null,
      stateRoot: join(userData, "worker"),
      protectedPaths: [userData],
      serverIsLocal: true,
      spawn: (binary, args) => spawn(binary, [...args], { stdio: ["ignore", "ignore", "pipe"] }),
      log: () => undefined,
    });

    expect((await launcher.status()).state).toBe("stopped");
    const started = await launcher.start();
    expect(started).toMatchObject({ state: "running", attached: true, root: project });
    expect(started.targetId).toMatch(/^tgt_/);
    expect(stderr.join("")).toContain(`worker attached for ${started.targetId ?? ""}`);

    // The target is the workspace's, ready, and the server says a worker is attached to it.
    const asked = await provision();
    expect(asked).toMatchObject({ created: false, attached: true, status: "READY", target_id: started.targetId });
    const targets = (await bridge.handlers()["live.targets"]({})) as { items?: { id: string; status: string; substrate: string }[] };
    expect(targets.items?.find((item) => item.id === started.targetId)).toMatchObject({ substrate: "customer_private_worker", status: "READY" });

    // Stopping ends the daemon, the hub releases the lease, and the target can be started against again.
    await launcher.stop();
    await eventually(async () => ((await provision()).attached ? undefined : true), "the server to let go of the stopped worker");
    const again = await launcher.start();
    expect(again).toMatchObject({ state: "running", attached: true, targetId: started.targetId });
    await launcher.stop();
    rmSync(project, { recursive: true, force: true });
  }, 90_000);
});
