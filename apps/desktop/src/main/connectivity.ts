/**
 * Connectivity (ADP-004, DOMAIN.md §13.6 F).
 *
 * The renderer reports `online`/`offline`. An offline report is held for a short grace period (10 s,
 * `config/desktop-shell.yaml`) before the shell treats the machine as offline, so a brief flap (a Wi-Fi handover, a
 * captive-portal probe) causes nothing: an online report inside the grace cancels it with no reconnect.
 *
 * What the monitor does with a signal is call the one reconnect that already exists: the bridge closes the
 * stream transport and resubscribes from the durable cursor (`client/stream.ts`, backoff 250 ms to 8 s). The
 * signals that call it are a return to online after being offline, a resume from sleep and a window focus while
 * the stream is not open. Several signals arriving together (resume, then online, then focus) cause one
 * reconnect: calls inside `minGapMs` of the last are dropped. There is no second reconnection path.
 */

export type ConnectivityState = "online" | "offline_pending" | "offline";

export interface ConnectivityOptions {
  readonly graceMs: number;
  readonly minGapMs: number;
  /** The existing stream reconnect; `force` resubscribes even when the stream claims to be open. */
  readonly reconnect: (force: boolean) => void;
  /** Whether the stream is open now (a window focus reconnects only when it is not). */
  readonly streamOpen: () => boolean;
  readonly now: () => number;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
  readonly clearTimer: (handle: unknown) => void;
  readonly onState?: (state: ConnectivityState) => void;
}

export class ConnectivityMonitor {
  readonly #options: ConnectivityOptions;
  #state: ConnectivityState = "online";
  #timer: unknown = null;
  #lastReconnect = Number.NEGATIVE_INFINITY;

  constructor(options: ConnectivityOptions) {
    this.#options = options;
  }

  get state(): ConnectivityState {
    return this.#state;
  }

  #set(state: ConnectivityState): void {
    if (state !== this.#state) {
      this.#state = state;
      this.#options.onState?.(state);
    }
  }

  #reconnect(force: boolean): void {
    const now = this.#options.now();
    if (now - this.#lastReconnect < this.#options.minGapMs) {
      return;
    }
    this.#lastReconnect = now;
    this.#options.reconnect(force);
  }

  /** The renderer's `navigator.onLine` changed. */
  report(online: boolean): void {
    if (!online) {
      if (this.#state === "online") {
        this.#set("offline_pending");
        this.#timer = this.#options.setTimer(() => {
          this.#timer = null;
          if (this.#state === "offline_pending") {
            this.#set("offline");
          }
        }, this.#options.graceMs);
      }
      return;
    }
    if (this.#timer !== null) {
      this.#options.clearTimer(this.#timer);
      this.#timer = null;
    }
    const was = this.#state;
    this.#set("online");
    if (was === "offline") {
      // A socket that was open when the network went away may be half-dead: resubscribe regardless.
      this.#reconnect(true);
    }
  }

  /** The machine woke from sleep: the stream is suspect whatever the renderer says. */
  resumed(): void {
    this.#reconnect(true);
  }

  /** A window gained focus: reconnect if the stream is not open (and the machine is not known to be offline). */
  focused(): void {
    if (this.#state === "online" && !this.#options.streamOpen()) {
      this.#reconnect(false);
    }
  }

  dispose(): void {
    if (this.#timer !== null) {
      this.#options.clearTimer(this.#timer);
      this.#timer = null;
    }
  }
}
