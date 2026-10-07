/**
 * Granted roots and opaque entry ids (ADP-005, DOMAIN.md §13.7 B).
 *
 * A **granted root** is a folder the person picked in a native folder dialog. Main keeps its `realpath` under a random
 * root id, for the window that picked it. The page lists a root, and everything it learns it learns as an **entry**:
 * an opaque random id that main maps to `(root, relative path)`. No channel accepts a path, so there is nothing for a
 * page to forge: an entry id the window was never given is `FILE_OUTSIDE_ROOTS`, the same answer as a path that left
 * its root. Every use re-proves containment on real paths at that moment (`resolveInRoot`), so a link made after the
 * listing, or a directory swapped for one, cannot carry a use outside the root.
 *
 * Symbolic links are listed as nothing and never followed. A root, its entries and everything built on them end with
 * their window.
 */

import { randomBytes } from "node:crypto";
import { readdirSync, lstatSync } from "node:fs";
import { basename, join } from "node:path";
import { homedir } from "node:os";

import { realDirectory, resolveInRoot } from "./fs-ops.js";
import { done, refuse, refusedFile, type Answer } from "./results.js";

export interface RootLimits {
  readonly rootsMax: number;
  readonly entriesMax: number;
  /** The most entries one listing returns. */
  readonly listMax: number;
}

export interface RootView {
  readonly rootId: string;
  readonly name: string;
}

export interface EntryView {
  readonly entryId: string;
  readonly name: string;
  readonly kind: "file" | "directory";
  readonly sizeBytes: number;
  readonly mtimeMs: number;
}

export interface Listing {
  readonly entries: readonly EntryView[];
  readonly truncated: boolean;
}

/** An entry resolved on real paths, ready to open. */
export interface ResolvedEntry {
  readonly rootId: string;
  readonly relative: string;
  readonly absolute: string;
  readonly kind: "file" | "directory";
  readonly name: string;
}

interface Root {
  readonly rootId: string;
  readonly windowId: number;
  readonly real: string;
  readonly name: string;
}

interface Entry {
  readonly windowId: number;
  readonly rootId: string;
  readonly relative: string;
}

const ROOT_ID = /^lroot_[0-9a-f]{32}$/;
const ENTRY_ID = /^ent_[0-9a-f]{32}$/;

export function isRootId(value: string): boolean {
  return ROOT_ID.test(value);
}

export function isEntryId(value: string): boolean {
  return ENTRY_ID.test(value);
}

const outside = (): Answer<never> => refuse("FILE_OUTSIDE_ROOTS", "that is not inside a folder this window was given");

export class RootRegistry {
  readonly #limits: RootLimits;
  readonly #home: string;
  readonly #roots = new Map<string, Root>();
  /** Entries by id, insertion-ordered so the oldest is forgotten first. */
  readonly #entries = new Map<string, Entry>();
  /** The id already minted for a (window, root, path), so a file keeps its id across listings. */
  readonly #byPath = new Map<string, string>();
  readonly #onReleased: (windowId: number, rootId: string) => void;

  constructor(limits: RootLimits, onReleased: (windowId: number, rootId: string) => void = () => undefined, home: string = homedir()) {
    this.#limits = limits;
    this.#onReleased = onReleased;
    this.#home = home;
  }

  /** Hold the folder at `directory` (a path from a native folder dialog) for `windowId`. */
  grant(windowId: number, directory: string): Answer<RootView> {
    const real = realDirectory(directory);
    if (real === null) {
      return refusedFile("not_regular", "that is not a folder");
    }
    // The filesystem root and the person's home folder are not a "workspace": a grant over them is a grant over everything.
    const realHome = realDirectory(this.#home);
    if (real === "/" || real === realHome || /^[A-Za-z]:\\?$/.test(real)) {
      return refusedFile("root_not_allowed", "pick a folder inside your home folder, not the whole of it");
    }
    let held = 0;
    for (const root of this.#roots.values()) {
      if (root.windowId === windowId) {
        held += 1;
        if (root.real === real) {
          return done({ rootId: root.rootId, name: root.name });
        }
      }
    }
    if (held >= this.#limits.rootsMax) {
      return refusedFile("limit", "this window holds as many folders as it may; release one first");
    }
    const root: Root = { rootId: `lroot_${randomBytes(16).toString("hex")}`, windowId, real, name: basename(real) };
    this.#roots.set(root.rootId, root);
    return done({ rootId: root.rootId, name: root.name });
  }

  /** Release a root and every entry made from it. */
  release(windowId: number, rootId: string): boolean {
    const root = this.#roots.get(rootId);
    if (root?.windowId !== windowId) {
      return false;
    }
    this.#drop(root);
    return true;
  }

  /** The window is gone: its roots and entries are too. */
  revokeWindow(windowId: number): void {
    for (const root of [...this.#roots.values()]) {
      if (root.windowId === windowId) {
        this.#drop(root);
      }
    }
  }

  #drop(root: Root): void {
    this.#roots.delete(root.rootId);
    for (const [id, entry] of [...this.#entries]) {
      if (entry.rootId === root.rootId) {
        this.#forget(id, entry);
      }
    }
    this.#onReleased(root.windowId, root.rootId);
  }

  /** The roots `windowId` holds. */
  rootsOf(windowId: number): RootView[] {
    return [...this.#roots.values()].filter((root) => root.windowId === windowId).map((root) => ({ rootId: root.rootId, name: root.name }));
  }

  /** The root's real path, for the modules that watch or serve inside it; `null` unless `windowId` holds it. */
  realOf(windowId: number, rootId: string): string | null {
    const root = this.#roots.get(rootId);
    return root?.windowId === windowId ? root.real : null;
  }

  /** Is `rootId` still held by `windowId`? */
  holds(windowId: number, rootId: string): boolean {
    return this.#roots.get(rootId)?.windowId === windowId;
  }

  /** Mint an entry id for `(rootId, relative)`; the oldest entries are forgotten beyond `entriesMax`. */
  mint(windowId: number, rootId: string, relative: string): string {
    const pathKey = `${String(windowId)}|${rootId}|${relative}`;
    const known = this.#byPath.get(pathKey);
    if (known !== undefined && this.#entries.has(known)) {
      return known;
    }
    const id = `ent_${randomBytes(16).toString("hex")}`;
    this.#entries.set(id, { windowId, rootId, relative });
    this.#byPath.set(pathKey, id);
    let surplus = 0;
    for (const entry of this.#entries.values()) {
      if (entry.windowId === windowId) {
        surplus += 1;
      }
    }
    surplus -= this.#limits.entriesMax;
    if (surplus > 0) {
      for (const [oldId, entry] of this.#entries) {
        if (surplus === 0) {
          break;
        }
        if (entry.windowId === windowId) {
          this.#forget(oldId, entry);
          surplus -= 1;
        }
      }
    }
    return id;
  }

  #forget(id: string, entry: Entry): void {
    this.#entries.delete(id);
    this.#byPath.delete(`${String(entry.windowId)}|${entry.rootId}|${entry.relative}`);
  }

  /** Resolve an entry id on real paths now; an id the window was not given, or a path that left its root, is `FILE_OUTSIDE_ROOTS`. */
  resolve(windowId: number, entryId: string): Answer<ResolvedEntry> {
    const entry = this.#entries.get(entryId);
    if (entry?.windowId !== windowId) {
      return outside();
    }
    const root = this.#roots.get(entry.rootId);
    if (root?.windowId !== windowId) {
      return outside();
    }
    const resolved = this.#inside(root, entry.relative);
    return resolved.ok ? done({ ...resolved.result, name: basename(entry.relative) }) : resolved;
  }

  /** The root directory itself as a resolved directory entry (for a listing or a watch with no entry named). */
  resolveRoot(windowId: number, rootId: string): Answer<ResolvedEntry> {
    const root = this.#roots.get(rootId);
    if (root?.windowId !== windowId || realDirectory(root.real) !== root.real) {
      return outside();
    }
    return done({ rootId, relative: "", absolute: root.real, kind: "directory", name: root.name });
  }

  #inside(root: Root, relative: string): Answer<Omit<ResolvedEntry, "name">> {
    if (relative === "") {
      return done({ rootId: root.rootId, relative, absolute: root.real, kind: "directory" });
    }
    const contained = resolveInRoot(root.real, relative);
    if (!contained.ok) {
      return contained.reason === "not_found" ? refuse("NOT_FOUND", "that file is gone") : outside();
    }
    return done({ rootId: root.rootId, relative, absolute: contained.absolute, kind: contained.kind });
  }

  /** List a directory of a root (the root itself when `entryId` is `null`): entries with fresh ids, links omitted. */
  list(windowId: number, rootId: string, entryId: string | null): Answer<Listing> {
    const directory = entryId === null ? this.resolveRoot(windowId, rootId) : this.resolve(windowId, entryId);
    if (!directory.ok) {
      return directory;
    }
    if (directory.result.rootId !== rootId || directory.result.kind !== "directory") {
      return outside();
    }
    let names: string[];
    try {
      names = readdirSync(directory.result.absolute).sort();
    } catch {
      return refuse("NOT_FOUND", "that folder cannot be read");
    }
    const entries: EntryView[] = [];
    let truncated = false;
    for (const name of names) {
      if (entries.length >= this.#limits.listMax) {
        truncated = true;
        break;
      }
      const relative = directory.result.relative === "" ? name : `${directory.result.relative}/${name}`;
      let stats;
      try {
        stats = lstatSync(join(directory.result.absolute, name));
      } catch {
        continue;
      }
      if (stats.isSymbolicLink() || !(stats.isFile() || stats.isDirectory())) {
        continue;
      }
      entries.push({
        entryId: this.mint(windowId, rootId, relative),
        name,
        kind: stats.isDirectory() ? "directory" : "file",
        sizeBytes: stats.isFile() ? stats.size : 0,
        mtimeMs: Math.floor(stats.mtimeMs),
      });
    }
    return done({ entries, truncated });
  }
}
