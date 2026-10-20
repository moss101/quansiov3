/**
 * Starting a worker from the app (EXEC-002, DOMAIN.md §8.3.2): the person picks a folder, the desktop registers the
 * workspace's worker target with the server and starts `qworkerd` on this computer against it, so the agent's files,
 * terminal and browser run on the person's own machine without anyone seeding a database or typing flags.
 *
 * What the renderer may say is "start" and "stop"; it names no path and no program. The folder is the person's pick in a
 * native dialog, the program is found by main (an environment override, the app's resources, or a development build), and
 * the socket, tenant and target are what the server's own answer names. The worker is the same daemon a headless install
 * runs; it holds no credential and decides nothing, and stopping the app stops it.
 */

import { existsSync, mkdirSync, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";

/** The slice of a child process the launcher drives (Node's `ChildProcess` satisfies it; a test supplies its own). */
export interface WorkerChild {
  readonly pid?: number | undefined;
  kill(signal?: NodeJS.Signals): boolean;
  on(event: "exit", listener: (code: number | null, signal: NodeJS.Signals | null) => void): unknown;
  on(event: "error", listener: (error: Error) => void): unknown;
  readonly stderr: { on(event: "data", listener: (chunk: Buffer | string) => void): unknown } | null;
}

export type SpawnWorker = (binary: string, args: readonly string[]) => WorkerChild;

/** What the server answered to `ProvisionTarget`. */
export interface ProvisionedWorker {
  readonly target_id: string;
  readonly tenant_id: string;
  readonly status: string;
  readonly created: boolean;
  readonly attached: boolean;
  readonly worker_socket: string | null;
}

export interface WorkerLauncherOptions {
  /** `ProvisionTarget` for this workspace; idempotent, so it is also how the launcher asks whether the worker attached. */
  readonly provision: () => Promise<ProvisionedWorker>;
  /** The native folder dialog; `null` when the person cancels. */
  readonly pickFolder: () => Promise<string | null>;
  /** The worker program, or `null` when this build has none. */
  readonly binary: string | null;
  /** The browser the worker may drive, or `null` for a worker with no browser. */
  readonly chrome: string | null;
  /** Where the worker keeps its browser profile and journal; never inside the folder it works in. */
  readonly stateRoot: string;
  /** Paths a worker must never be given as its folder (the shell's own data). */
  readonly protectedPaths: readonly string[];
  /** Whether the server runs on this computer: its worker socket is a local file. */
  readonly serverIsLocal: boolean;
  readonly spawn: SpawnWorker;
  /** How long to wait for the worker to attach before reporting it did not. */
  readonly attachTimeoutMs?: number;
  readonly sleep?: (ms: number) => Promise<void>;
  readonly log: (line: string) => void;
}

export type WorkerState = "stopped" | "starting" | "running" | "exited";

export interface WorkerStatus {
  /** Whether this app can start a worker, and if not, why. */
  readonly available: boolean;
  readonly reason: string | null;
  readonly state: WorkerState;
  readonly pid: number | null;
  /** The folder the worker works in, while it runs. */
  readonly root: string | null;
  readonly targetId: string | null;
  /** Whether the server holds the worker's lease now. */
  readonly attached: boolean;
  readonly exitCode: number | null;
  /** The worker's last words, for a person to read when it stopped on its own. */
  readonly lastError: string | null;
}

/** The refusal of a start, as the settled channels answer it. */
export class WorkerLaunchError extends Error {
  constructor(
    readonly code: "VALIDATION_SCHEMA" | "CONFLICT_STATE" | "TARGET_UNAVAILABLE" | "POLICY_DENIED",
    message: string,
  ) {
    super(message);
  }
}

const LOOPBACK = new Set(["127.0.0.1", "localhost", "::1", "[::1]"]);

/** Whether an origin is this computer. */
export function isLoopbackOrigin(origin: string): boolean {
  try {
    return LOOPBACK.has(new URL(origin).hostname);
  } catch {
    return false;
  }
}

/** Find the worker program: an override named in the environment, the app's own resources, then a development build. */
export function discoverWorkerBinary(env: NodeJS.ProcessEnv, resourcesPath: string | null, repoRoot: string | null, exists: (path: string) => boolean = existsSync): string | null {
  const candidates = [
    env["QUANSIO_QWORKERD_BIN"]?.trim() ?? "",
    resourcesPath ? join(resourcesPath, "bin", "qworkerd") : "",
    repoRoot ? join(repoRoot, "target", "debug", "qworkerd") : "",
  ];
  return candidates.find((path) => path !== "" && isAbsolute(path) && exists(path)) ?? null;
}

/** The browser a worker may drive on this platform, if one is installed. */
export function discoverChrome(platform: string, env: NodeJS.ProcessEnv, exists: (path: string) => boolean = existsSync): string | null {
  const candidates =
    platform === "darwin"
      ? ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium"]
      : platform === "win32"
        ? [join(env["PROGRAMFILES"] ?? "C:\\Program Files", "Google", "Chrome", "Application", "chrome.exe")]
        : ["/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser"];
  return candidates.find(exists) ?? null;
}

const STDERR_LINES = 12;

export class WorkerLauncher {
  readonly #o: WorkerLauncherOptions;
  #child: WorkerChild | null = null;
  #state: WorkerState = "stopped";
  #root: string | null = null;
  #targetId: string | null = null;
  #exitCode: number | null = null;
  #stderr: string[] = [];
  #starting: Promise<WorkerStatus> | null = null;

  constructor(options: WorkerLauncherOptions) {
    this.#o = options;
  }

  #unavailable(): string | null {
    if (!this.#o.serverIsLocal) {
      return "the server is not on this computer, and a worker dials a local socket";
    }
    if (this.#o.binary === null) {
      return "this build of the app has no worker program (set QUANSIO_QWORKERD_BIN)";
    }
    return null;
  }

  /** What the app can do and what the worker is doing; `attached` is the server's word, asked fresh. */
  async status(): Promise<WorkerStatus> {
    let attached = false;
    if (this.#state === "running" && this.#o.serverIsLocal) {
      attached = await this.#o.provision().then((answer) => answer.attached, () => false);
    }
    return this.#snapshot(attached);
  }

  #snapshot(attached: boolean): WorkerStatus {
    const reason = this.#unavailable();
    return {
      available: reason === null,
      reason,
      state: this.#state,
      pid: this.#child?.pid ?? null,
      root: this.#root,
      targetId: this.#targetId,
      attached,
      exitCode: this.#exitCode,
      lastError: this.#stderr.length === 0 ? null : this.#stderr.join("\n"),
    };
  }

  /**
   * Ask for a folder, register the workspace's worker target and start the worker against it. Resolves once the server
   * holds the worker's lease, or refuses saying why. A second call while one is in progress shares it.
   */
  start(): Promise<WorkerStatus> {
    if (this.#starting) {
      return this.#starting;
    }
    this.#starting = this.#start().finally(() => {
      this.#starting = null;
    });
    return this.#starting;
  }

  async #start(): Promise<WorkerStatus> {
    const o = this.#o;
    const reason = this.#unavailable();
    if (reason !== null || o.binary === null) {
      throw new WorkerLaunchError("TARGET_UNAVAILABLE", reason ?? "no worker program");
    }
    if (this.#state === "running" || this.#state === "starting") {
      throw new WorkerLaunchError("CONFLICT_STATE", "a worker is already running; stop it first");
    }
    const picked = await o.pickFolder();
    if (picked === null) {
      return this.#snapshot(false);
    }
    const root = this.#acceptableRoot(picked);
    const provisioned = await o.provision();
    if (provisioned.worker_socket === null || !existsSync(provisioned.worker_socket)) {
      throw new WorkerLaunchError("TARGET_UNAVAILABLE", "the server has no worker socket to attach to");
    }
    const stateDir = join(o.stateRoot, provisioned.target_id);
    mkdirSync(stateDir, { recursive: true, mode: 0o700 });
    const args = ["--socket", provisioned.worker_socket, "--tenant", provisioned.tenant_id, "--target", provisioned.target_id, "--workspace-root", root];
    if (o.chrome !== null) {
      args.push("--chrome", o.chrome, "--state-dir", stateDir);
    }
    this.#state = "starting";
    this.#root = root;
    this.#targetId = provisioned.target_id;
    this.#exitCode = null;
    this.#stderr = [];
    const child = o.spawn(o.binary, args);
    this.#child = child;
    child.stderr?.on("data", (chunk) => {
      for (const line of String(chunk).split("\n")) {
        if (line.trim() !== "") {
          this.#stderr.push(line.slice(0, 400));
        }
      }
      this.#stderr = this.#stderr.slice(-STDERR_LINES);
    });
    child.on("error", (error) => {
      this.#stderr.push(`could not start the worker: ${error.message}`);
      this.#ended(null);
    });
    child.on("exit", (code) => {
      o.log(`worker exited with ${String(code)}`);
      this.#ended(code);
    });
    // The worker reconnects on its own; what is awaited is the first attach, which proves the socket, the tenant and the
    // target agree. If it exits first or never attaches, the person is told, with what the worker said.
    const sleep = o.sleep ?? ((ms: number) => new Promise<void>((done) => setTimeout(done, ms)));
    const deadline = o.attachTimeoutMs ?? 20_000;
    for (let waited = 0; waited <= deadline; waited += 250) {
      if (this.#child !== child) {
        throw new WorkerLaunchError("TARGET_UNAVAILABLE", `the worker stopped before it attached${this.#stderr.length === 0 ? "" : `: ${this.#stderr[this.#stderr.length - 1] ?? ""}`}`);
      }
      const answer = await o.provision().catch(() => null);
      if (answer?.attached) {
        this.#state = "running";
        return this.#snapshot(true);
      }
      await sleep(250);
    }
    await this.stop();
    throw new WorkerLaunchError("TARGET_UNAVAILABLE", "the worker did not attach to the server in time");
  }

  #ended(code: number | null): void {
    this.#child = null;
    this.#exitCode = code;
    this.#state = this.#state === "stopped" ? "stopped" : "exited";
  }

  /** Stop the worker. Whatever it was running is interrupted and settles as unknown on the server; nothing is re-sent. */
  stop(): Promise<WorkerStatus> {
    const child = this.#child;
    this.#state = "stopped";
    this.#child = null;
    child?.kill("SIGTERM");
    return Promise.resolve(this.#snapshot(false));
  }

  /** Stop the worker because the app is quitting. */
  dispose(): void {
    void this.stop();
  }

  /** A folder the worker may work in: an absolute existing directory that is not the whole disk, the home folder or the shell's own data. */
  #acceptableRoot(picked: string): string {
    const refuse = (message: string): never => {
      throw new WorkerLaunchError("VALIDATION_SCHEMA", message);
    };
    if (!isAbsolute(picked)) {
      return refuse("the folder is not an absolute path");
    }
    let real: string;
    try {
      real = realpathSync(resolve(picked));
      if (!statSync(real).isDirectory()) {
        return refuse("that is not a folder");
      }
    } catch {
      return refuse("that folder cannot be opened");
    }
    if (real === dirname(real) || real === realpathSync(homedir())) {
      return refuse("a worker works in a project folder, not the whole disk or the home folder");
    }
    for (const protectedPath of this.#o.protectedPaths) {
      let guarded: string;
      try {
        guarded = realpathSync(protectedPath);
      } catch {
        continue;
      }
      const inside = relative(guarded, real);
      const contains = relative(real, guarded);
      if (inside === "" || (!inside.startsWith("..") && !isAbsolute(inside)) || (!contains.startsWith("..") && !isAbsolute(contains) && contains !== "")) {
        return refuse("that folder belongs to Quansio itself");
      }
    }
    return real;
  }
}
