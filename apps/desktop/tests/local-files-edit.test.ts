/**
 * Edit-conflict-safe local writes (ADP-005, DOMAIN.md 13.7 D): a baseline captured at read, the new bytes written to a
 * temporary file, the target re-checked after that and before the atomic rename, a backup created exclusively.
 *
 * Real files. The race is real: a second writer changes the file from inside the one window the re-check closes (the
 * hook runs after the temporary file is complete and before the target is looked at again), with real `writeFileSync`.
 */

import { chmodSync, existsSync, readdirSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { RootRegistry } from "../src/main/local-files/roots.js";
import { EditSessions, readForEdit, saveWithBaseline, type Baseline, type EditHooks } from "../src/main/local-files/safe-edit.js";

import { link, put, scratch } from "./local-files-fixtures.js";

const A = 41;
const B = 42;

function rig(options: { maxBytes?: number; maxSessions?: number; hooks?: EditHooks } = {}) {
  const base = scratch();
  const folder = join(base, "project");
  const registry = new RootRegistry({ rootsMax: 8, entriesMax: 4096, listMax: 5000 }, () => undefined, scratch());
  const written: { absolute: string; baseline: Baseline }[] = [];
  const edits = new EditSessions({
    maxBytes: options.maxBytes ?? 1 << 20,
    maxSessions: options.maxSessions ?? 16,
    resolve: (window, entryId) => registry.resolve(window, entryId),
    onWritten: (absolute, baseline) => written.push({ absolute, baseline }),
    ...(options.hooks === undefined ? {} : { hooks: options.hooks }),
  });
  const open = (window: number, name: string) => {
    const root = registry.grant(window, folder);
    if (!root.ok) {
      throw new Error("root refused");
    }
    const listing = registry.list(window, root.result.rootId, null);
    const entry = listing.ok ? listing.result.entries.find((candidate) => candidate.name === name) : undefined;
    if (entry === undefined) {
      throw new Error(`no entry ${name}`);
    }
    return { entryId: entry.entryId, rootId: root.result.rootId, opened: edits.open(window, entry.entryId) };
  };
  return { base, folder, registry, edits, written, open };
}

const names = (folder: string): string[] => readdirSync(folder).sort();

describe("a save that nothing disturbed", () => {
  it("replaces the file atomically, keeps its permissions, and the new stat is the next baseline", () => {
    const r = rig();
    const path = put(r.folder, "notes.txt", "version one\n");
    chmodSync(path, 0o640);
    const { opened } = r.open(A, "notes.txt");
    if (!opened.ok) {
      throw new Error(JSON.stringify(opened));
    }
    expect(opened.result).toMatchObject({ text: "version one\n", sizeBytes: 12 });
    expect(opened.result.editId).toMatch(/^edit_[0-9a-f]{32}$/);

    const saved = r.edits.save(A, opened.result.editId, "version two, longer\n", "fail");
    expect(saved.ok).toBe(true);
    expect(readFileSync(path, "utf8")).toBe("version two, longer\n");
    expect(statSync(path).mode & 0o777).toBe(0o640);
    expect(names(r.folder)).toEqual(["notes.txt"]);
    // Saving again from the same session is not a conflict with our own write.
    expect(r.edits.save(A, opened.result.editId, "version three\n", "fail").ok).toBe(true);
    expect(readFileSync(path, "utf8")).toBe("version three\n");
    // The watcher is told about each write so the change it causes is not reported back.
    expect(r.written.map((write) => write.absolute)).toEqual([path, path]);
    expect(r.written[1]?.baseline.sizeBytes).toBe(14);
  });
});

describe("a file that changed after it was read", () => {
  it("is a conflict that names both versions' metadata and writes nothing", () => {
    const r = rig();
    const path = put(r.folder, "notes.txt", "version one\n");
    const { opened } = r.open(A, "notes.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    // Someone else edits it after the read.
    writeFileSync(path, "edited by someone else, and longer\n");
    utimesSync(path, new Date(Date.now() + 5000), new Date(Date.now() + 5000));
    const answer = r.edits.save(A, opened.result.editId, "my version\n", "fail");
    expect(answer).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT", retryable: false } });
    const details = (answer as unknown as { error: { details: { baseline: { size_bytes: number; mtime_ns: string }; current: { size_bytes: number; mtime_ns: string } | null } } }).error.details;
    expect(details.baseline.size_bytes).toBe(12);
    expect(details.current?.size_bytes).toBe(35);
    expect(details.baseline.mtime_ns).toMatch(/^\d+$/);
    expect(details.current?.mtime_ns).not.toBe(details.baseline.mtime_ns);
    expect(readFileSync(path, "utf8")).toBe("edited by someone else, and longer\n");
    expect(names(r.folder)).toEqual(["notes.txt"]);
    expect(r.written).toEqual([]);
  });

  it("is a conflict when only the modification time changed", () => {
    const r = rig();
    const path = put(r.folder, "t.txt", "same size");
    const { opened } = r.open(A, "t.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    utimesSync(path, new Date(1_000_000_000_000), new Date(1_000_000_000_000));
    expect(r.edits.save(A, opened.result.editId, "new bytes", "fail")).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT" } });
    expect(readFileSync(path, "utf8")).toBe("same size");
  });

  it("is a conflict when the file was deleted, reported with no current version", () => {
    const r = rig();
    const path = put(r.folder, "gone.txt", "x");
    const { opened } = r.open(A, "gone.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    writeFileSync(join(r.folder, "other.txt"), "keeps the folder");
    rmFile(path);
    const answer = r.edits.save(A, opened.result.editId, "y", "fail");
    expect(answer).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT", details: { current: null } } });
    expect(existsSync(path), "a deleted file is not recreated by a refused save").toBe(false);
  });

  it("is caught when the other writer lands after the temporary file is written and before the target is re-checked", () => {
    // The whole point of re-checking after the write: an edit made while the new bytes were being written.
    let target = "";
    const r = rig({
      hooks: {
        afterTemporary: (path) => {
          target = path;
          writeFileSync(path, "the other writer got there first, in the window\n");
        },
      },
    });
    const path = put(r.folder, "race.txt", "base\n");
    const { opened } = r.open(A, "race.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    const answer = r.edits.save(A, opened.result.editId, "mine\n", "fail");
    expect(answer).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT" } });
    expect(target).toBe(path);
    // The other writer's bytes survive, and the temporary file is gone: nothing of ours is left in the folder.
    expect(readFileSync(path, "utf8")).toBe("the other writer got there first, in the window\n");
    expect(names(r.folder)).toEqual(["race.txt"]);
    expect(r.written).toEqual([]);
  });

  it("is refused, not followed, when the file became a link", () => {
    const r = rig();
    const path = put(r.folder, "ln.txt", "x");
    const { opened } = r.open(A, "ln.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    const outside = put(r.base, "outside.txt", "outside");
    rmFile(path);
    link(outside, path);
    const answer = r.edits.save(A, opened.result.editId, "y", "fail");
    expect(answer.ok).toBe(false);
    expect(readFileSync(outside, "utf8")).toBe("outside");
  });
});

describe("overwrite after a conflict", () => {
  it("keeps the version on disk as an exclusive backup, then writes", () => {
    const r = rig();
    const path = put(r.folder, "doc.txt", "base\n");
    const { opened } = r.open(A, "doc.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    writeFileSync(path, "the other version\n");
    expect(r.edits.save(A, opened.result.editId, "mine\n", "fail")).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT" } });
    const saved = r.edits.save(A, opened.result.editId, "mine\n", "overwrite");
    expect(saved).toMatchObject({ ok: true, result: { backup: "doc.txt.quansio-backup-1" } });
    expect(readFileSync(path, "utf8")).toBe("mine\n");
    expect(readFileSync(join(r.folder, "doc.txt.quansio-backup-1"), "utf8")).toBe("the other version\n");
  });

  it("never replaces a backup that already exists", () => {
    const r = rig();
    const path = put(r.folder, "doc.txt", "base\n");
    put(r.folder, "doc.txt.quansio-backup-1", "an earlier backup\n");
    put(r.folder, "doc.txt.quansio-backup-2", "another earlier backup\n");
    const { opened } = r.open(A, "doc.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    writeFileSync(path, "the version to keep\n");
    expect(r.edits.save(A, opened.result.editId, "mine\n", "overwrite")).toMatchObject({ ok: true, result: { backup: "doc.txt.quansio-backup-3" } });
    expect(readFileSync(join(r.folder, "doc.txt.quansio-backup-1"), "utf8")).toBe("an earlier backup\n");
    expect(readFileSync(join(r.folder, "doc.txt.quansio-backup-2"), "utf8")).toBe("another earlier backup\n");
    expect(readFileSync(join(r.folder, "doc.txt.quansio-backup-3"), "utf8")).toBe("the version to keep\n");
  });

  it("makes no backup when nothing conflicted, and an overwrite is itself checked against a third writer", () => {
    const quiet = rig();
    const path = put(quiet.folder, "q.txt", "base\n");
    const first = quiet.open(A, "q.txt").opened;
    if (!first.ok) {
      throw new Error("not opened");
    }
    expect(quiet.edits.save(A, first.result.editId, "mine\n", "overwrite")).toMatchObject({ ok: true, result: { backup: null } });
    expect(names(quiet.folder)).toEqual(["q.txt"]);
    expect(readFileSync(path, "utf8")).toBe("mine\n");

    // A third writer changes the file after the backup is taken and before the rename: the overwrite is a conflict too.
    let fired = false;
    const racy = rig({
      hooks: {
        afterTemporary: (target) => {
          if (!fired) {
            fired = true;
            writeFileSync(target, "a third writer\n");
          }
        },
      },
    });
    const racePath = put(racy.folder, "r.txt", "base\n");
    const second = racy.open(A, "r.txt").opened;
    if (!second.ok) {
      throw new Error("not opened");
    }
    writeFileSync(racePath, "the second writer\n");
    const answer = racy.edits.save(A, second.result.editId, "mine\n", "overwrite");
    expect(answer).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT" } });
    expect(readFileSync(racePath, "utf8")).toBe("a third writer\n");
    expect(readFileSync(join(racy.folder, "r.txt.quansio-backup-1"), "utf8")).toBe("the second writer\n");
  });
});

describe("what can be opened and saved", () => {
  it("opens a UTF-8 text file within the bound, and refuses binary, oversized and non-file entries", () => {
    const r = rig({ maxBytes: 64 });
    put(r.folder, "ok.txt", "héllo ✓");
    put(r.folder, "binary.bin", Buffer.from([0x68, 0x00, 0x69]));
    put(r.folder, "latin1.txt", Buffer.from([0x68, 0xe9, 0x69]));
    put(r.folder, "big.txt", "x".repeat(65));
    put(r.folder, "sub/inner.txt", "x");
    expect(r.open(A, "ok.txt").opened).toMatchObject({ ok: true, result: { text: "héllo ✓" } });
    for (const [name, reason] of [["binary.bin", "not_text"], ["latin1.txt", "not_text"], ["big.txt", "too_large"], ["sub", "not_regular"]] as const) {
      expect(r.open(A, name).opened, name).toMatchObject({ ok: false, error: { code: "FILE_REFUSED", details: { reason } } });
    }
  });

  it("refuses to save more than the bound or text with a NUL, and only a window's own sessions", () => {
    const r = rig({ maxBytes: 16 });
    put(r.folder, "a.txt", "x");
    const { opened } = r.open(A, "a.txt");
    if (!opened.ok) {
      throw new Error("not opened");
    }
    expect(r.edits.save(A, opened.result.editId, "y".repeat(17), "fail")).toMatchObject({ ok: false, error: { details: { reason: "too_large" } } });
    expect(r.edits.save(A, opened.result.editId, "a\0b", "fail")).toMatchObject({ ok: false, error: { details: { reason: "not_text" } } });
    expect(r.edits.save(B, opened.result.editId, "z", "fail")).toMatchObject({ ok: false, error: { code: "NOT_FOUND" } });
    expect(r.edits.close(B, opened.result.editId)).toBe(false);
    expect(r.edits.close(A, opened.result.editId)).toBe(true);
    expect(r.edits.save(A, opened.result.editId, "z", "fail")).toMatchObject({ ok: false, error: { code: "NOT_FOUND" } });
  });

  it("limits the sessions a window holds, and ends them with the window or its root", () => {
    const r = rig({ maxSessions: 2 });
    for (const name of ["1.txt", "2.txt", "3.txt"]) {
      put(r.folder, name, name);
    }
    const one = r.open(A, "1.txt");
    const two = r.open(A, "2.txt");
    expect(one.opened.ok && two.opened.ok).toBe(true);
    expect(r.open(A, "3.txt").opened).toMatchObject({ ok: false, error: { details: { reason: "limit" } } });
    expect(r.edits.count(A)).toBe(2);
    r.edits.revokeRoot(one.rootId);
    expect(r.edits.count(A)).toBe(0);
    const again = r.open(A, "3.txt").opened;
    expect(again.ok).toBe(true);
    r.edits.revokeWindow(A);
    expect(r.edits.count()).toBe(0);
  });

  it("an entry of another window's root is outside, and a file replaced by a link before the open is refused", () => {
    const r = rig();
    const path = put(r.folder, "a.txt", "x");
    const mine = r.open(A, "a.txt");
    expect(r.edits.open(B, mine.entryId)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    // Replaced by a link before the next open.
    const outside = put(r.base, "outside.txt", "outside");
    rmFile(path);
    link(outside, path);
    expect(r.edits.open(A, mine.entryId)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
  });
});

describe("saveWithBaseline and readForEdit directly", () => {
  it("compares size and modification time, and never writes to a target that is a directory", () => {
    const dir = scratch();
    const path = put(dir, "f.txt", "hello");
    const read = readForEdit(path, 100);
    if (!read.ok) {
      throw new Error("not readable");
    }
    expect(read.baseline.sizeBytes).toBe(5);
    expect(saveWithBaseline(path, Buffer.from("HELLO"), read.baseline, "fail")).toMatchObject({ ok: true });
    // The old baseline is stale now (same size, new time): a second save from it is a conflict.
    expect(saveWithBaseline(path, Buffer.from("again"), read.baseline, "fail")).toMatchObject({ ok: false, kind: "conflict" });
    expect(saveWithBaseline(dir, Buffer.from("x"), read.baseline, "fail")).toMatchObject({ ok: false, kind: "refused", reason: "not_regular" });
    // A target that is a link is refused as one and the file it points at is never written, whatever its size and time.
    const target = put(dir, "target.txt", "hello");
    const linked = link(target, join(dir, "linked.txt"));
    const targetRead = readForEdit(target, 100);
    if (!targetRead.ok) {
      throw new Error("not readable");
    }
    expect(saveWithBaseline(linked, Buffer.from("HELLO"), targetRead.baseline, "fail")).toMatchObject({ ok: false, kind: "refused", reason: "symlink" });
    expect(readFileSync(target, "utf8")).toBe("hello");
    expect(readForEdit(join(dir, "missing"), 100)).toMatchObject({ ok: false });
  });
});

function rmFile(path: string): void {
  rmSync(path);
}
