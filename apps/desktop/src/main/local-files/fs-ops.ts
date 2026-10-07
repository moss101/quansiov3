/**
 * The filesystem operations the local-file modules are built on (ADP-005): files opened by descriptor, never
 * followed through a symbolic link, and containment proven on real paths.
 *
 * Three rules, each one a place a path-based check goes wrong:
 *
 * - **Open, then ask.** A file is opened with `O_NOFOLLOW | O_NONBLOCK` and only then `fstat`ed, so what was checked
 *   is what is read: replacing the path after the check changes nothing the descriptor holds, and a FIFO or a device
 *   cannot hang the open.
 * - **Real paths, not text.** A path is inside a root when its parent's `realpath` is the root or below it, and its
 *   last element is not itself a link; `root/link` is textually inside the root, so only resolution catches it.
 * - **A request names a plain relative path.** No `..`, no absolute path, no NUL, no empty component: a relative
 *   path that is not plain is outside every root before the filesystem is asked.
 */

import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readSync,
  realpathSync,
  type BigIntStats,
} from "node:fs";
import { basename, dirname, isAbsolute, join, sep } from "node:path";

/** What an open descriptor says about its file. */
export interface FileFacts {
  readonly sizeBytes: number;
  readonly mtimeNs: bigint;
  readonly mode: number;
  readonly dev: bigint;
  readonly ino: bigint;
}

export type OpenFailure = "symlink" | "not_regular" | "not_found" | "unreadable";

export type Opened = { readonly ok: true; readonly fd: number; readonly facts: FileFacts } | { readonly ok: false; readonly reason: OpenFailure };

/** `O_NOFOLLOW` where the platform has it (not Windows: a `lstat` check stands in there). */
const NOFOLLOW: number = (constants as { O_NOFOLLOW?: number }).O_NOFOLLOW ?? 0;
/** `O_NONBLOCK` where the platform has it: opening a FIFO must not wait for a writer. */
const NONBLOCK: number = (constants as { O_NONBLOCK?: number }).O_NONBLOCK ?? 0;

/** `fstat` facts of a descriptor. */
export function factsOf(fd: number): FileFacts {
  const stats: BigIntStats = fstatSync(fd, { bigint: true });
  return { sizeBytes: Number(stats.size), mtimeNs: stats.mtimeNs, mode: Number(stats.mode), dev: stats.dev, ino: stats.ino };
}

/**
 * Open a regular file for reading without following a link at its last element. The caller owns the descriptor.
 * A directory, a device, a FIFO and a symbolic link are refused with a reason; none is read.
 */
export function openRegularFile(path: string): Opened {
  try {
    if (NOFOLLOW === 0 && lstatSync(path).isSymbolicLink()) {
      return { ok: false, reason: "symlink" };
    }
    const fd = openSync(path, constants.O_RDONLY | NOFOLLOW | NONBLOCK);
    try {
      const stats = fstatSync(fd, { bigint: true });
      if (!stats.isFile()) {
        closeSync(fd);
        return { ok: false, reason: "not_regular" };
      }
      return { ok: true, fd, facts: { sizeBytes: Number(stats.size), mtimeNs: stats.mtimeNs, mode: Number(stats.mode), dev: stats.dev, ino: stats.ino } };
    } catch {
      closeSync(fd);
      return { ok: false, reason: "unreadable" };
    }
  } catch (error) {
    return { ok: false, reason: failureOf(error) };
  }
}

function failureOf(error: unknown): OpenFailure {
  const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined;
  if (code === "ELOOP" || code === "EMLINK") {
    // `O_NOFOLLOW` on a link: ELOOP on Linux and macOS, EMLINK on some BSDs.
    return "symlink";
  }
  if (code === "ENOENT" || code === "ENOTDIR") {
    return "not_found";
  }
  return "unreadable";
}

/** Read exactly `length` bytes at `position` from a descriptor (fewer if the file ends first). */
export function readAt(fd: number, position: number, length: number): Buffer {
  const buffer = Buffer.alloc(length);
  let filled = 0;
  while (filled < length) {
    const count = readSync(fd, buffer, filled, length - filled, position + filled);
    if (count === 0) {
      break;
    }
    filled += count;
  }
  return filled === length ? buffer : buffer.subarray(0, filled);
}

/** Close a descriptor, ignoring a descriptor that is already closed. */
export function closeQuietly(fd: number): void {
  try {
    closeSync(fd);
  } catch {
    // Already closed.
  }
}

/** A relative path that is plain: no `..`, no absolute form, no NUL, no empty or `.` component, bounded. */
export function plainRelative(relative: string): boolean {
  if (relative === "" || relative.length > 4096 || relative.includes("\0") || isAbsolute(relative) || relative.includes("\\")) {
    return false;
  }
  return relative.split("/").every((part) => part !== "" && part !== "." && part !== "..");
}

function within(rootReal: string, candidate: string): boolean {
  return candidate === rootReal || candidate.startsWith(rootReal.endsWith(sep) ? rootReal : `${rootReal}${sep}`);
}

/** The result of resolving a plain relative path under a root. */
export type Contained =
  | { readonly ok: true; readonly absolute: string; readonly kind: "file" | "directory" }
  | { readonly ok: false; readonly reason: "not_plain" | "outside" | "symlink" | "not_found" | "unreadable" };

/**
 * Prove `relative` (plain) names an existing file or directory inside `rootReal` (a `realpath`), and return the
 * absolute path to open. The parent is resolved with `realpath` (so a linked ancestor that leaves the root is
 * `outside`) and the last element must not itself be a link.
 */
export function resolveInRoot(rootReal: string, relative: string): Contained {
  if (!plainRelative(relative)) {
    return { ok: false, reason: "not_plain" };
  }
  const absolute = join(rootReal, ...relative.split("/"));
  try {
    const parentReal = realpathSync(dirname(absolute));
    if (!within(rootReal, parentReal)) {
      return { ok: false, reason: "outside" };
    }
    const target = join(parentReal, basename(absolute));
    const stats = lstatSync(target);
    if (stats.isSymbolicLink()) {
      return { ok: false, reason: "symlink" };
    }
    if (stats.isDirectory()) {
      return { ok: true, absolute: target, kind: "directory" };
    }
    if (stats.isFile()) {
      return { ok: true, absolute: target, kind: "file" };
    }
    return { ok: false, reason: "unreadable" };
  } catch (error) {
    const reason = failureOf(error);
    return { ok: false, reason: reason === "not_found" ? "not_found" : "unreadable" };
  }
}

/** `realpath` of a directory the person picked, or `null` when it is not a directory. */
export function realDirectory(path: string): string | null {
  try {
    const real = realpathSync(path);
    return lstatSync(real).isDirectory() ? real : null;
  } catch {
    return null;
  }
}
