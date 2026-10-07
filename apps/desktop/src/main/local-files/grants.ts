/**
 * One-time file grants (ADP-005, DOMAIN.md §13.7 A).
 *
 * The renderer can never name a path. A native open dialog (main's, never the page's) yields paths; main opens each
 * one by descriptor at that moment and keeps the descriptors under a **grant**: a random 32-byte token bound to the
 * window that asked, single use, short lived, and limited in how many files and bytes it holds. The page holds the
 * token and the file names; the attach pipeline in main claims it and reads from the descriptors. There is no call
 * that turns a token, or anything else the page holds, into a path or into a read of one.
 *
 * What makes a grant narrow:
 *
 * - **bound to a window.** A claim from another window, a claim after the window closed and a claim of a token that
 *   never existed all answer the same `invalid`; only the owning window learns a token `expired`.
 * - **single use.** The first claim removes the grant. Its files move to the claimant, who closes them.
 * - **opened, then checked.** Each file is opened `O_NOFOLLOW` and `fstat`ed; a directory, a device, a FIFO and a
 *   link are refused, and what is read later is what was opened, whatever happens to the path in between.
 * - **bounded.** At most `grantMaxLive` grants, `grantMaxFiles` files, `grantMaxFileBytes` per file and
 *   `grantMaxTotalBytes` in all; a grant that breaks a limit is refused whole and nothing it opened stays open.
 * - **gone with the window**, and with the clock: a timer closes an expired grant's descriptors, so a grant nobody
 *   claims does not hold files open.
 *
 * The raw token is returned once and never stored: the registry keeps its SHA-256 as the key.
 */

import { createHash, randomBytes } from "node:crypto";

import { closeQuietly, openRegularFile, readAt } from "./fs-ops.js";
import { displayName, mediaTypeOf, type RefusedReason } from "./results.js";

export interface GrantLimits {
  readonly ttlMs: number;
  readonly maxLive: number;
  readonly maxFiles: number;
  readonly maxFileBytes: number;
  readonly maxTotalBytes: number;
}

/** Timers, injectable so a test can move time without waiting. */
export interface Scheduler {
  set(callback: () => void, delayMs: number): unknown;
  clear(handle: unknown): void;
}

export const REAL_SCHEDULER: Scheduler = {
  set: (callback, delayMs) => {
    const handle = setTimeout(callback, delayMs);
    // A pending expiry must not keep the app alive.
    handle.unref();
    return handle;
  },
  clear: (handle) => {
    clearTimeout(handle as ReturnType<typeof setTimeout>);
  },
};

export interface GrantDeps {
  readonly limits: GrantLimits;
  readonly now: () => number;
  readonly scheduler: Scheduler;
  /** A fresh token: 32 random bytes as 64 hex digits. */
  readonly token?: () => string;
  /** Called with the grant's key whenever a grant stops existing (claimed, discarded, expired, window gone). */
  readonly onRevoked?: (key: string) => void;
}

/** What the page is told about one picked file. */
export interface GrantedFileView {
  readonly index: number;
  readonly name: string;
  readonly sizeBytes: number;
  readonly mediaType: string;
}

/** What the page is told about a grant: the token and what it holds, never a path. */
export interface GrantView {
  readonly token: string;
  /** Epoch milliseconds. */
  readonly expiresAt: number;
  readonly files: readonly GrantedFileView[];
  readonly totalBytes: number;
}

export type GrantFailure = { readonly reason: RefusedReason; readonly name: string };

export type CreateResult = { readonly ok: true; readonly grant: GrantView } | { readonly ok: false; readonly failure: GrantFailure };

/** A file handed to the claimant: read it once, then close it. */
export interface ClaimedFile {
  readonly name: string;
  readonly sizeBytes: number;
  readonly mediaType: string;
  /** The file's bytes as of the descriptor (a file that shrank returns fewer). Throws once closed. */
  read(): Buffer;
  close(): void;
}

export type ClaimResult = { readonly ok: true; readonly files: readonly ClaimedFile[] } | { readonly ok: false; readonly reason: "invalid" | "expired" };

interface HeldFile {
  readonly name: string;
  readonly sizeBytes: number;
  readonly mediaType: string;
  readonly fd: number;
}

interface LiveGrant {
  readonly key: string;
  readonly windowId: number;
  readonly expiresAt: number;
  readonly files: readonly HeldFile[];
  timer: unknown;
}

/** The SHA-256 key a token is stored under. */
export function grantKey(token: string): string {
  return createHash("sha256").update(token).digest("hex");
}

export class GrantRegistry {
  readonly #deps: GrantDeps;
  readonly #live = new Map<string, LiveGrant>();
  /** Files handed to a claimant and not yet closed, by window: closed if the window goes first. */
  readonly #claimed = new Map<number, Set<ClaimedHandle>>();

  constructor(deps: GrantDeps) {
    this.#deps = deps;
  }

  /** Grants alive now (expired ones are dropped first). */
  liveCount(): number {
    this.#expireDue();
    return this.#live.size;
  }

  /** Descriptors held by live grants and unclosed claimed files; a leak shows here. */
  openDescriptors(): number {
    let count = 0;
    for (const grant of this.#live.values()) {
      count += grant.files.length;
    }
    for (const set of this.#claimed.values()) {
      for (const handle of set) {
        count += handle.closed ? 0 : 1;
      }
    }
    return count;
  }

  /**
   * Open `picked` (paths from a native dialog) and hold them under a new grant for `windowId`. Refused whole when
   * any file breaks a rule or the count, size or live-grant limits are passed; nothing it opened stays open.
   */
  create(windowId: number, picked: readonly string[]): CreateResult {
    const limits = this.#deps.limits;
    this.#expireDue();
    if (this.#live.size >= limits.maxLive) {
      return { ok: false, failure: { reason: "limit", name: "" } };
    }
    if (picked.length > limits.maxFiles) {
      return { ok: false, failure: { reason: "too_many", name: "" } };
    }
    const held: HeldFile[] = [];
    let total = 0;
    const fail = (reason: RefusedReason, name: string): CreateResult => {
      for (const file of held) {
        closeQuietly(file.fd);
      }
      return { ok: false, failure: { reason, name } };
    };
    for (const path of picked) {
      const name = displayName(path);
      const opened = openRegularFile(path);
      if (!opened.ok) {
        return fail(opened.reason === "symlink" ? "symlink" : opened.reason === "not_regular" ? "not_regular" : "unreadable", name);
      }
      if (opened.facts.sizeBytes > limits.maxFileBytes) {
        closeQuietly(opened.fd);
        return fail("too_large", name);
      }
      total += opened.facts.sizeBytes;
      if (total > limits.maxTotalBytes) {
        closeQuietly(opened.fd);
        return fail("total_too_large", name);
      }
      held.push({ name, sizeBytes: opened.facts.sizeBytes, mediaType: mediaTypeOf(name), fd: opened.fd });
    }
    const token = (this.#deps.token ?? randomToken)();
    const key = grantKey(token);
    const expiresAt = this.#deps.now() + limits.ttlMs;
    const grant: LiveGrant = { key, windowId, expiresAt, files: held, timer: null };
    grant.timer = this.#deps.scheduler.set(() => {
      this.#expire(key);
    }, limits.ttlMs);
    this.#live.set(key, grant);
    return {
      ok: true,
      grant: {
        token,
        expiresAt,
        files: held.map((file, index) => ({ index, name: file.name, sizeBytes: file.sizeBytes, mediaType: file.mediaType })),
        totalBytes: total,
      },
    };
  }

  /**
   * The grant behind `token` if `windowId` holds it and it is alive: used to preview a file before the claim. It
   * grants nothing the owner did not have; a token that is another window's, expired or unknown is `null`.
   */
  lookup(windowId: number, token: string): { readonly key: string; readonly files: readonly GrantedFileView[] } | null {
    const grant = this.#owned(windowId, token);
    if (grant === null || grant === "expired") {
      return null;
    }
    return { key: grant.key, files: grant.files.map((file, index) => ({ index, name: file.name, sizeBytes: file.sizeBytes, mediaType: file.mediaType })) };
  }

  /** A file of a live grant for a media request: its descriptor stays owned by the grant. `null` once the grant is gone. */
  readerFor(key: string, index: number): { readonly fd: number; readonly sizeBytes: number; readonly name: string } | null {
    this.#expireDue();
    const grant = this.#live.get(key);
    const file = grant?.files[index];
    return file === undefined ? null : { fd: file.fd, sizeBytes: file.sizeBytes, name: file.name };
  }

  /** Claim the grant: it is removed on this call, whatever happens next, and its files move to the claimant. */
  claim(windowId: number, token: string): ClaimResult {
    const grant = this.#owned(windowId, token);
    if (grant === null) {
      return { ok: false, reason: "invalid" };
    }
    if (grant === "expired") {
      return { ok: false, reason: "expired" };
    }
    this.#remove(grant, false);
    const handles = grant.files.map(
      (file) =>
        new ClaimedHandle(file, (handle) => {
          this.#released(windowId, handle);
        }),
    );
    const set = this.#claimed.get(windowId) ?? new Set<ClaimedHandle>();
    for (const handle of handles) {
      set.add(handle);
    }
    this.#claimed.set(windowId, set);
    return { ok: true, files: handles };
  }

  /** Revoke a grant its window no longer wants. */
  discard(windowId: number, token: string): boolean {
    const grant = this.#owned(windowId, token);
    if (grant === null || grant === "expired") {
      return false;
    }
    this.#remove(grant, true);
    return true;
  }

  /** The window is gone: its live grants are revoked and the files it was handed are closed. */
  revokeWindow(windowId: number): number {
    let count = 0;
    for (const grant of [...this.#live.values()]) {
      if (grant.windowId === windowId) {
        this.#remove(grant, true);
        count += 1;
      }
    }
    for (const handle of [...(this.#claimed.get(windowId) ?? [])]) {
      handle.close();
    }
    this.#claimed.delete(windowId);
    return count;
  }

  /** Every live grant is revoked (the application is quitting or the feature was switched off). */
  revokeAll(): void {
    for (const grant of [...this.#live.values()]) {
      this.#remove(grant, true);
    }
    for (const set of this.#claimed.values()) {
      for (const handle of [...set]) {
        handle.close();
      }
    }
    this.#claimed.clear();
  }

  // ---- internals ----

  /** The live grant of `windowId` for `token`; `"expired"` for the owner's expired one; `null` for anything else. */
  #owned(windowId: number, token: string): LiveGrant | "expired" | null {
    if (typeof token !== "string" || !/^[0-9a-f]{64}$/.test(token)) {
      return null;
    }
    const grant = this.#live.get(grantKey(token));
    if (grant === undefined || grant.windowId !== windowId) {
      return null;
    }
    if (this.#deps.now() >= grant.expiresAt) {
      this.#remove(grant, true);
      return "expired";
    }
    return grant;
  }

  #expire(key: string): void {
    const grant = this.#live.get(key);
    if (grant !== undefined) {
      this.#remove(grant, true);
    }
  }

  #expireDue(): void {
    const now = this.#deps.now();
    for (const grant of [...this.#live.values()]) {
      if (now >= grant.expiresAt) {
        this.#remove(grant, true);
      }
    }
  }

  /** Take a grant out; `closeFiles` is false when its descriptors move to a claimant. */
  #remove(grant: LiveGrant, closeFiles: boolean): void {
    this.#live.delete(grant.key);
    this.#deps.scheduler.clear(grant.timer);
    if (closeFiles) {
      for (const file of grant.files) {
        closeQuietly(file.fd);
      }
    }
    this.#deps.onRevoked?.(grant.key);
  }

  #released(windowId: number, handle: ClaimedHandle): void {
    const set = this.#claimed.get(windowId);
    set?.delete(handle);
    if (set?.size === 0) {
      this.#claimed.delete(windowId);
    }
  }
}

class ClaimedHandle implements ClaimedFile {
  readonly name: string;
  readonly sizeBytes: number;
  readonly mediaType: string;
  closed = false;
  readonly #fd: number;
  readonly #onClose: (handle: ClaimedHandle) => void;

  constructor(file: HeldFile, onClose: (handle: ClaimedHandle) => void) {
    this.name = file.name;
    this.sizeBytes = file.sizeBytes;
    this.mediaType = file.mediaType;
    this.#fd = file.fd;
    this.#onClose = onClose;
  }

  read(): Buffer {
    if (this.closed) {
      throw new Error("the file was closed");
    }
    return readAt(this.#fd, 0, this.sizeBytes);
  }

  close(): void {
    if (!this.closed) {
      this.closed = true;
      closeQuietly(this.#fd);
      this.#onClose(this);
    }
  }
}

function randomToken(): string {
  return randomBytes(32).toString("hex");
}
