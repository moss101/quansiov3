/**
 * Edit-conflict-safe local writes (ADP-005, DOMAIN.md §13.7 D).
 *
 * When a person edits a file on their own disk, someone else may be editing it too (an editor, a build, an agent in
 * the same folder). A write that overwrites what it never saw loses that work silently, so a write here is a
 * compare-and-replace:
 *
 * 1. **A baseline is captured at read.** `size` and `mtime` (nanoseconds) come from the descriptor the bytes were
 *    read through, and main keeps them in an edit session; the page never supplies a baseline.
 * 2. **The new bytes go to a temporary file first**, created exclusively next to the target (so two writers never
 *    share one), with the target's permissions, and `fsync`ed.
 * 3. **Then the target is looked at again** and compared with the baseline. Any difference, a target that is gone or
 *    one that is now a link, deletes the temporary file, leaves the target exactly as it is and answers a conflict
 *    with both versions' metadata. Checking only before the write would miss an edit made while the bytes were being
 *    written; checking after the temporary file is complete leaves only the instants between this check and the
 *    rename. POSIX has no atomic compare-and-rename, so that window is narrowed, not closed, and this file says so.
 * 4. **The rename is the only step that changes the target**, and it is atomic: a reader sees the old file or the
 *    new one. The new stat becomes the session's baseline.
 *
 * "Overwrite anyway" is a separate request, never a retry: the version on disk is first copied to a sibling backup
 * created exclusively (an existing name is never replaced), the baseline becomes that version's metadata, and the
 * same check runs again, so even an overwrite cannot silently take the place of a third writer's edit.
 *
 * Writes register a self-write with the watcher (`onWritten`) so the change they cause is not reported back as a
 * change by someone else.
 */

import { randomBytes } from "node:crypto";
import { closeSync, constants, copyFileSync, fchmodSync, fsyncSync, lstatSync, openSync, renameSync, unlinkSync, writeSync, type BigIntStats } from "node:fs";
import { basename, dirname, join } from "node:path";

import { closeQuietly, openRegularFile, readAt } from "./fs-ops.js";
import type { ResolvedEntry } from "./roots.js";
import { done, refuse, refusedFile, type Answer } from "./results.js";

/** What was seen of a file when it was read. */
export interface Baseline {
  readonly sizeBytes: number;
  readonly mtimeNs: bigint;
  readonly mode: number;
}

/** What a conflict reports of one version: only what is needed to tell them apart. */
export interface VersionMeta {
  readonly size_bytes: number;
  readonly mtime_ns: string;
}

export interface EditHooks {
  /** Called with the target's path after the temporary file is written and before the target is re-checked (test seam). */
  readonly afterTemporary?: (target: string) => void;
}

export type OnConflict = "fail" | "overwrite";

export interface Saved {
  readonly baseline: Baseline;
  /** The name of the backup made by an overwrite, if one was. */
  readonly backup: string | null;
}

export type SaveOutcome =
  | { readonly ok: true; readonly saved: Saved }
  | { readonly ok: false; readonly kind: "conflict"; readonly baseline: VersionMeta; readonly current: VersionMeta | null }
  | { readonly ok: false; readonly kind: "refused"; readonly reason: "symlink" | "not_regular" | "unreadable" };

const meta = (stats: { readonly size: bigint | number; readonly mtimeNs: bigint }): VersionMeta => ({ size_bytes: Number(stats.size), mtime_ns: stats.mtimeNs.toString() });
const metaOf = (baseline: Baseline): VersionMeta => ({ size_bytes: baseline.sizeBytes, mtime_ns: baseline.mtimeNs.toString() });

function statNow(path: string): BigIntStats | null {
  try {
    return lstatSync(path, { bigint: true });
  } catch {
    return null;
  }
}

/** Whether `stats` is what `baseline` saw (size and modification time). */
function unchanged(stats: BigIntStats | null, baseline: Baseline): boolean {
  return stats !== null && stats.isFile() && Number(stats.size) === baseline.sizeBytes && stats.mtimeNs === baseline.mtimeNs;
}

function backupBeside(target: string): string | null {
  const directory = dirname(target);
  const name = basename(target);
  for (let number = 1; number <= 1000; number += 1) {
    const candidate = join(directory, `${name}.quansio-backup-${String(number)}`);
    try {
      copyFileSync(target, candidate, constants.COPYFILE_EXCL);
      return basename(candidate);
    } catch (error) {
      if ((error as { code?: unknown }).code !== "EEXIST") {
        return null;
      }
    }
  }
  return null;
}

/**
 * Replace `target` with `bytes` if it is still what `baseline` saw, as described above.
 *
 * `target` is an absolute path the caller proved is inside a granted root (`RootRegistry.resolve`); this function
 * does not follow a link at the last element and refuses a target that is one.
 */
export function saveWithBaseline(target: string, bytes: Buffer, baseline: Baseline, onConflict: OnConflict, hooks: EditHooks = {}): SaveOutcome {
  let effective = baseline;
  let backup: string | null = null;
  const first = statNow(target);
  if (first !== null && first.isSymbolicLink()) {
    return { ok: false, kind: "refused", reason: "symlink" };
  }
  if (first !== null && !first.isFile()) {
    return { ok: false, kind: "refused", reason: "not_regular" };
  }
  if (!unchanged(first, effective)) {
    if (onConflict === "fail" || first === null) {
      return { ok: false, kind: "conflict", baseline: metaOf(effective), current: first === null ? null : meta(first) };
    }
    // An overwrite takes the version that is on disk now as its baseline, after keeping a copy of it.
    backup = backupBeside(target);
    if (backup === null) {
      return { ok: false, kind: "refused", reason: "unreadable" };
    }
    effective = { sizeBytes: Number(first.size), mtimeNs: first.mtimeNs, mode: Number(first.mode) };
  }

  const temporary = join(dirname(target), `.${basename(target)}.quansio-tmp-${randomBytes(6).toString("hex")}`);
  let fd = -1;
  try {
    // Exclusive: an existing name is an error, never a file to reuse. The target's permission bits, whatever the umask.
    const mode = effective.mode & 0o7777;
    fd = openSync(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL, mode);
    fchmodSync(fd, mode);
    let written = 0;
    while (written < bytes.length) {
      written += writeSync(fd, bytes, written, bytes.length - written);
    }
    fsyncSync(fd);
    closeSync(fd);
    fd = -1;
  } catch {
    if (fd !== -1) {
      closeQuietly(fd);
    }
    removeQuietly(temporary);
    return { ok: false, kind: "refused", reason: "unreadable" };
  }

  hooks.afterTemporary?.(target);

  // The re-check after the temporary file is complete: the one that catches an edit made while it was written.
  const second = statNow(target);
  if (!unchanged(second, effective)) {
    removeQuietly(temporary);
    return { ok: false, kind: "conflict", baseline: metaOf(effective), current: second === null ? null : meta(second) };
  }
  try {
    renameSync(temporary, target);
  } catch {
    removeQuietly(temporary);
    return { ok: false, kind: "refused", reason: "unreadable" };
  }
  syncDirectory(dirname(target));
  const after = statNow(target);
  if (after === null || !after.isFile()) {
    return { ok: false, kind: "refused", reason: "unreadable" };
  }
  return { ok: true, saved: { baseline: { sizeBytes: Number(after.size), mtimeNs: after.mtimeNs, mode: Number(after.mode) }, backup } };
}

function removeQuietly(path: string): void {
  try {
    unlinkSync(path);
  } catch {
    // Already gone.
  }
}

function syncDirectory(path: string): void {
  try {
    const fd = openSync(path, constants.O_RDONLY);
    try {
      fsyncSync(fd);
    } finally {
      closeSync(fd);
    }
  } catch {
    // A directory that cannot be synced (some platforms) is not a failed write.
  }
}

export type ReadOutcome =
  | { readonly ok: true; readonly text: string; readonly baseline: Baseline }
  | { readonly ok: false; readonly reason: "symlink" | "not_regular" | "unreadable" | "too_large" | "not_text" };

/** Read a small UTF-8 text file through one descriptor and take the baseline from that descriptor. */
export function readForEdit(absolute: string, maxBytes: number): ReadOutcome {
  const opened = openRegularFile(absolute);
  if (!opened.ok) {
    return { ok: false, reason: opened.reason === "not_found" ? "unreadable" : opened.reason };
  }
  try {
    if (opened.facts.sizeBytes > maxBytes) {
      return { ok: false, reason: "too_large" };
    }
    const bytes = readAt(opened.fd, 0, opened.facts.sizeBytes);
    if (bytes.includes(0)) {
      return { ok: false, reason: "not_text" };
    }
    let text: string;
    try {
      text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    } catch {
      return { ok: false, reason: "not_text" };
    }
    return { ok: true, text, baseline: { sizeBytes: opened.facts.sizeBytes, mtimeNs: opened.facts.mtimeNs, mode: opened.facts.mode } };
  } finally {
    closeQuietly(opened.fd);
  }
}

// ---- edit sessions ----

interface Session {
  readonly windowId: number;
  readonly entryId: string;
  readonly rootId: string;
  baseline: Baseline;
}

export interface EditDeps {
  readonly maxBytes: number;
  readonly maxSessions: number;
  readonly resolve: (windowId: number, entryId: string) => Answer<ResolvedEntry>;
  /** Told after a successful write, so the watcher can recognise the change as ours. */
  readonly onWritten?: (absolute: string, baseline: Baseline) => void;
  readonly hooks?: EditHooks;
}

export interface Opened {
  readonly editId: string;
  readonly text: string;
  readonly sizeBytes: number;
  readonly mtimeNs: string;
}

/** What the page learns after a save. */
export interface SavedView {
  readonly sizeBytes: number;
  readonly mtimeNs: string;
  readonly backup: string | null;
}

const EDIT_ID = /^edit_[0-9a-f]{32}$/;

export function isEditId(value: string): boolean {
  return EDIT_ID.test(value);
}

export class EditSessions {
  readonly #deps: EditDeps;
  readonly #sessions = new Map<string, Session>();

  constructor(deps: EditDeps) {
    this.#deps = deps;
  }

  count(windowId?: number): number {
    return [...this.#sessions.values()].filter((session) => windowId === undefined || session.windowId === windowId).length;
  }

  /** Read an entry for editing and keep its baseline. */
  open(windowId: number, entryId: string): Answer<Opened> {
    const resolved = this.#deps.resolve(windowId, entryId);
    if (!resolved.ok) {
      return resolved;
    }
    if (resolved.result.kind !== "file") {
      return refusedFile("not_regular", "only a file can be edited");
    }
    if (this.count(windowId) >= this.#deps.maxSessions) {
      return refusedFile("limit", "this window has as many files open for editing as it may; close one first");
    }
    const read = readForEdit(resolved.result.absolute, this.#deps.maxBytes);
    if (!read.ok) {
      return refusedFile(read.reason, `that file cannot be edited (${read.reason})`, resolved.result.name);
    }
    const editId = `edit_${randomBytes(16).toString("hex")}`;
    this.#sessions.set(editId, { windowId, entryId, rootId: resolved.result.rootId, baseline: read.baseline });
    return done({ editId, text: read.text, sizeBytes: read.baseline.sizeBytes, mtimeNs: read.baseline.mtimeNs.toString() });
  }

  /** Save `text` if the file is still what was read; see the file's header. */
  save(windowId: number, editId: string, text: string, onConflict: OnConflict): Answer<SavedView> {
    const session = this.#sessions.get(editId);
    if (session?.windowId !== windowId) {
      return refuse("NOT_FOUND", "that edit session is not open");
    }
    const bytes = Buffer.from(text, "utf8");
    if (bytes.length > this.#deps.maxBytes) {
      return refusedFile("too_large", "that is more than a file can be edited at");
    }
    if (bytes.includes(0)) {
      return refusedFile("not_text", "text with a NUL byte is not saved as a text file");
    }
    // Containment is proved again now: the entry may have been replaced by a link since it was opened.
    const resolved = this.#deps.resolve(windowId, session.entryId);
    if (!resolved.ok) {
      // A file that is gone is the strongest kind of change since the read, and is reported as one.
      return resolved.error.code === "NOT_FOUND"
        ? refuse("FILE_EDIT_CONFLICT", "the file was deleted after it was read; nothing was written", { baseline: metaOf(session.baseline), current: null })
        : resolved;
    }
    const outcome = saveWithBaseline(resolved.result.absolute, bytes, session.baseline, onConflict, this.#deps.hooks);
    if (!outcome.ok) {
      if (outcome.kind === "conflict") {
        return refuse("FILE_EDIT_CONFLICT", "the file changed after it was read; nothing was written", { baseline: outcome.baseline, current: outcome.current });
      }
      return refusedFile(outcome.reason, `the file cannot be written (${outcome.reason})`, resolved.result.name);
    }
    session.baseline = outcome.saved.baseline;
    this.#deps.onWritten?.(resolved.result.absolute, outcome.saved.baseline);
    return done({ sizeBytes: outcome.saved.baseline.sizeBytes, mtimeNs: outcome.saved.baseline.mtimeNs.toString(), backup: outcome.saved.backup });
  }

  close(windowId: number, editId: string): boolean {
    const session = this.#sessions.get(editId);
    if (session?.windowId !== windowId) {
      return false;
    }
    this.#sessions.delete(editId);
    return true;
  }

  revokeWindow(windowId: number): void {
    for (const [id, session] of [...this.#sessions]) {
      if (session.windowId === windowId) {
        this.#sessions.delete(id);
      }
    }
  }

  revokeRoot(rootId: string): void {
    for (const [id, session] of [...this.#sessions]) {
      if (session.rootId === rootId) {
        this.#sessions.delete(id);
      }
    }
  }
}
