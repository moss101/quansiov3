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

  constructor(channel: UpdateChannel, updater: AutoUpdaterLike | null, feedUrl: string, signed: boolean) {
    this.#channel = channel;
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

  /** Apply a downloaded update; refused unless one is actually downloaded. */
  install(): boolean {
    if (this.#status.state !== "downloaded" || !this.#updater) {
      return false;
    }
    this.#updater.quitAndInstall();
    return true;
  }

  #set(status: UpdateStatus): void {
    this.#status = status;
    for (const listener of this.#listeners) {
      listener(status);
    }
  }
}
