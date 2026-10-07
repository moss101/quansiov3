/**
 * Update hooks (APP-003, DOSSIER.md §18).
 *
 * Desktop auto-update channels are `stable` and `beta`. The main process asks the update
 * feed for the channel it was built with, and a downloaded update is applied only after
 * the user is told (`update.status` on the bridge). Signing and notarization, without which
 * the platform updater refuses a package, are REL-002/REL-003; until then `checkForUpdates`
 * reports `unavailable` rather than pretending.
 */

export type UpdateChannel = "stable" | "beta";

export type UpdateStatus =
  | { readonly state: "unavailable"; readonly reason: string }
  | { readonly state: "idle"; readonly channel: UpdateChannel }
  | { readonly state: "checking"; readonly channel: UpdateChannel }
  | { readonly state: "downloaded"; readonly channel: UpdateChannel; readonly version: string }
  | { readonly state: "error"; readonly channel: UpdateChannel; readonly message: string };

/** The slice of Electron's `autoUpdater` the hooks use. */
export interface AutoUpdaterLike {
  setFeedURL(options: { url: string }): void;
  checkForUpdates(): void;
  quitAndInstall(): void;
  on(event: "update-downloaded", listener: (...args: unknown[]) => void): unknown;
  on(event: "error", listener: (error: Error) => void): unknown;
  on(event: "update-not-available" | "checking-for-update", listener: () => void): unknown;
}

export function parseUpdateChannel(value: string | undefined): UpdateChannel {
  return value === "beta" ? "beta" : "stable";
}

/** Feed URL per channel: the server's update catalog, versioned like the public API. */
export function updateFeedUrl(serverOrigin: string, channel: UpdateChannel, platform: string, arch: string): string {
  const origin = serverOrigin.replace(/\/+$/, "");
  return `${origin}/v1/desktop/updates/${channel}/${platform}-${arch}`;
}

export class UpdateHooks {
  #status: UpdateStatus;
  readonly #channel: UpdateChannel;
  readonly #updater: AutoUpdaterLike | null;
  readonly #listeners = new Set<(status: UpdateStatus) => void>();
  readonly #installOnQuitListeners = new Set<(enabled: boolean) => void>();
  /** An install was started and has not been seen to succeed (a successful install ends the process). */
  #installing = false;
  #installOnQuit: boolean;

  constructor(channel: UpdateChannel, updater: AutoUpdaterLike | null, feedUrl: string, signed: boolean, installOnQuit = false) {
    this.#channel = channel;
    this.#installOnQuit = installOnQuit;
    this.#updater = signed ? updater : null;
    this.#status = this.#updater
      ? { state: "idle", channel }
      : { state: "unavailable", reason: signed ? "no platform updater" : "unsigned build: updates are delivered by REL-002/REL-003" };
    if (this.#updater) {
      this.#updater.setFeedURL({ url: feedUrl });
      this.#updater.on("checking-for-update", () => {
        this.#set({ state: "checking", channel });
      });
      this.#updater.on("update-not-available", () => {
        this.#set({ state: "idle", channel });
      });
      this.#updater.on("update-downloaded", (...args: unknown[]) => {
        const version = typeof args[2] === "string" ? args[2] : "unknown";
        this.#set({ state: "downloaded", channel, version });
      });
      this.#updater.on("error", (error: Error) => {
        if (this.#installing) {
          // The install failed: do not try again on every quit (ADP-004, DOMAIN.md §13.6 G).
          this.#installing = false;
          this.setInstallOnQuit(false);
        }
        this.#set({ state: "error", channel, message: error.message });
      });
    }
  }

  get channel(): UpdateChannel {
    return this.#channel;
  }

  status(): UpdateStatus {
    return this.#status;
  }

  onStatus(listener: (status: UpdateStatus) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  checkForUpdates(): UpdateStatus {
    if (this.#updater) {
      this.#updater.checkForUpdates();
    }
    return this.#status;
  }

  /** Whether a downloaded update installs when the app quits (the person's choice; a failed install turns it off). */
  get installOnQuit(): boolean {
    return this.#installOnQuit;
  }

  setInstallOnQuit(enabled: boolean): void {
    if (enabled !== this.#installOnQuit) {
      this.#installOnQuit = enabled;
      for (const listener of this.#installOnQuitListeners) {
        listener(enabled);
      }
    }
  }

  /** Called when install-on-quit changes, including when a failed install turns it off. */
  onInstallOnQuit(listener: (enabled: boolean) => void): () => void {
    this.#installOnQuitListeners.add(listener);
    return () => this.#installOnQuitListeners.delete(listener);
  }

  /** Whether an update is downloaded and ready to apply. */
  get downloaded(): boolean {
    return this.#status.state === "downloaded" && this.#updater !== null;
  }

  /**
   * Apply a downloaded update; refused unless one is actually downloaded. This only restarts the app: whether
   * it may, while work is running, is `UpdateGate`'s decision, and nothing else calls this.
   */
  install(): boolean {
    if (this.#status.state !== "downloaded" || !this.#updater) {
      return false;
    }
    this.#installing = true;
    try {
      this.#updater.quitAndInstall();
    } catch (error) {
      this.#installing = false;
      this.setInstallOnQuit(false);
      this.#set({ state: "error", channel: this.#channel, message: error instanceof Error ? error.message : String(error) });
      return false;
    }
    return true;
  }

  #set(status: UpdateStatus): void {
    this.#status = status;
    for (const listener of this.#listeners) {
      listener(status);
    }
  }
}
