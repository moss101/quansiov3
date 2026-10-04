/**
 * The person's recorder, as the desktop's main process runs it (ADP-003, DOMAIN.md §11.7).
 *
 * Main starts `quansio-recorder` for the length of one recording, hands it a one-time token in a bootstrap
 * file (mode 0600, deleted by the recorder as it reads it) and talks to it over a private Unix-domain socket.
 * The recorder observes the person and redacts before it keeps anything; main never sees what it observes, only
 * counts, and at the end the archive it built, which main forwards to the server as `FinishRecording`.
 *
 * What this file guarantees, each pinned by `tests/recorder.test.ts`:
 *
 * - **the person finishes it.** `finish` needs a genuine click (`gesture`), and nothing in this file calls it
 *   but the IPC handler a click reaches; a recorder that is merely quiet, full or old is never finished by main;
 * - **the owner fence is main's.** The epoch is a number main advances whenever the signed-in account changes
 *   (`accountChanged`); every command main sends carries it, and an account change ends the local recording at
 *   once (the recorder is stopped and what it held is gone);
 * - **no permission, no capture.** The recorder is asked for its permissions first and `start` refuses, with the
 *   typed permission, before any recording exists on the server.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createConnection, type Socket } from "node:net";
import { join } from "node:path";

import { newCommandId } from "../client/ids.js";
import { IDLE_RECORDER, decodeRecorder, type RecorderView } from "../recording/state.js";

/** The slice of the server client the host needs. */
export interface RecordingServerPort {
  invoke(name: string, commandId: string, params: Record<string, unknown>): Promise<{ readonly result: unknown }>;
}

/** Who is signed in, as main knows it. */
export interface RecorderIdentity {
  readonly tenantId: string;
  readonly userId: string;
}

export interface RecorderHostOptions {
  readonly server: RecordingServerPort;
  /** The `quansio-recorder` executable, or null when this build has none. */
  readonly binary: string | null;
  readonly identity: () => RecorderIdentity | null;
  /** Where the private directory is made. A Unix socket path must fit in 104 bytes, so not `os.tmpdir()`. */
  readonly tempRoot?: string;
  /** How long to wait for the recorder to say it is ready; the first launch of a new binary can be slow. */
  readonly readyTimeoutMs?: number;
  /** How often the recorder's counts are read while recording. */
  readonly pollMs?: number;
}

/** A refusal as data: the page branches on `code`. */
export interface Refusal {
  readonly ok: false;
  readonly code: string;
  readonly message: string;
  readonly permission?: string;
  readonly status?: string;
  readonly permissions?: Readonly<Record<string, string>>;
}

export type Outcome<T> = { readonly ok: true; readonly value: T } | Refusal;

function refusal(code: string, message: string, extra: Partial<Refusal> = {}): Refusal {
  return { ok: false, code, message, ...extra };
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

/** One authenticated connection to a recorder: newline-delimited JSON, one request in flight at a time. */
export class RecorderChannel {
  readonly #socket: Socket;
  #buffer = "";
  #waiting: { resolve: (line: Record<string, unknown>) => void; reject: (error: Error) => void } | null = null;
  #closed = false;
  #chain: Promise<unknown> = Promise.resolve();

  private constructor(socket: Socket) {
    this.#socket = socket;
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      this.#buffer += chunk;
      let index = this.#buffer.indexOf("\n");
      while (index >= 0) {
        const line = this.#buffer.slice(0, index);
        this.#buffer = this.#buffer.slice(index + 1);
        this.#deliver(line);
        index = this.#buffer.indexOf("\n");
      }
    });
    const closed = (): void => {
      this.#closed = true;
      this.#waiting?.reject(new Error("the recorder closed its channel"));
      this.#waiting = null;
    };
    socket.on("close", closed);
    socket.on("error", closed);
  }

  static connect(path: string): Promise<RecorderChannel> {
    return new Promise((resolve, reject) => {
      const socket = createConnection(path);
      socket.once("connect", () => {
        resolve(new RecorderChannel(socket));
      });
      socket.once("error", reject);
    });
  }

  #deliver(line: string): void {
    let parsed: Record<string, unknown>;
    try {
      parsed = record(JSON.parse(line));
    } catch {
      parsed = { ok: false, error: "malformed_answer" };
    }
    // Answers are per request and in order; one that arrives with nobody waiting is dropped.
    const waiting = this.#waiting;
    if (waiting) {
      this.#waiting = null;
      waiting.resolve(parsed);
    }
  }

  get closed(): boolean {
    return this.#closed;
  }

  /** Send one request and read its answer. */
  call(request: Record<string, unknown>, timeoutMs = 120_000): Promise<Record<string, unknown>> {
    const run = (): Promise<Record<string, unknown>> =>
      new Promise((resolve, reject) => {
        if (this.#closed) {
          reject(new Error("the recorder closed its channel"));
          return;
        }
        const timer = setTimeout(() => {
          this.#waiting = null;
          reject(new Error("the recorder did not answer in time"));
        }, timeoutMs);
        this.#waiting = {
          resolve: (answer) => {
            clearTimeout(timer);
            resolve(answer);
          },
          reject: (error) => {
            clearTimeout(timer);
            reject(error);
          },
        };
        this.#socket.write(`${JSON.stringify(request)}\n`, (error) => {
          if (error) {
            clearTimeout(timer);
            this.#waiting = null;
            reject(error);
          }
        });
      });
    const next = this.#chain.then(run, run);
    this.#chain = next.catch(() => undefined);
    return next;
  }

  close(): void {
    this.#socket.destroy();
  }
}

interface Running {
  readonly child: ChildProcess;
  readonly channel: RecorderChannel;
  readonly directory: string;
}

/** Runs the person's recorder and the commands around it. */
export class RecordingHost {
  readonly #options: RecorderHostOptions;
  #epoch = 1;
  #running: Running | null = null;
  #view: RecorderView = IDLE_RECORDER;
  #recordingId: string | null = null;
  #poll: ReturnType<typeof setInterval> | null = null;
  #pendingArchive: { readonly base64: string; readonly sha256: string; readonly requestId: string } | null = null;
  #finishCalls = 0;

  constructor(options: RecorderHostOptions) {
    this.#options = options;
  }

  /** The epoch main holds for the signed-in account; it is added to every recording command. */
  get epoch(): number {
    return this.#epoch;
  }

  status(): RecorderView {
    return this.#view;
  }

  /** Whether `running` is still the recorder this host holds (the account may have changed while awaiting). */
  #holds(running: Running): boolean {
    return this.#running === running;
  }

  /** The account changed (sign-in, sign-out or a switch): advance the epoch and end any local recording. */
  accountChanged(): void {
    this.#epoch += 1;
    if (this.#running) {
      this.#teardown("account_changed");
    }
  }

  /** Stop the recorder without a word to the server (application quit). */
  dispose(): void {
    this.#teardown(null);
  }

  // ---- start -----------------------------------------------------------------------------------

  async start(workspaceId: string): Promise<Outcome<{ readonly recordingId: string }>> {
    if (this.#view.state !== "idle" || this.#running) {
      return refusal("CONFLICT_STATE", "a recording is already in progress");
    }
    if (!this.#options.binary) {
      return refusal("RECORDER_UNAVAILABLE", "this build has no recorder (quansio-recorder)");
    }
    const identity = this.#options.identity();
    if (!identity) {
      return refusal("AUTH_REQUIRED", "sign in to record");
    }
    const epoch = this.#epoch;
    this.#view = { ...IDLE_RECORDER, state: "starting" };
    let running: Running;
    try {
      running = await this.#launch();
    } catch (error) {
      this.#view = { ...IDLE_RECORDER, notice: "recorder_failed" };
      return refusal("RECORDER_UNAVAILABLE", error instanceof Error ? error.message : "the recorder could not be started");
    }
    this.#running = running;
    // No permission, no recording: ask the recorder before the server hears of it.
    try {
      const permissions = record((await running.channel.call({ op: "permissions", id: newCommandId() }))["permissions"]);
      for (const needed of ["accessibility", "input_monitoring"]) {
        const status = permissions[needed];
        if (status !== "granted") {
          this.#teardown(null);
          return refusal("RECORDING_PERMISSION_REQUIRED", `${needed} is ${String(status)}`, {
            permission: needed,
            status: String(status),
            permissions: Object.fromEntries(Object.entries(permissions).map(([name, value]) => [name, String(value)])),
          });
        }
      }
    } catch (error) {
      this.#teardown(null);
      return refusal("RECORDER_UNAVAILABLE", error instanceof Error ? error.message : "the recorder did not answer");
    }
    let started: Record<string, unknown>;
    try {
      started = record(
        (await this.#options.server.invoke("StartRecording", newCommandId(), { workspace_id: workspaceId, start_key: newCommandId(), epoch })).result,
      );
    } catch (error) {
      this.#teardown(null);
      return refusal(errorCode(error), errorMessage(error));
    }
    const recordingId = typeof started["recording_id"] === "string" ? started["recording_id"] : null;
    if (!recordingId || epoch !== this.#epoch || !this.#holds(running)) {
      // The account changed while the server was answering: the recording is not ours to carry on.
      if (recordingId) {
        await this.#cancelOnServer(recordingId, epoch);
      }
      this.#teardown(null);
      return refusal("RECORDING_OWNER_CHANGED", "the account changed while the recording was starting");
    }
    const answer = await running.channel
      .call({
        op: "start",
        id: newCommandId(),
        recording_id: recordingId,
        owner: { tenant_id: identity.tenantId, user_id: identity.userId, epoch },
        limits: started["limits"],
        max_duration_ms: started["max_duration_ms"],
      })
      .catch((error: unknown): Record<string, unknown> => ({ ok: false, error: "recorder_failed", detail: errorMessage(error) }));
    if (answer["ok"] !== true) {
      await this.#cancelOnServer(recordingId, epoch);
      this.#teardown(null);
      if (answer["error"] === "permission_required") {
        return refusal("RECORDING_PERMISSION_REQUIRED", "a permission the recording needs is missing", {
          permission: String(answer["permission"]),
          status: String(answer["status"]),
        });
      }
      return refusal("RECORDER_UNAVAILABLE", text(answer["detail"]) ?? text(answer["error"]) ?? "the recorder refused");
    }
    this.#recordingId = recordingId;
    this.#pendingArchive = null;
    this.#view = { ...IDLE_RECORDER, state: "recording", recordingId, startedAt: new Date().toISOString() };
    this.#startPolling(identity, epoch);
    return { ok: true, value: { recordingId } };
  }

  // ---- finish and cancel ---------------------------------------------------------------------------

  /**
   * Finish the recording: only the person's own click does this. The request must carry `gesture: true`, which
   * the page sets from a genuine (`isTrusted`) click; anything else is refused before the recorder is asked.
   */
  async finish(request: { readonly gesture: boolean }): Promise<Outcome<unknown>> {
    if (!request.gesture) {
      return refusal("GESTURE_REQUIRED", "a recording is finished by the person's own click");
    }
    const running = this.#running;
    const recordingId = this.#recordingId;
    const identity = this.#options.identity();
    if (!running || !recordingId || !identity || (this.#view.state !== "recording" && this.#view.state !== "finishing")) {
      return refusal("CONFLICT_STATE", "there is no recording in progress to finish");
    }
    const epoch = this.#epoch;
    this.#stopPolling();
    this.#view = { ...this.#view, state: "finishing" };
    if (!this.#pendingArchive) {
      this.#finishCalls += 1;
      const requestId = `fin_${String(this.#finishCalls)}_${newCommandId().slice(-8)}`;
      let answer: Record<string, unknown>;
      try {
        answer = await running.channel.call({
          op: "finish",
          id: requestId,
          request_id: requestId,
          owner: { tenant_id: identity.tenantId, user_id: identity.userId, epoch },
        });
      } catch (error) {
        this.#teardown("recorder_stopped");
        return refusal("RECORDER_UNAVAILABLE", errorMessage(error));
      }
      if (answer["ok"] !== true) {
        const code = answer["error"] === "owner_changed" ? "RECORDING_OWNER_CHANGED" : answer["error"] === "expired" ? "CONFLICT_STATE" : "RECORDER_UNAVAILABLE";
        this.#teardown(text(answer["error"]) ?? "recorder_failed");
        return refusal(code, text(answer["detail"]) ?? text(answer["error"]) ?? "the recorder refused");
      }
      this.#pendingArchive = {
        base64: String(answer["archive_base64"]),
        sha256: String(answer["sha256"]),
        requestId,
      };
    }
    const archive = this.#pendingArchive;
    try {
      const result = (
        await this.#options.server.invoke("FinishRecording", newCommandId(), {
          recording_id: recordingId,
          epoch,
          archive: { content_base64: archive.base64, content_digest: archive.sha256 },
        })
      ).result;
      this.#teardown(null);
      return { ok: true, value: result };
    } catch (error) {
      // The server refused (an unredacted field, a limit): the archive is kept here for one more try
      // or a cancel; nothing was stored.
      this.#view = { ...this.#view, state: "finishing", notice: errorCode(error) };
      return refusal(errorCode(error), errorMessage(error));
    }
  }

  async cancel(): Promise<Outcome<unknown>> {
    const recordingId = this.#recordingId;
    const epoch = this.#epoch;
    if (!this.#running && !recordingId) {
      return refusal("CONFLICT_STATE", "there is no recording in progress to cancel");
    }
    this.#teardown(null);
    if (recordingId) {
      const result = await this.#cancelOnServer(recordingId, epoch);
      return result;
    }
    return { ok: true, value: {} };
  }

  async #cancelOnServer(recordingId: string, epoch: number): Promise<Outcome<unknown>> {
    try {
      return { ok: true, value: (await this.#options.server.invoke("CancelRecording", newCommandId(), { recording_id: recordingId, epoch })).result };
    } catch (error) {
      return refusal(errorCode(error), errorMessage(error));
    }
  }

  // ---- the recorder process ----------------------------------------------------------------------------

  async #launch(): Promise<Running> {
    const binary = this.#options.binary;
    if (!binary) {
      throw new Error("no recorder binary");
    }
    const directory = mkdtempSync(join(this.#options.tempRoot ?? "/tmp", "qrec."));
    const token = randomBytes(32).toString("hex");
    const bootstrap = join(directory, "token");
    writeFileSync(bootstrap, `${token}\n`, { mode: 0o600 });
    chmodSync(bootstrap, 0o600);
    const child = spawn(binary, ["--bootstrap", bootstrap, "--socket-dir", join(directory, "sock"), "--parent-pid", String(process.pid)], {
      stdio: ["ignore", "pipe", "ignore"],
    });
    try {
      const socketPath = await new Promise<string>((resolve, reject) => {
        let seen = "";
        const timer = setTimeout(() => {
          reject(new Error("the recorder did not become ready in time"));
        }, this.#options.readyTimeoutMs ?? 120_000);
        child.stdout.setEncoding("utf8");
        child.stdout.on("data", (chunk: string) => {
          seen += chunk;
          const line = seen.split("\n").find((candidate) => candidate.startsWith("quansio-recorder ready "));
          if (line) {
            clearTimeout(timer);
            resolve(line.slice("quansio-recorder ready ".length).trim());
          }
        });
        child.once("exit", (code) => {
          clearTimeout(timer);
          reject(new Error(`the recorder exited (${String(code)}) before it was ready`));
        });
        child.once("error", (error) => {
          clearTimeout(timer);
          reject(error);
        });
      });
      const channel = await RecorderChannel.connect(socketPath);
      const hello = await channel.call({ op: "hello", token }, 30_000);
      if (hello["ok"] !== true) {
        channel.close();
        throw new Error("the recorder refused the channel");
      }
      return { child, channel, directory };
    } catch (error) {
      child.kill();
      rmSync(directory, { recursive: true, force: true });
      throw error;
    }
  }

  #startPolling(identity: RecorderIdentity, epoch: number): void {
    this.#stopPolling();
    this.#poll = setInterval(() => {
      const running = this.#running;
      if (!running || this.#view.state !== "recording") {
        return;
      }
      void running.channel
        .call({ op: "status", id: newCommandId(), owner: { tenant_id: identity.tenantId, user_id: identity.userId, epoch } }, 10_000)
        .then((answer) => {
          if (answer["ok"] !== true) {
            this.#teardown(text(answer["error"]) ?? "recorder_stopped");
            return;
          }
          this.#view = decodeRecorder({
            state: "recording",
            recordingId: this.#recordingId,
            startedAt: this.#view.startedAt,
            actions: answer["actions"],
            screenshots: answer["screenshots"],
            paused: answer["paused"],
            counts: answer["counts"],
          });
        })
        .catch(() => {
          this.#teardown("recorder_stopped");
        });
    }, this.#options.pollMs ?? 1000);
  }

  #stopPolling(): void {
    if (this.#poll) {
      clearInterval(this.#poll);
      this.#poll = null;
    }
  }

  /** End the local recorder: its channel closes and it discards what it held. */
  #teardown(notice: string | null): void {
    this.#stopPolling();
    const running = this.#running;
    this.#running = null;
    this.#recordingId = null;
    this.#pendingArchive = null;
    this.#view = { ...IDLE_RECORDER, notice };
    if (running) {
      running.channel.close();
      running.child.kill();
      rmSync(running.directory, { recursive: true, force: true });
    }
  }
}

function errorCode(error: unknown): string {
  const code = error && typeof error === "object" ? (error as { code?: unknown }).code : undefined;
  return typeof code === "string" ? code : "INTERNAL";
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "the call failed";
}
