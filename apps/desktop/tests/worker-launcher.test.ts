/**
 * Starting a worker from the app (EXEC-002): the person's folder pick is validated, the worker is registered and started
 * against what the server's own answer names, the launcher waits for the server to hold the worker's lease and says plainly
 * when it never does, and the renderer's channels carry no path and no program. The server's half (the registration, its
 * idempotence, a worker attaching to the registered target) is proved against PostgreSQL in
 * `crates/server/tests/worker_live.rs`; here the process is a stand-in because what is under test is the launcher's decisions.
 */

import { EventEmitter } from "node:events";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { PRELOAD_ALLOWLIST } from "../src/preload/bridge.js";
import { WorkerLaunchError, WorkerLauncher, discoverChrome, discoverWorkerBinary, isLoopbackOrigin, type ProvisionedWorker, type WorkerLauncherOptions } from "../src/main/worker-launcher.js";

class FakeChild extends EventEmitter {
  pid = 4242;
  stderr = new EventEmitter();
  killed: string[] = [];
  kill(signal?: NodeJS.Signals): boolean {
    this.killed.push(signal ?? "SIGTERM");
    return true;
  }
}

function rig(over: Partial<WorkerLauncherOptions> & { attachAfter?: number; folder?: string | null; socket?: string | null } = {}) {
  const base = mkdtempSync(join(tmpdir(), "quansio-launcher-"));
  const socket = join(base, "w.sock");
  writeFileSync(socket, "");
  const work = join(base, "project");
  mkdirSync(work);
  const spawned: { binary: string; args: readonly string[]; child: FakeChild }[] = [];
  let asked = 0;
  const attachAfter = over.attachAfter ?? 2;
  const options: WorkerLauncherOptions = {
    provision: () => {
      asked += 1;
      const answer: ProvisionedWorker = { target_id: "tgt_01J8Z3K6F1N8VQ2X5W9YWRKR01", tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9YWRKR01", status: "READY", created: asked === 1, attached: spawned.length > 0 && asked > attachAfter, worker_socket: over.socket === undefined ? socket : over.socket };
      return Promise.resolve(answer);
    },
    pickFolder: () => Promise.resolve(over.folder === undefined ? work : over.folder),
    binary: "/opt/quansio/qworkerd",
    chrome: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    stateRoot: join(base, "state"),
    protectedPaths: [join(base, "userdata")],
    serverIsLocal: true,
    spawn: (binary, args) => {
      const child = new FakeChild();
      spawned.push({ binary, args, child });
      return child;
    },
    sleep: () => new Promise((done) => setTimeout(done, 1)),
    attachTimeoutMs: 5000,
    log: () => undefined,
    ...over,
  };
  return { launcher: new WorkerLauncher(options), spawned, base, work };
}

describe("starting a worker", () => {
  it("registers the target, starts the program with what the server named, and resolves once the server holds its lease", async () => {
    const { launcher, spawned, work } = rig();
    const status = await launcher.start();
    expect(status).toMatchObject({ state: "running", attached: true, targetId: "tgt_01J8Z3K6F1N8VQ2X5W9YWRKR01" });
    expect(spawned).toHaveLength(1);
    const [only] = spawned;
    expect(only?.binary).toBe("/opt/quansio/qworkerd");
    const args = [...(only?.args ?? [])];
    const flag = (name: string) => args[args.indexOf(name) + 1];
    expect(flag("--tenant")).toBe("tn_01J8Z3K6F1N8VQ2X5W9YWRKR01");
    expect(flag("--target")).toBe("tgt_01J8Z3K6F1N8VQ2X5W9YWRKR01");
    expect(flag("--socket")).toMatch(/w\.sock$/);
    // The folder is the person's pick as the disk resolves it, and the browser's state is kept outside it.
    expect(flag("--workspace-root")).toBe(status.root);
    expect(status.root?.endsWith("project")).toBe(true);
    expect(flag("--state-dir")?.startsWith(work)).toBe(false);
    expect(args).toContain("--chrome");
  });

  it("starts a worker with no browser when there is none to drive", async () => {
    const { launcher, spawned } = rig({ chrome: null });
    await launcher.start();
    expect([...(spawned[0]?.args ?? [])]).not.toContain("--chrome");
  });

  it("does nothing when the person cancels the dialog", async () => {
    const { launcher, spawned } = rig({ folder: null });
    expect(await launcher.start()).toMatchObject({ state: "stopped" });
    expect(spawned).toHaveLength(0);
  });

  it("refuses a folder that is not a project folder, before anything is registered or started", async () => {
    for (const folder of ["relative/path", "/", homedir(), "/definitely/not/a/folder", "/etc/hosts"]) {
      let registered = 0;
      const { launcher, spawned } = rig({ folder, provision: () => { registered += 1; return Promise.reject(new Error("must not be asked")); } });
      await expect(launcher.start(), folder).rejects.toMatchObject({ code: "VALIDATION_SCHEMA" });
      expect(spawned, folder).toHaveLength(0);
      expect(registered, folder).toBe(0);
    }
  });

  it("refuses the shell's own data directory, and anything inside or around it", async () => {
    const base = mkdtempSync(join(tmpdir(), "quansio-launcher-guard-"));
    const userData = join(base, "userdata");
    mkdirSync(join(userData, "inner"), { recursive: true });
    for (const folder of [userData, join(userData, "inner"), base]) {
      const { launcher, spawned } = rig({ folder, protectedPaths: [userData] });
      await expect(launcher.start(), folder).rejects.toMatchObject({ code: "VALIDATION_SCHEMA" });
      expect(spawned).toHaveLength(0);
    }
  });

  it("says so when the server has no worker socket, and when the app cannot start one at all", async () => {
    const none = rig({ socket: null });
    await expect(none.launcher.start()).rejects.toMatchObject({ code: "TARGET_UNAVAILABLE" });
    expect(none.spawned).toHaveLength(0);
    const remote = rig({ serverIsLocal: false });
    await expect(remote.launcher.start()).rejects.toMatchObject({ code: "TARGET_UNAVAILABLE" });
    expect(await remote.launcher.status()).toMatchObject({ available: false });
    const nobinary = rig({ binary: null });
    await expect(nobinary.launcher.start()).rejects.toBeInstanceOf(WorkerLaunchError);
    expect((await nobinary.launcher.status()).reason).toMatch(/no worker program/);
  });

  it("tells the person what the worker said when it stops before it attaches", async () => {
    const { launcher, spawned } = rig({ attachAfter: 100_000, attachTimeoutMs: 250_000 });
    const starting = launcher.start();
    await new Promise((done) => setTimeout(done, 10));
    spawned[0]?.child.stderr.emit("data", "qworkerd: --workspace-root /x: not a directory\n");
    spawned[0]?.child.emit("exit", 2, null);
    await expect(starting).rejects.toThrow(/stopped before it attached: qworkerd: --workspace-root/);
    expect(await launcher.status()).toMatchObject({ state: "exited", exitCode: 2 });
  });

  it("stops a worker that never attaches", async () => {
    const { launcher, spawned } = rig({ attachAfter: 10_000, attachTimeoutMs: 1000 });
    await expect(launcher.start()).rejects.toThrow(/did not attach/);
    expect(spawned[0]?.child.killed).toEqual(["SIGTERM"]);
  });

  it("runs one worker at a time, and stopping it ends the process", async () => {
    const { launcher, spawned } = rig();
    const [first, second] = await Promise.allSettled([launcher.start(), launcher.start()]);
    expect(first.status).toBe("fulfilled");
    expect(second.status).toBe("fulfilled");
    expect(spawned).toHaveLength(1);
    await expect(launcher.start()).rejects.toMatchObject({ code: "CONFLICT_STATE" });
    expect(await launcher.stop()).toMatchObject({ state: "stopped" });
    expect(spawned[0]?.child.killed).toEqual(["SIGTERM"]);
    // A worker that stops on its own is reported as exited, not as running.
    const next = rig();
    await next.launcher.start();
    next.spawned[0]?.child.emit("exit", 1, null);
    expect(await next.launcher.status()).toMatchObject({ state: "exited", exitCode: 1 });
  });
});

describe("finding the program and the browser", () => {
  it("takes an absolute path that exists, from the environment, the app's resources or a development build, in that order", () => {
    const exists = (path: string) => ["/res/bin/qworkerd", "/repo/target/debug/qworkerd", "/env/qworkerd"].includes(path);
    expect(discoverWorkerBinary({ QUANSIO_QWORKERD_BIN: "/env/qworkerd" }, "/res", "/repo", exists)).toBe("/env/qworkerd");
    expect(discoverWorkerBinary({}, "/res", "/repo", exists)).toBe("/res/bin/qworkerd");
    expect(discoverWorkerBinary({}, null, "/repo", exists)).toBe("/repo/target/debug/qworkerd");
    expect(discoverWorkerBinary({ QUANSIO_QWORKERD_BIN: "relative/qworkerd" }, null, null, () => true)).toBeNull();
    expect(discoverWorkerBinary({}, null, null, exists)).toBeNull();
    expect(discoverChrome("darwin", {}, () => true)).toMatch(/Google Chrome/);
    expect(discoverChrome("darwin", {}, () => false)).toBeNull();
  });

  it("calls only this computer local", () => {
    for (const local of ["http://127.0.0.1:8080", "http://localhost:3000", "http://[::1]:9"]) {
      expect(isLoopbackOrigin(local), local).toBe(true);
    }
    for (const remote of ["https://app.example.com", "http://10.0.0.5:8080", "http://127.0.0.1.evil.test", "not a url"]) {
      expect(isLoopbackOrigin(remote), remote).toBe(false);
    }
  });
});

describe("the worker channels", () => {
  it("take no path and no program from the page, and are the main window's alone", async () => {
    for (const channel of ["worker.status", "worker.start", "worker.stop"]) {
      expect((PRELOAD_ALLOWLIST as readonly string[]).includes(channel), channel).toBe(true);
    }
    const { POLICY } = await import("../src/main/ipc-trust.js");
    for (const channel of ["worker.status", "worker.start", "worker.stop"] as const) {
      expect(POLICY[channel].windows, channel).toEqual(["main"]);
    }
    const { readFileSync } = await import("node:fs");
    const bridge = readFileSync(join(__dirname, "..", "src", "preload", "bridge.ts"), "utf8");
    expect(bridge).toContain('startWorker: () => send("worker.start", {})');
    expect(bridge).toContain('workerStatus: () => send("worker.status", {})');
    expect(bridge).toContain('stopWorker: () => send("worker.stop", {})');
  });
});
