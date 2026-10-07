/**
 * Granted roots and entry ids (ADP-005, DOMAIN.md 13.7 B): a path outside the granted roots is refused, however it
 * is reached, and no call takes a path at all.
 *
 * Real directories, real symbolic links. The tests make a link that leaves the root before the listing, after it, and
 * as a replacement for a listed file, and ask the registry to resolve the entry ids it handed out.
 */

import { renameSync, rmSync, symlinkSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { plainRelative, resolveInRoot } from "../src/main/local-files/fs-ops.js";
import { RootRegistry } from "../src/main/local-files/roots.js";

import { link, put, scratch } from "./local-files-fixtures.js";

const A = 21;
const B = 22;

function rig(limits: Partial<{ rootsMax: number; entriesMax: number; listMax: number }> = {}) {
  const home = scratch();
  const released: string[] = [];
  const registry = new RootRegistry({ rootsMax: 8, entriesMax: 4096, listMax: 5000, ...limits }, (_window, rootId) => released.push(rootId), home);
  return { registry, home, released, base: scratch() };
}

function granted(registry: RootRegistry, window: number, directory: string) {
  const answer = registry.grant(window, directory);
  if (!answer.ok) {
    throw new Error(`refused: ${JSON.stringify(answer.error)}`);
  }
  return answer.result;
}

function entries(registry: RootRegistry, window: number, rootId: string, entryId: string | null = null) {
  const answer = registry.list(window, rootId, entryId);
  if (!answer.ok) {
    throw new Error(`refused: ${JSON.stringify(answer.error)}`);
  }
  return answer.result;
}

describe("granting a root", () => {
  it("holds the folder for the window that picked it and tells the page its name, never its path", () => {
    const { registry, base } = rig();
    const project = join(base, "project");
    put(project, "a.txt", "a");
    const root = granted(registry, A, project);
    expect(root.name).toBe("project");
    expect(root.rootId).toMatch(/^lroot_[0-9a-f]{32}$/);
    expect(JSON.stringify(root)).not.toContain(base);
    expect(registry.rootsOf(A)).toEqual([root]);
    expect(registry.rootsOf(B)).toEqual([]);
    // The same folder again is the same root.
    expect(granted(registry, A, project)).toEqual(root);
  });

  it("refuses a file, a missing folder, the filesystem root and the home folder itself", () => {
    const { registry, base, home } = rig();
    const file = put(base, "file.txt", "x");
    for (const path of [file, join(base, "missing"), "/", home]) {
      const answer = registry.grant(A, path);
      expect(answer.ok, path).toBe(false);
      expect(answer).toMatchObject({ ok: false, error: { code: "FILE_REFUSED" } });
    }
    expect(registry.grant(A, home)).toMatchObject({ error: { details: { reason: "root_not_allowed" } } });
    expect(registry.grant(A, "/")).toMatchObject({ error: { details: { reason: "root_not_allowed" } } });
    // A folder inside home is fine.
    expect(registry.grant(A, put(home, "work/x.txt", "x") && join(home, "work")).ok).toBe(true);
  });

  it("holds at most the configured number of roots per window, and a release frees the place and everything under it", () => {
    const { registry, base, released } = rig({ rootsMax: 2 });
    const roots = ["one", "two", "three"].map((name) => put(join(base, name), "f.txt", name) && join(base, name));
    const first = granted(registry, A, roots[0] ?? "");
    granted(registry, A, roots[1] ?? "");
    expect(registry.grant(A, roots[2] ?? "")).toMatchObject({ ok: false, error: { code: "FILE_REFUSED", details: { reason: "limit" } } });
    // Another window has its own allowance.
    expect(registry.grant(B, roots[2] ?? "").ok).toBe(true);
    const listed = entries(registry, A, first.rootId);
    expect(registry.entryCount(A)).toBe(1);
    expect(registry.release(B, first.rootId)).toBe(false);
    expect(registry.release(A, first.rootId)).toBe(true);
    expect(registry.entryCount(A), "its entry ids went with it").toBe(0);
    expect(released).toEqual([first.rootId]);
    // Its entry ids went with it.
    const file = listed.entries[0];
    expect(registry.resolve(A, file?.entryId ?? "")).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(registry.grant(A, roots[2] ?? "").ok).toBe(true);
  });
});

describe("entries", () => {
  it("lists names, kinds and sizes with opaque ids, in order, and leaves out every kind of link", () => {
    const { registry, base } = rig();
    const project = join(base, "project");
    put(project, "b.txt", "bb");
    put(project, "a.txt", "a");
    put(project, "sub/c.txt", "ccc");
    const outside = put(base, "outside/secret.txt", "secret");
    link(outside, join(project, "to-secret.txt"));
    link(join(base, "outside"), join(project, "to-outside"));
    link(join(project, "a.txt"), join(project, "alias.txt"));
    const root = granted(registry, A, project);
    const listing = entries(registry, A, root.rootId);
    expect(listing.truncated).toBe(false);
    expect(listing.entries.map((entry) => [entry.name, entry.kind, entry.sizeBytes])).toEqual([
      ["a.txt", "file", 1],
      ["b.txt", "file", 2],
      ["sub", "directory", 0],
    ]);
    for (const entry of listing.entries) {
      expect(entry.entryId).toMatch(/^ent_[0-9a-f]{32}$/);
      expect(JSON.stringify(entry)).not.toContain(base);
    }
    // A directory's entries are listed through its entry id, and the same file keeps its id across listings.
    const sub = listing.entries.find((entry) => entry.name === "sub");
    const inside = entries(registry, A, root.rootId, sub?.entryId ?? null);
    expect(inside.entries.map((entry) => entry.name)).toEqual(["c.txt"]);
    expect(entries(registry, A, root.rootId).entries.map((entry) => entry.entryId)).toEqual(listing.entries.map((entry) => entry.entryId));
  });

  it("truncates a listing at the limit and says so", () => {
    const { registry, base } = rig({ listMax: 3 });
    const project = join(base, "project");
    for (const name of ["a", "b", "c", "d", "e"]) {
      put(project, `${name}.txt`, name);
    }
    const listing = entries(registry, A, granted(registry, A, project).rootId);
    expect(listing.entries).toHaveLength(3);
    expect(listing.truncated).toBe(true);
  });

  it("forgets the oldest entry ids beyond the limit, per window", () => {
    const { registry, base } = rig({ entriesMax: 64 });
    const project = join(base, "project");
    for (let index = 0; index < 100; index += 1) {
      put(project, `f${String(index).padStart(3, "0")}.txt`, "x");
    }
    const root = granted(registry, A, project);
    const listing = entries(registry, A, root.rootId);
    const resolvable = listing.entries.filter((entry) => registry.resolve(A, entry.entryId).ok);
    expect(resolvable).toHaveLength(64);
    // The oldest were forgotten, the newest kept.
    expect(registry.resolve(A, listing.entries[0]?.entryId ?? "").ok).toBe(false);
    expect(registry.resolve(A, listing.entries[99]?.entryId ?? "").ok).toBe(true);
  });
});

describe("a path outside the granted roots is refused", () => {
  it("an entry id another window was given, or nobody was, is outside every root of this window", () => {
    const { registry, base } = rig();
    const project = join(base, "project");
    put(project, "a.txt", "a");
    const mine = granted(registry, A, project);
    const theirs = granted(registry, B, project);
    const myEntry = entries(registry, A, mine.rootId).entries[0]?.entryId ?? "";
    const theirEntry = entries(registry, B, theirs.rootId).entries[0]?.entryId ?? "";
    expect(registry.resolve(A, myEntry).ok).toBe(true);
    for (const id of [theirEntry, "ent_" + "0".repeat(32), "/etc/passwd", "../../etc/passwd", "", myEntry.toUpperCase()]) {
      expect(registry.resolve(A, id), id).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    }
    // Another window's root is not listable, and not watchable either.
    expect(registry.list(A, theirs.rootId, null)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(registry.resolveRoot(A, theirs.rootId)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    // An entry of one root cannot be listed as a directory of another.
    const second = granted(registry, A, put(base, "second/x.txt", "x") && join(base, "second"));
    expect(registry.list(A, second.rootId, myEntry)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
  });

  it("a link that replaces a listed file or folder, pointing out of the root, is outside, and the target is never opened", () => {
    const { registry, base } = rig();
    const project = join(base, "project");
    put(project, "a.txt", "a");
    put(project, "sub/inner.txt", "inner");
    const secret = put(base, "outside/secret.txt", "secret");
    const root = granted(registry, A, project);
    const listing = entries(registry, A, root.rootId);
    const file = listing.entries.find((entry) => entry.name === "a.txt")?.entryId ?? "";
    const folder = listing.entries.find((entry) => entry.name === "sub")?.entryId ?? "";
    const inner = entries(registry, A, root.rootId, folder).entries[0]?.entryId ?? "";
    expect(registry.resolve(A, file).ok).toBe(true);
    // The file is replaced by a link to a file outside the root; the folder by a link to a folder outside.
    rmSync(join(project, "a.txt"));
    symlinkSync(secret, join(project, "a.txt"));
    renameSync(join(project, "sub"), join(base, "moved-sub"));
    symlinkSync(join(base, "outside"), join(project, "sub"));
    expect(registry.resolve(A, file)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    // `sub/inner.txt` is textually inside the root; only resolution shows `sub` now leads out of it.
    expect(registry.resolve(A, inner)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(registry.list(A, root.rootId, folder)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
  });

  it("an entry that no longer exists is not found, not outside", () => {
    const { registry, base } = rig();
    const project = join(base, "project");
    const path = put(project, "a.txt", "a");
    const root = granted(registry, A, project);
    const file = entries(registry, A, root.rootId).entries[0]?.entryId ?? "";
    rmSync(path);
    expect(registry.resolve(A, file)).toMatchObject({ ok: false, error: { code: "NOT_FOUND" } });
  });

  it("a root that was itself replaced by a link is outside", () => {
    const { registry, base } = rig();
    const project = join(base, "project");
    put(project, "a.txt", "a");
    const root = granted(registry, A, project);
    renameSync(project, join(base, "gone"));
    symlinkSync(put(base, "elsewhere/x.txt", "x") && join(base, "elsewhere"), project);
    // The root's stored real path is the old directory; it no longer exists at that path.
    expect(registry.list(A, root.rootId, null)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
  });

  it("a window that closes loses its roots and every entry id", () => {
    const { registry, base } = rig();
    const project = join(base, "project");
    put(project, "a.txt", "a");
    const root = granted(registry, A, project);
    const file = entries(registry, A, root.rootId).entries[0]?.entryId ?? "";
    expect(registry.entryCount(A)).toBe(1);
    registry.revokeWindow(A);
    expect(registry.entryCount(A)).toBe(0);
    expect(registry.rootsOf(A)).toEqual([]);
    expect(registry.resolve(A, file)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(registry.list(A, root.rootId, null)).toMatchObject({ ok: false });
  });
});

describe("the path rules under every entry", () => {
  it("a relative path is plain or it is refused before the filesystem is asked", () => {
    for (const good of ["a", "a/b", "a/b/c.txt", "with space/x", "…/ünï"]) {
      expect(plainRelative(good), good).toBe(true);
    }
    for (const bad of ["", ".", "..", "../x", "a/../b", "a/./b", "/etc/passwd", "a//b", "a/", "a\0b", "a\\b", "x".repeat(5000)]) {
      expect(plainRelative(bad), JSON.stringify(bad.slice(0, 20))).toBe(false);
    }
  });

  it("resolveInRoot proves containment on real paths", () => {
    const base = scratch();
    const root = join(base, "root");
    put(root, "dir/file.txt", "f");
    put(base, "outside/o.txt", "o");
    link(join(base, "outside"), join(root, "dir", "out"));
    link(join(base, "outside", "o.txt"), join(root, "ol"));
    expect(resolveInRoot(root, "dir/file.txt")).toMatchObject({ ok: true, kind: "file" });
    expect(resolveInRoot(root, "dir")).toMatchObject({ ok: true, kind: "directory" });
    expect(resolveInRoot(root, "dir/out/o.txt")).toEqual({ ok: false, reason: "outside" });
    expect(resolveInRoot(root, "dir/out")).toEqual({ ok: false, reason: "symlink" });
    expect(resolveInRoot(root, "ol")).toEqual({ ok: false, reason: "symlink" });
    expect(resolveInRoot(root, "../outside/o.txt")).toEqual({ ok: false, reason: "not_plain" });
    expect(resolveInRoot(root, "dir/nope.txt")).toEqual({ ok: false, reason: "not_found" });
  });
});
