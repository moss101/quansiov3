/**
 * A real `qworkerd` for the E2Es whose scripted turns call `fs.*` (EXEC-002): the served server refuses a worker tool
 * `WORKER_UNAVAILABLE` unless a worker is attached, so those tests seed the `--worker` target, give the server a
 * `QUANSIO_WORKER_SOCKET` and dial a real daemon in on it. Its workspace root is a scratch directory holding `notes.md`.
 *
 * Environment: `QUANSIO_TEST_QWORKERD_BIN` (default `target/debug/qworkerd`, built by `cargo build -p quansio-qworkerd`).
 */
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/** The READY `customer_private_worker` target `quansio-dev-seed --worker` registers. */
export const WORKER_TARGET = "tgt_01J8Z3K6F1N8VQ2X5W9Y0E2WK1";

export const QWORKERD_BIN =
  process.env["QUANSIO_TEST_QWORKERD_BIN"]?.trim() || join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "target", "debug", "qworkerd");

export interface WorkerHandle {
  /** `QUANSIO_WORKER_SOCKET` for the server. */
  readonly socket: string;
  /** Dial the daemon in; resolves once the server's hub says it attached. */
  attach(serverStderr: string[]): Promise<void>;
  stop(): void;
}

/** A socket directory (short, under /tmp: Unix socket paths are length-limited) and a workspace root with `notes.md`. */
export function prepareWorker(tenant: string): WorkerHandle {
  const socketDir = mkdtempSync(join("/tmp", "qw-"));
  const root = mkdtempSync(join(tmpdir(), "quansio-worker-root-"));
  mkdirSync(root, { recursive: true });
  writeFileSync(join(root, "notes.md"), "notes for the conformance turn\n");
  const socket = join(socketDir, "w.sock");
  let daemon: ChildProcess | null = null;
  return {
    socket,
    async attach(serverStderr) {
      daemon = spawn(QWORKERD_BIN, ["--socket", socket, "--tenant", tenant, "--target", WORKER_TARGET, "--workspace-root", root], { stdio: ["ignore", "ignore", "pipe"] });
      const deadline = Date.now() + 30_000;
      while (!serverStderr.join("").includes(`worker attached for ${WORKER_TARGET}`)) {
        if (daemon.exitCode !== null) {
          throw new Error(`qworkerd exited with ${String(daemon.exitCode)}`);
        }
        if (Date.now() > deadline) {
          throw new Error(`no worker attached for ${WORKER_TARGET}: ${serverStderr.join("")}`);
        }
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    },
    stop() {
      daemon?.kill("SIGKILL");
      rmSync(socketDir, { recursive: true, force: true });
      rmSync(root, { recursive: true, force: true });
    },
  };
}

export const workerBinMissing = (): boolean => !existsSync(QWORKERD_BIN);
