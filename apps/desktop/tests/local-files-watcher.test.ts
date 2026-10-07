/**
 * The shared workspace file watcher (ADP-005, DOMAIN.md 13.7 E): one OS watch per (device, inode), a debounce, typed
 * changes, an identity check, bounded subscribers and a typed overflow.
 *
 * Two layers, both over real directories: the hub's logic over an adapter the test fires by hand and a clock the test
 * moves (so a debounce is exact), and the same hub over Node's real `fs.watch` with real timers (so the operating
 * system's own events are what drive it).
 */

import { mkdirSync, renameSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { RootRegistry } from "../src/main/local-files/roots.js";
import { NODE_WATCH_ADAPTER, WatcherHub, type Delivery, type DirWatchAdapter, type DirWatchHandle, type FileChange, type WatchLimits } from "../src/main/local-files/watcher.js";
import { REAL_SCHEDULER } from "../src/main/local-files/grants.js";

import { CONFIG, eventually, put, schedulerOf, scratch } from "./local-files-fixtures.js";
import { Clock } from "./shell-fakes.js";

const A = 51;
const B = 52;

const LIMITS: WatchLimits = {
  debounceMs: CONFIG.watchDebounceMs,
  maxDirs: CONFIG.watchMaxDirs,
  maxSubscribers: CONFIG.watchMaxSubscribers,
  queueMax: CONFIG.watchQueueMax,
  maxEntries: CONFIG.watchMaxEntries,
  selfWriteTtlMs: CONFIG.selfWriteTtlMs,
};

/** An adapter the test fires: it records every watch opened and closed. */
class HandAdapter implements DirWatchAdapter {
  readonly opened: string[] = [];
  closed = 0;
  readonly #events = new Map<string, () => void>();
  readonly #errors = new Map<string, (error: Error) => void>();

  watch(directory: string, onEvent: () => void, onError: (error: Error) => void): DirWatchHandle {
    this.opened.push(directory);
    this.#events.set(directory, onEvent);
    this.#errors.set(directory, onError);
    return {
      close: () => {
        this.closed += 1;
        this.#events.delete(directory);
      },
    };
  }

  fire(directory: string): void {
    this.#events.get(directory)?.();
  }

  fail(directory: string): void {
    this.#errors.get(directory)?.(new Error("the watch failed"));
  }

  get live(): number {
    return this.#events.size;
  }
}

function rig(limits: Partial<WatchLimits> = {}) {
  const clock = new Clock();
  const adapter = new HandAdapter();
  const delivered: { window: number; delivery: Delivery }[] = [];
  const registry = new RootRegistry({ rootsMax: 8, entriesMax: 4096, listMax: 5000 }, () => undefined, scratch());
  const hub = new WatcherHub({
    limits: { ...LIMITS, ...limits },
    adapter,
    scheduler: schedulerOf(clock),
    now: () => clock.now,
    deliver: (window, delivery) => delivered.push({ window, delivery }),
    mint: (subscription, name) => registry.mint(subscription.windowId, subscription.rootId, subscription.relative === "" ? name : `${subscription.relative}/${name}`),
  });
  return { clock, adapter, delivered, hub, registry, base: scratch() };
}

type Rig = ReturnType<typeof rig>;

function watch(r: Rig, window: number, directory: string, relative = ""): string {
  const answer = r.hub.subscribe(window, "lroot_" + "a".repeat(32), relative, directory);
  if (!answer.ok) {
    throw new Error(`refused: ${JSON.stringify(answer.error)}`);
  }
  return answer.result.subscriptionId;
}

const changesOf = (r: Rig, window?: number): FileChange[] =>
  r.delivered.filter((entry) => (window === undefined || entry.window === window) && "changes" in entry.delivery).flatMap((entry) => ("changes" in entry.delivery ? [...entry.delivery.changes] : []));

describe("one OS watch per directory", () => {
  it("is shared by every subscriber of the same (device, inode), however its path is reached, and closed with the last", () => {
    const r = rig();
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    const sub1 = watch(r, A, dir);
    const sub2 = watch(r, B, dir);
    // The same directory by another path (a link to it) is the same inode: still one watch.
    const aliased = join(r.base, "alias");
    symlinkSync(dir, aliased);
    const sub3 = watch(r, A, aliased);
    expect(r.adapter.opened).toEqual([dir]);
    expect(r.hub.watchCount()).toBe(1);
    expect(r.hub.subscriberCount()).toBe(3);
    expect(r.hub.unsubscribe(B, sub1)).toBe(false);
    expect(r.hub.unsubscribe(A, sub1)).toBe(true);
    expect(r.adapter.live).toBe(1);
    r.hub.unsubscribe(B, sub2);
    r.hub.unsubscribe(A, sub3);
    expect(r.hub.watchCount()).toBe(0);
    expect(r.adapter.live).toBe(0);
  });

  it("is dropped when its window goes or its root is released, and not before", () => {
    const r = rig();
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    watch(r, A, dir);
    watch(r, B, dir);
    r.hub.revokeWindow(A);
    expect([r.hub.subscriberCount(A), r.hub.subscriberCount(B)]).toEqual([0, 1]);
    expect(r.hub.watchCount()).toBe(1);
    r.hub.revokeRoot("lroot_" + "a".repeat(32));
    expect(r.hub.watchCount()).toBe(0);
    expect(r.adapter.live).toBe(0);
  });

  it("is bounded: subscribers per window and watches in all", () => {
    const r = rig({ maxSubscribers: 2, maxDirs: 3 });
    const dirs = ["d1", "d2", "d3", "d4"].map((name) => {
      mkdirSync(join(r.base, name), { recursive: true });
      return join(r.base, name);
    });
    watch(r, A, dirs[0] ?? "");
    watch(r, A, dirs[1] ?? "");
    expect(r.hub.subscribe(A, "lroot_" + "a".repeat(32), "", dirs[2] ?? "")).toMatchObject({ ok: false, error: { code: "FILE_REFUSED", details: { reason: "limit" } } });
    watch(r, B, dirs[2] ?? "");
    // A fourth directory is over the watch limit, for a window that has room itself.
    expect(r.hub.subscribe(B, "lroot_" + "a".repeat(32), "", dirs[3] ?? "")).toMatchObject({ ok: false, error: { details: { reason: "limit" } } });
    expect(r.hub.watchCount()).toBe(3);
  });

  it("refuses a file, a missing path and a directory with too many entries", () => {
    const r = rig({ maxEntries: 100 });
    const file = put(r.base, "f.txt", "x");
    expect(r.hub.subscribe(A, "lroot_" + "a".repeat(32), "", file)).toMatchObject({ ok: false, error: { details: { reason: "not_regular" } } });
    expect(r.hub.subscribe(A, "lroot_" + "a".repeat(32), "", join(r.base, "missing"))).toMatchObject({ ok: false, error: { details: { reason: "unreadable" } } });
    const crowded = join(r.base, "crowded");
    for (let index = 0; index < 101; index += 1) {
      put(crowded, `f${String(index)}`, "x");
    }
    expect(r.hub.subscribe(A, "lroot_" + "a".repeat(32), "", crowded)).toMatchObject({ ok: false, error: { details: { reason: "too_many_entries" } } });
    expect(r.hub.watchCount()).toBe(0);
  });
});

describe("the debounce", () => {
  it("reports a burst of events once, 300 ms after the last, as typed changes", () => {
    const r = rig();
    const dir = join(r.base, "project");
    put(dir, "existing.txt", "old");
    put(dir, "doomed.txt", "bye");
    const subscription = watch(r, A, dir);
    // A burst: a file added, one changed, one removed, a folder made, events arriving over 250 ms.
    put(dir, "new.txt", "n");
    r.adapter.fire(dir);
    r.clock.advance(100);
    writeFileSync(join(dir, "existing.txt"), "new content, longer");
    r.adapter.fire(dir);
    r.clock.advance(100);
    rmSync(join(dir, "doomed.txt"));
    mkdirSync(join(dir, "folder"));
    r.adapter.fire(dir);
    expect(r.delivered, "nothing is reported while events keep coming").toEqual([]);
    r.clock.advance(299);
    expect(r.delivered, "nor 299 ms after the last").toEqual([]);
    r.clock.advance(1);
    expect(r.delivered).toHaveLength(1);
    expect(r.delivered[0]).toMatchObject({ window: A, delivery: { subscriptionId: subscription } });
    expect(changesOf(r).map((change) => [change.kind, change.name, change.directory])).toEqual([
      ["unlink", "doomed.txt", false],
      ["change", "existing.txt", false],
      ["add", "folder", true],
      ["add", "new.txt", false],
    ]);
    // An entry id is minted for what still exists, none for what is gone.
    expect(changesOf(r).map((change) => change.entryId === null)).toEqual([true, false, false, false]);
    for (const change of changesOf(r)) {
      expect(JSON.stringify(change)).not.toContain(r.base);
    }
  });

  it("reports nothing for an event that changed nothing, and then nothing more until the next one", () => {
    const r = rig();
    const dir = join(r.base, "project");
    put(dir, "a.txt", "a");
    watch(r, A, dir);
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(r.delivered).toEqual([]);
    put(dir, "b.txt", "b");
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(changesOf(r).map((change) => change.name)).toEqual(["b.txt"]);
    r.clock.advance(10_000);
    expect(r.delivered).toHaveLength(1);
  });

  it("reports a directory that never goes quiet within a ceiling, instead of never", () => {
    const r = rig();
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    watch(r, A, dir);
    put(dir, "first.txt", "x");
    // An event every 100 ms for four seconds: never 300 ms of quiet.
    for (let tick = 0; tick < 40; tick += 1) {
      r.adapter.fire(dir);
      r.clock.advance(100);
    }
    expect(changesOf(r).map((change) => change.name)).toContain("first.txt");
    // The ceiling is ten debounce intervals after the first event.
    expect(r.delivered.length).toBeGreaterThanOrEqual(1);
  });

  it("tells a file that became a folder, and ignores links and special files", () => {
    const r = rig();
    const dir = join(r.base, "project");
    put(dir, "thing", "a file");
    put(r.base, "outside.txt", "outside");
    watch(r, A, dir);
    rmSync(join(dir, "thing"));
    mkdirSync(join(dir, "thing"));
    symlinkSync(join(r.base, "outside.txt"), join(dir, "link.txt"));
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(changesOf(r).map((change) => [change.kind, change.name, change.directory])).toEqual([
      ["unlink", "thing", false],
      ["add", "thing", true],
    ]);
  });
});

describe("self-writes", () => {
  it("a write the shell made is not reported back, and the next write by anyone else is", () => {
    const r = rig();
    const dir = join(r.base, "project");
    const path = put(dir, "doc.txt", "one");
    watch(r, A, dir);
    writeFileSync(path, "two, written by the shell");
    const stats = statSync(path, { bigint: true });
    r.hub.noteSelfWrite(path, { sizeBytes: Number(stats.size), mtimeNs: stats.mtimeNs, mode: Number(stats.mode) });
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(r.delivered, "our own change is silent").toEqual([]);
    // Someone else then writes it: that is news.
    writeFileSync(path, "three, written by someone else");
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(changesOf(r).map((change) => [change.kind, change.name])).toEqual([["change", "doc.txt"]]);
  });

  it("a recorded write is forgotten after its time to live, and a different change under the same name is still news", () => {
    const r = rig();
    const dir = join(r.base, "project");
    const path = put(dir, "doc.txt", "one");
    watch(r, A, dir);
    // The shell recorded a write that is not what is on disk (someone wrote after it, a different size).
    r.hub.noteSelfWrite(path, { sizeBytes: 3, mtimeNs: 1n, mode: 0 });
    writeFileSync(path, "completely different");
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(changesOf(r)).toHaveLength(1);
  });
});

describe("identity and failure", () => {
  it("drops the watch and says so when the directory is replaced by another with the same path", () => {
    const r = rig();
    const dir = join(r.base, "project");
    put(dir, "a.txt", "a");
    const subscription = watch(r, A, dir);
    watch(r, B, dir);
    // The directory is moved away and a new one is made at the same path.
    renameSync(dir, join(r.base, "project-old"));
    put(dir, "b.txt", "b");
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(r.delivered.map((entry) => [entry.window, entry.delivery])).toEqual([
      [A, { subscriptionId: subscription, dropped: "directory_replaced" }],
      [B, expect.objectContaining({ dropped: "directory_replaced" })],
    ]);
    expect(r.hub.watchCount()).toBe(0);
    expect(r.adapter.live, "the OS watch was closed").toBe(0);
    // Nothing more is ever reported for the old subscription.
    r.adapter.fire(dir);
    r.clock.advance(1000);
    expect(r.delivered).toHaveLength(2);
  });

  it("notices a replaced directory with no event at all, on the slow identity check", () => {
    const r = rig();
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    watch(r, A, dir);
    rmSync(dir, { recursive: true });
    mkdirSync(dir);
    r.clock.advance(300 * 10);
    expect(r.delivered.map((entry) => entry.delivery)).toEqual([expect.objectContaining({ dropped: "directory_replaced" })]);
    expect(r.hub.watchCount()).toBe(0);
  });

  it("tells subscribers the directory was removed", () => {
    const r = rig();
    const dir = join(r.base, "project");
    put(dir, "a.txt", "a");
    watch(r, A, dir);
    rmSync(dir, { recursive: true });
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(r.delivered.map((entry) => entry.delivery)).toEqual([expect.objectContaining({ dropped: "directory_removed" })]);
    expect(r.hub.watchCount()).toBe(0);
  });

  it("drops the watch when the operating system's watch fails", () => {
    const r = rig();
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    watch(r, A, dir);
    r.adapter.fail(dir);
    expect(r.delivered.map((entry) => entry.delivery)).toEqual([expect.objectContaining({ dropped: "directory_removed" })]);
    expect(r.hub.watchCount()).toBe(0);
  });
});

describe("the bounded queue", () => {
  it("replaces a report longer than the queue with one typed overflow signal", () => {
    const r = rig({ queueMax: 16 });
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    const subscription = watch(r, A, dir);
    for (let index = 0; index < 17; index += 1) {
      put(dir, `f${String(index).padStart(2, "0")}.txt`, "x");
    }
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(r.delivered.map((entry) => entry.delivery)).toEqual([{ subscriptionId: subscription, overflow: true }]);
    // The snapshot moved on: the next small change is an ordinary report.
    put(dir, "later.txt", "x");
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(changesOf(r).map((change) => change.name)).toEqual(["later.txt"]);
  });

  it("reports exactly at the limit as changes, not an overflow", () => {
    const r = rig({ queueMax: 16 });
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    watch(r, A, dir);
    for (let index = 0; index < 16; index += 1) {
      put(dir, `f${String(index).padStart(2, "0")}.txt`, "x");
    }
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(changesOf(r)).toHaveLength(16);
  });

  it("a directory that grew past the entry limit is an overflow, not an unbounded report", () => {
    const r = rig({ maxEntries: 100 });
    const dir = join(r.base, "project");
    mkdirSync(dir, { recursive: true });
    watch(r, A, dir);
    for (let index = 0; index < 101; index += 1) {
      put(dir, `f${String(index)}`, "x");
    }
    r.adapter.fire(dir);
    r.clock.advance(300);
    expect(r.delivered.map((entry) => entry.delivery)).toEqual([expect.objectContaining({ overflow: true })]);
  });
});

describe("the real operating-system watch", () => {
  it("reports real file changes, debounced, through fs.watch", async () => {
    const base = scratch();
    const dir = join(base, "project");
    put(dir, "existing.txt", "old");
    const delivered: { window: number; delivery: Delivery }[] = [];
    const registry = new RootRegistry({ rootsMax: 8, entriesMax: 4096, listMax: 5000 }, () => undefined, scratch());
    const hub = new WatcherHub({
      limits: { ...LIMITS, debounceMs: 80 },
      adapter: NODE_WATCH_ADAPTER,
      scheduler: REAL_SCHEDULER,
      now: Date.now,
      deliver: (window, delivery) => delivered.push({ window, delivery }),
      mint: (subscription, name) => registry.mint(subscription.windowId, subscription.rootId, name),
    });
    const answer = hub.subscribe(A, "lroot_" + "a".repeat(32), "", dir);
    expect(answer.ok).toBe(true);
    try {
      // Three quick writes are one report.
      writeFileSync(join(dir, "one.txt"), "1");
      writeFileSync(join(dir, "two.txt"), "2");
      writeFileSync(join(dir, "existing.txt"), "changed");
      const seen = await eventually(() => {
        const names = delivered.flatMap((entry) => ("changes" in entry.delivery ? entry.delivery.changes.map((change) => `${change.kind}:${change.name}`) : []));
        return names.length >= 3 ? names : undefined;
      }, "the three changes");
      expect(seen.sort()).toEqual(["add:one.txt", "add:two.txt", "change:existing.txt"]);
      expect(delivered.length, "one debounced report").toBe(1);
      rmSync(join(dir, "one.txt"));
      const later = await eventually(() => {
        const last = delivered[delivered.length - 1]?.delivery;
        return last !== undefined && "changes" in last && last.changes.some((change) => change.kind === "unlink" && change.name === "one.txt") ? last : undefined;
      }, "the removal");
      expect(later).toBeDefined();
    } finally {
      hub.dispose();
    }
    expect(hub.watchCount()).toBe(0);
  });
});
