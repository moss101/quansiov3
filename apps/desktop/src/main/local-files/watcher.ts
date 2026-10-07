/**
 * The shared workspace file watcher (ADP-005, DOMAIN.md §13.7 E).
 *
 * A page that shows a folder wants to know when it changes, and nothing more: no path, no file system access, no
 * stream of operating-system events. The hub keeps **one operating-system watch per directory** (keyed by the
 * directory's `(device, inode)`, not its path, so two roots that reach the same directory share it) however many
 * windows subscribe, and turns what the OS says into typed changes:
 *
 * - **Debounced.** Raw events only mark a directory dirty. Its listing is compared with the last one reported
 *   `debounceMs` after the *last* event (with a ceiling, so a directory that never goes quiet is still reported),
 *   and the difference is the change list: `add`, `change` or `unlink`, each for a name in that directory.
 *   Comparing listings, not trusting event names, is what makes a burst one report and a no-op write none.
 * - **Confined.** A subscription is made only from a directory the root registry resolved for that window, so there
 *   is no way to name a path outside the granted roots; the hub never takes a path from a page.
 * - **Identity-checked.** Each flush and a slow timer compare the directory's `(device, inode)` with the one watched.
 *   A directory that was replaced or removed drops its watch and tells its subscribers, typed, so a page never goes on
 *   listening to something that is no longer there.
 * - **Quiet about itself.** A write the shell made through the safe-edit path is recorded with its resulting size and
 *   modification time; the change it causes is recognised and not reported.
 * - **Bounded.** At most `maxDirs` watches, `maxSubscribers` subscriptions per window, `maxEntries` entries per
 *   watched directory, and at most `queueMax` changes in one report: past it the report is one typed
 *   `{overflow: true}` and the page re-lists.
 *
 * The operating-system watch itself is an adapter (`DirWatchAdapter`); tests run the hub's logic over a fake one and
 * over the real `fs.watch` against real directories.
 */

import { randomBytes } from "node:crypto";
import { lstatSync, readdirSync, statSync, watch as fsWatch } from "node:fs";
import { join } from "node:path";

import { done, refusedFile, type Answer } from "./results.js";
import type { Scheduler } from "./grants.js";
import type { Baseline } from "./safe-edit.js";

export interface WatchLimits {
  readonly debounceMs: number;
  readonly maxDirs: number;
  readonly maxSubscribers: number;
  readonly queueMax: number;
  readonly maxEntries: number;
  readonly selfWriteTtlMs: number;
}

/** One OS watch on one directory. */
export interface DirWatchHandle {
  close(): void;
}

export interface DirWatchAdapter {
  /** Watch `directory`; `onEvent` is called for anything the OS reports (its detail is not trusted), `onError` if the watch fails. */
  watch(directory: string, onEvent: () => void, onError: (error: Error) => void): DirWatchHandle;
}

/** The real thing: Node's `fs.watch`, non-recursive, that does not keep the process alive. */
export const NODE_WATCH_ADAPTER: DirWatchAdapter = {
  watch(directory, onEvent, onError) {
    const watcher = fsWatch(directory, { persistent: false }, () => {
      onEvent();
    });
    watcher.on("error", onError);
    return {
      close: () => {
        watcher.close();
      },
    };
  },
};

export interface FileChange {
  readonly kind: "add" | "change" | "unlink";
  readonly name: string;
  readonly directory: boolean;
  /** An entry id for a file or directory that still exists (`null` for an `unlink`); minted for the subscriber's window. */
  readonly entryId: string | null;
}

/** What a subscriber is sent. */
export type Delivery =
  | { readonly subscriptionId: string; readonly changes: readonly FileChange[] }
  | { readonly subscriptionId: string; readonly overflow: true }
  | { readonly subscriptionId: string; readonly dropped: "directory_replaced" | "directory_removed" };

/** What one listed name looked like. */
interface Seen {
  readonly directory: boolean;
  readonly sizeBytes: number;
  readonly mtimeNs: bigint;
  readonly ino: bigint;
}

interface Subscription {
  readonly id: string;
  readonly windowId: number;
  readonly rootId: string;
  /** The directory's path relative to the root, for minting entry ids of what is inside it. */
  readonly relative: string;
}

interface DirWatch {
  readonly key: string;
  readonly absolute: string;
  readonly handle: DirWatchHandle;
  snapshot: Map<string, Seen>;
  readonly subscribers: Map<string, Subscription>;
  dirtySince: number | null;
  timer: unknown;
  identityTimer: unknown;
}

export interface WatcherDeps {
  readonly limits: WatchLimits;
  readonly adapter: DirWatchAdapter;
  readonly scheduler: Scheduler;
  readonly now: () => number;
  readonly deliver: (windowId: number, delivery: Delivery) => void;
  /** An entry id for `name` inside the watched directory, for the subscriber's window. */
  readonly mint: (subscription: { readonly windowId: number; readonly rootId: string; readonly relative: string }, name: string) => string;
}

const SUBSCRIPTION_ID = /^sub_[0-9a-f]{16}$/;

export function isSubscriptionId(value: string): boolean {
  return SUBSCRIPTION_ID.test(value);
}

/** The ceiling on how long a directory that keeps changing goes unreported, in debounce intervals. */
const MAX_WAIT_INTERVALS = 10;
/** How often a watch checks it is still on the directory it was made for, in debounce intervals. */
const IDENTITY_INTERVALS = 10;
/** The self-writes remembered at once. */
const SELF_WRITES_MAX = 256;

export class WatcherHub {
  readonly #deps: WatcherDeps;
  readonly #dirs = new Map<string, DirWatch>();
  readonly #selfWrites = new Map<string, { sizeBytes: number; mtimeNs: bigint; expiresAt: number }>();

  constructor(deps: WatcherDeps) {
    this.#deps = deps;
  }

  /** OS watches open now. */
  watchCount(): number {
    return this.#dirs.size;
  }

  /** Subscriptions alive now, for a window (all windows when omitted). */
  subscriberCount(windowId?: number): number {
    let count = 0;
    for (const dir of this.#dirs.values()) {
      for (const subscription of dir.subscribers.values()) {
        count += windowId === undefined || subscription.windowId === windowId ? 1 : 0;
      }
    }
    return count;
  }

  /** A shell write made the file at `absolute` this size and time: the change it causes is not news. */
  noteSelfWrite(absolute: string, baseline: Baseline): void {
    const now = this.#deps.now();
    for (const [path, write] of this.#selfWrites) {
      if (write.expiresAt <= now) {
        this.#selfWrites.delete(path);
      }
    }
    while (this.#selfWrites.size >= SELF_WRITES_MAX) {
      const oldest = this.#selfWrites.keys().next().value;
      if (oldest === undefined) {
        break;
      }
      this.#selfWrites.delete(oldest);
    }
    this.#selfWrites.set(absolute, { sizeBytes: baseline.sizeBytes, mtimeNs: baseline.mtimeNs, expiresAt: now + this.#deps.limits.selfWriteTtlMs });
  }

  /**
   * Subscribe `windowId` to the directory at `directory` (an absolute path a root registry resolved for that window).
   * One OS watch is shared by every subscriber of the same `(device, inode)`.
   */
  subscribe(windowId: number, rootId: string, relative: string, directory: string): Answer<{ readonly subscriptionId: string }> {
    const limits = this.#deps.limits;
    if (this.subscriberCount(windowId) >= limits.maxSubscribers) {
      return refusedFile("limit", "this window watches as many folders as it may; stop one first");
    }
    let key: string;
    try {
      const stats = statSync(directory, { bigint: true });
      if (!stats.isDirectory()) {
        return refusedFile("not_regular", "only a folder can be watched");
      }
      key = `${stats.dev.toString()}:${stats.ino.toString()}`;
    } catch {
      return refusedFile("unreadable", "that folder cannot be read");
    }
    let dir = this.#dirs.get(key);
    if (dir === undefined) {
      if (this.#dirs.size >= limits.maxDirs) {
        return refusedFile("limit", "too many folders are being watched; stop one first");
      }
      const scanned = this.#scan(directory);
      if (scanned === "too_many") {
        return refusedFile("too_many_entries", "that folder has too many entries to watch");
      }
      if (scanned === null) {
        return refusedFile("unreadable", "that folder cannot be read");
      }
      let handle: DirWatchHandle;
      try {
        handle = this.#deps.adapter.watch(
          directory,
          () => {
            this.#touched(key);
          },
          () => {
            this.#drop(key, "directory_removed");
          },
        );
      } catch {
        return refusedFile("unreadable", "that folder cannot be watched");
      }
      dir = { key, absolute: directory, handle, snapshot: scanned, subscribers: new Map(), dirtySince: null, timer: null, identityTimer: null };
      this.#dirs.set(key, dir);
      const watched = dir;
      const interval = limits.debounceMs * IDENTITY_INTERVALS;
      const arm = (): void => {
        watched.identityTimer = this.#deps.scheduler.set(() => {
          if (this.#dirs.get(key) === watched) {
            this.#verifyIdentity(watched);
            if (this.#dirs.get(key) === watched) {
              arm();
            }
          }
        }, interval);
      };
      arm();
    }
    const id = `sub_${randomBytes(8).toString("hex")}`;
    dir.subscribers.set(id, { id, windowId, rootId, relative });
    return done({ subscriptionId: id });
  }

  unsubscribe(windowId: number, subscriptionId: string): boolean {
    for (const dir of this.#dirs.values()) {
      const subscription = dir.subscribers.get(subscriptionId);
      if (subscription?.windowId === windowId) {
        dir.subscribers.delete(subscriptionId);
        this.#closeIfUnused(dir);
        return true;
      }
    }
    return false;
  }

  revokeWindow(windowId: number): void {
    this.#revokeWhere((subscription) => subscription.windowId === windowId);
  }

  revokeRoot(rootId: string): void {
    this.#revokeWhere((subscription) => subscription.rootId === rootId);
  }

  /** Close everything (the application is quitting). */
  dispose(): void {
    for (const key of [...this.#dirs.keys()]) {
      this.#close(key);
    }
  }

  #revokeWhere(test: (subscription: Subscription) => boolean): void {
    for (const dir of [...this.#dirs.values()]) {
      for (const [id, subscription] of [...dir.subscribers]) {
        if (test(subscription)) {
          dir.subscribers.delete(id);
        }
      }
      this.#closeIfUnused(dir);
    }
  }

  #closeIfUnused(dir: DirWatch): void {
    if (dir.subscribers.size === 0) {
      this.#close(dir.key);
    }
  }

  #close(key: string): void {
    const dir = this.#dirs.get(key);
    if (dir === undefined) {
      return;
    }
    this.#dirs.delete(key);
    this.#deps.scheduler.clear(dir.timer);
    this.#deps.scheduler.clear(dir.identityTimer);
    try {
      dir.handle.close();
    } catch {
      // Already closed by the OS.
    }
  }

  /** Tell every subscriber the directory is not what it was, and stop watching it. */
  #drop(key: string, reason: "directory_replaced" | "directory_removed"): void {
    const dir = this.#dirs.get(key);
    if (dir === undefined) {
      return;
    }
    this.#close(key);
    for (const subscription of dir.subscribers.values()) {
      this.#deps.deliver(subscription.windowId, { subscriptionId: subscription.id, dropped: reason });
    }
  }

  #verifyIdentity(dir: DirWatch): boolean {
    try {
      const stats = statSync(dir.absolute, { bigint: true });
      if (!stats.isDirectory() || `${stats.dev.toString()}:${stats.ino.toString()}` !== dir.key) {
        this.#drop(dir.key, "directory_replaced");
        return false;
      }
      return true;
    } catch {
      this.#drop(dir.key, "directory_removed");
      return false;
    }
  }

  /** The OS said something happened in a directory: mark it and (re)arm the debounce. */
  #touched(key: string): void {
    const dir = this.#dirs.get(key);
    if (dir === undefined) {
      return;
    }
    const now = this.#deps.now();
    dir.dirtySince ??= now;
    this.#deps.scheduler.clear(dir.timer);
    const limits = this.#deps.limits;
    // Quiet for the debounce interval, or the ceiling since the first event, whichever comes first.
    const ceiling = Math.max(0, dir.dirtySince + limits.debounceMs * MAX_WAIT_INTERVALS - now);
    dir.timer = this.#deps.scheduler.set(() => {
      this.#flush(key);
    }, Math.min(limits.debounceMs, ceiling));
  }

  /** `null` when the directory cannot be read, `"too_many"` past the entry limit, else what it holds. */
  #scan(directory: string): Map<string, Seen> | "too_many" | null {
    let names: string[];
    try {
      names = readdirSync(directory);
    } catch {
      return null;
    }
    if (names.length > this.#deps.limits.maxEntries) {
      return "too_many";
    }
    const seen = new Map<string, Seen>();
    for (const name of names) {
      try {
        const stats = lstatSync(join(directory, name), { bigint: true });
        if (stats.isSymbolicLink() || !(stats.isFile() || stats.isDirectory())) {
          continue;
        }
        seen.set(name, { directory: stats.isDirectory(), sizeBytes: Number(stats.size), mtimeNs: stats.mtimeNs, ino: stats.ino });
      } catch {
        // Gone between the listing and the stat: the next scan sees it as gone.
      }
    }
    return seen;
  }

  #flush(key: string): void {
    const dir = this.#dirs.get(key);
    if (dir === undefined) {
      return;
    }
    dir.dirtySince = null;
    dir.timer = null;
    if (!this.#verifyIdentity(dir)) {
      return;
    }
    const next = this.#scan(dir.absolute);
    if (next === null) {
      this.#drop(key, "directory_removed");
      return;
    }
    if (next === "too_many") {
      // A directory that grew past the limit is reported as an overflow, and the page re-lists.
      for (const subscription of dir.subscribers.values()) {
        this.#deps.deliver(subscription.windowId, { subscriptionId: subscription.id, overflow: true });
      }
      return;
    }
    const before = dir.snapshot;
    dir.snapshot = next;
    const changes: { kind: FileChange["kind"]; name: string; directory: boolean }[] = [];
    const now = this.#deps.now();
    for (const [name, seen] of next) {
      const old = before.get(name);
      if (old === undefined) {
        changes.push({ kind: "add", name, directory: seen.directory });
      } else if (old.directory !== seen.directory) {
        changes.push({ kind: "unlink", name, directory: old.directory }, { kind: "add", name, directory: seen.directory });
      } else if (!seen.directory && (old.sizeBytes !== seen.sizeBytes || old.mtimeNs !== seen.mtimeNs || old.ino !== seen.ino)) {
        changes.push({ kind: "change", name, directory: false });
      }
    }
    for (const [name, old] of before) {
      if (!next.has(name)) {
        changes.push({ kind: "unlink", name, directory: old.directory });
      }
    }
    // What the shell itself just wrote is not news.
    const reported = changes.filter((change) => {
      if (change.directory || change.kind === "unlink") {
        return true;
      }
      const own = this.#selfWrites.get(join(dir.absolute, change.name));
      const seen = next.get(change.name);
      return !(own !== undefined && own.expiresAt > now && seen !== undefined && own.sizeBytes === seen.sizeBytes && own.mtimeNs === seen.mtimeNs);
    });
    if (reported.length === 0) {
      return;
    }
    reported.sort((a, b) => (a.name === b.name ? 0 : a.name < b.name ? -1 : 1));
    for (const subscription of dir.subscribers.values()) {
      if (reported.length > this.#deps.limits.queueMax) {
        this.#deps.deliver(subscription.windowId, { subscriptionId: subscription.id, overflow: true });
        continue;
      }
      this.#deps.deliver(subscription.windowId, {
        subscriptionId: subscription.id,
        changes: reported.map((change) => ({
          kind: change.kind,
          name: change.name,
          directory: change.directory,
          entryId: change.kind === "unlink" ? null : this.#deps.mint(subscription, change.name),
        })),
      });
    }
  }
}
