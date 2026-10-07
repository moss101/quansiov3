/**
 * Update deferral (ADP-004, DOMAIN.md §13.6 G).
 *
 * Installing an update restarts the app, which pauses whatever is running. This gate is the only caller of
 * `UpdateHooks.install()`, and it installs without asking only when the server's maintenance status says idle:
 *
 * - **Restart now** reads the status first. Idle: install. Busy or unreadable: ask, with what is running, and let
 *   the person restart anyway, install when idle, or leave it for later. Nothing is installed over running work
 *   without that choice.
 * - **Install when idle** polls the status with a self-rescheduling timer (never an interval) and installs at the
 *   first idle reading; a busy or unreadable reading just waits for the next one.
 * - **Install on quit** is applied only after the quit confirmation has permitted the quit, so it cannot skip it;
 *   a failed install turns it off (`UpdateHooks`).
 */

import { describeWork, type WorkProbe, type WorkStatus } from "./work-status.js";

export type UpdateChoice = "install" | "when_idle" | "later";

export type UpdateOutcome =
  | { readonly kind: "not_downloaded" }
  | { readonly kind: "installing" }
  | { readonly kind: "deferred" }
  | { readonly kind: "waiting_for_idle" }
  | { readonly kind: "failed" };

/** The slice of `UpdateHooks` the gate uses. */
export interface UpdateHost {
  readonly downloaded: boolean;
  install(): boolean;
}

export interface UpdateGateOptions {
  readonly updates: UpdateHost;
  readonly probe: WorkProbe;
  /** Ask the person what to do about an update that cannot install silently. */
  readonly ask: (status: Exclude<WorkStatus, { kind: "idle" }>) => Promise<UpdateChoice>;
  /** Tell the quit gate the restart that is about to happen has been decided (so it does not ask a second time). */
  readonly allowQuit: () => void;
  /** Take that permission back (the install did not start, so the app is not quitting). */
  readonly revokeQuit: () => void;
  readonly pollMs: number;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
  readonly clearTimer: (handle: unknown) => void;
  readonly onWaiting?: (waiting: boolean) => void;
}

export class UpdateGate {
  readonly #options: UpdateGateOptions;
  #timer: unknown = null;
  #waiting = false;

  constructor(options: UpdateGateOptions) {
    this.#options = options;
  }

  /** Whether an install is waiting for idle. */
  get waitingForIdle(): boolean {
    return this.#waiting;
  }

  #install(): UpdateOutcome {
    this.#options.allowQuit();
    if (this.#options.updates.install()) {
      return { kind: "installing" };
    }
    // Nothing was started, so nothing is quitting: the next quit asks again.
    this.#options.revokeQuit();
    return { kind: "failed" };
  }

  async #read(): Promise<WorkStatus> {
    try {
      return await this.#options.probe();
    } catch {
      return { kind: "unknown" };
    }
  }

  /** The person asked to restart for the update. */
  async requestInstall(): Promise<UpdateOutcome> {
    if (!this.#options.updates.downloaded) {
      return { kind: "not_downloaded" };
    }
    const status = await this.#read();
    if (status.kind === "idle") {
      this.#stopWaiting();
      return this.#install();
    }
    const choice = await this.#options.ask(status);
    if (choice === "install") {
      this.#stopWaiting();
      return this.#install();
    }
    if (choice === "when_idle") {
      this.installWhenIdle();
      return { kind: "waiting_for_idle" };
    }
    return { kind: "deferred" };
  }

  /** Install at the first idle reading (and keep checking until then). */
  installWhenIdle(): void {
    if (this.#waiting || !this.#options.updates.downloaded) {
      return;
    }
    this.#waiting = true;
    this.#options.onWaiting?.(true);
    this.#schedule();
  }

  #schedule(): void {
    this.#timer = this.#options.setTimer(() => {
      this.#timer = null;
      void this.#tick();
    }, this.#options.pollMs);
  }

  async #tick(): Promise<void> {
    if (!this.#waiting) {
      return;
    }
    const status = await this.#read();
    if (!this.waitingForIdle) {
      return;
    }
    if (status.kind === "idle" && this.#options.updates.downloaded) {
      this.#stopWaiting();
      this.#install();
      return;
    }
    this.#schedule();
  }

  /** Stop waiting for idle (the person changed their mind, or the install went ahead). */
  cancelWhenIdle(): void {
    this.#stopWaiting();
  }

  #stopWaiting(): void {
    if (this.#timer !== null) {
      this.#options.clearTimer(this.#timer);
      this.#timer = null;
    }
    if (this.#waiting) {
      this.#waiting = false;
      this.#options.onWaiting?.(false);
    }
  }

  /** The wording for what is running, for a dialog built from a status. */
  static describe(status: Exclude<WorkStatus, { kind: "idle" }>): string {
    return describeWork(status);
  }
}
