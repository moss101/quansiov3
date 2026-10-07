/**
 * Capability-token media URLs (ADP-005, DOMAIN.md §13.7 C).
 *
 * A page that previews a local file (an image picked to attach, a PDF in a granted folder) is given a URL it can put
 * in an `<img>`, `<video>` or `<iframe>`, and nothing else: `quansio-media://m/<token>/<name>`. The **token** is 32
 * random bytes bound to one window and one file. It is the whole capability: there is no path in the URL, the page
 * cannot ask for another file by editing it, and a token that is unknown, revoked or never minted is `404` with no
 * body. Main mints a token only from a source it already holds for that window: a live file grant's descriptor or an
 * entry of a granted root.
 *
 * The protocol handler is a pure function from a request to a response description (`handle`), so everything it
 * promises is testable without a browser: `GET` and `HEAD` only, a single `Range`, `Cache-Control: no-store`,
 * `Content-Security-Policy: sandbox`, `X-Content-Type-Options: nosniff`, and a closed table of served media types.
 * Containment is proven on real paths at each request. Tokens die with their window, their grant, their root.
 */

import { createHash, randomBytes } from "node:crypto";
import { createReadStream } from "node:fs";
import type { Readable } from "node:stream";

import { closeQuietly, openRegularFile } from "./fs-ops.js";
import type { ResolvedEntry } from "./roots.js";
import { servedMediaType, type Answer } from "./results.js";

/** The custom scheme and the one host the URLs use. */
export const MEDIA_SCHEME = "quansio-media";
export const MEDIA_HOST = "m";

export type MediaSource =
  | { readonly kind: "grant"; readonly key: string; readonly index: number }
  | { readonly kind: "entry"; readonly entryId: string; readonly rootId: string };

/** The slice of the grant registry the handler reads. */
export interface MediaGrants {
  readerFor(key: string, index: number): { readonly fd: number; readonly sizeBytes: number; readonly name: string } | null;
}

/** The slice of the root registry the handler reads. */
export interface MediaRoots {
  resolve(windowId: number, entryId: string): Answer<ResolvedEntry>;
}

export interface MediaDeps {
  readonly maxTokens: number;
  readonly grants: MediaGrants;
  readonly roots: MediaRoots;
  /**
   * Whether a request must say which window sent it. The shell tells the registry the requesting `webContents` when the
   * platform reports it (`MediaRequest.requester`); with this on, a request that cannot say is refused, so the window
   * binding is enforced on every request and not only at the moment of minting.
   */
  readonly requireRequester?: boolean;
}

export interface MediaRequest {
  readonly method: string;
  readonly url: string;
  /** The `Range` request header, if any. */
  readonly range: string | null;
  /** The window (`webContents` id) that sent the request, when the platform says; `null` when it does not. */
  readonly requester?: number | null;
}

export interface MediaResponse {
  readonly status: number;
  readonly headers: Readonly<Record<string, string>>;
  /** The body; absent for `HEAD`, errors and empty files. Call once. */
  readonly body: (() => Readable) | null;
}

interface Capability {
  readonly windowId: number;
  readonly source: MediaSource;
  readonly name: string;
}

/** The headers every response carries, whatever its status. */
const BASE_HEADERS: Readonly<Record<string, string>> = {
  "Cache-Control": "no-store",
  "Content-Security-Policy": "sandbox",
  "X-Content-Type-Options": "nosniff",
};

export type RangeResult = { readonly kind: "none" } | { readonly kind: "partial"; readonly start: number; readonly end: number } | { readonly kind: "unsatisfiable" };

/** Parse a `Range` header against a file of `size` bytes. One `bytes=` range is honoured; more than one is unsatisfiable. */
export function parseRange(header: string | null, size: number): RangeResult {
  if (header === null || header.trim() === "") {
    return { kind: "none" };
  }
  const match = /^\s*bytes\s*=\s*(\d*)\s*-\s*(\d*)\s*$/i.exec(header);
  if (match === null) {
    // Another unit, or several ranges: not one this handler serves.
    return /^\s*bytes\s*=/i.test(header) ? { kind: "unsatisfiable" } : { kind: "none" };
  }
  const [, first = "", last = ""] = match;
  if (first === "" && last === "") {
    return { kind: "unsatisfiable" };
  }
  if (first === "") {
    // A suffix: the last `n` bytes.
    const count = Number(last);
    if (!Number.isSafeInteger(count) || count === 0 || size === 0) {
      return { kind: "unsatisfiable" };
    }
    return { kind: "partial", start: Math.max(0, size - count), end: size - 1 };
  }
  const start = Number(first);
  const end = last === "" ? size - 1 : Math.min(Number(last), size - 1);
  if (!Number.isSafeInteger(start) || start >= size || (last !== "" && Number(last) < start)) {
    return { kind: "unsatisfiable" };
  }
  return { kind: "partial", start, end };
}

function tokenKey(token: string): string {
  return createHash("sha256").update(token).digest("hex");
}

export class MediaRegistry {
  readonly #deps: MediaDeps;
  readonly #capabilities = new Map<string, Capability>();

  constructor(deps: MediaDeps) {
    this.#deps = deps;
  }

  /** Tokens alive for a window (all windows when omitted). */
  count(windowId?: number): number {
    if (windowId === undefined) {
      return this.#capabilities.size;
    }
    let count = 0;
    for (const capability of this.#capabilities.values()) {
      count += capability.windowId === windowId ? 1 : 0;
    }
    return count;
  }

  /** Mint a token for `windowId` over a source the caller has already proven that window holds. */
  mint(windowId: number, source: MediaSource, name: string): string {
    const token = randomBytes(32).toString("hex");
    this.#capabilities.set(tokenKey(token), { windowId, source, name });
    // Beyond the window's limit, its oldest tokens are revoked first.
    const own = [...this.#capabilities].filter(([, capability]) => capability.windowId === windowId);
    for (const [key] of own.slice(0, Math.max(0, own.length - this.#deps.maxTokens))) {
      this.#capabilities.delete(key);
    }
    return `${MEDIA_SCHEME}://${MEDIA_HOST}/${token}/${encodeURIComponent(name)}`;
  }

  revokeWindow(windowId: number): void {
    this.#revokeWhere((capability) => capability.windowId === windowId);
  }

  /** The window a media URL's token is bound to, or `null` for a URL that is not one this registry minted. */
  windowOf(url: string): number | null {
    try {
      const parsed = new URL(url);
      const token = parsed.pathname.split("/")[1] ?? "";
      if (parsed.protocol !== `${MEDIA_SCHEME}:` || parsed.hostname !== MEDIA_HOST || !/^[0-9a-f]{64}$/.test(token)) {
        return null;
      }
      return this.#capabilities.get(tokenKey(token))?.windowId ?? null;
    } catch {
      return null;
    }
  }

  /** A grant stopped existing (claimed, discarded, expired, or its window went). */
  revokeGrant(key: string): void {
    this.#revokeWhere((capability) => capability.source.kind === "grant" && capability.source.key === key);
  }

  /** A root was released. */
  revokeRoot(rootId: string): void {
    this.#revokeWhere((capability) => capability.source.kind === "entry" && capability.source.rootId === rootId);
  }

  #revokeWhere(test: (capability: Capability) => boolean): void {
    for (const [key, capability] of [...this.#capabilities]) {
      if (test(capability)) {
        this.#capabilities.delete(key);
      }
    }
  }

  /** Answer one request. */
  handle(request: MediaRequest): MediaResponse {
    const gone = (status: number, extra: Readonly<Record<string, string>> = {}): MediaResponse => ({ status, headers: { ...BASE_HEADERS, ...extra }, body: null });
    let url: URL;
    try {
      url = new URL(request.url);
    } catch {
      return gone(404);
    }
    const token = url.pathname.split("/")[1] ?? "";
    if (url.protocol !== `${MEDIA_SCHEME}:` || url.hostname !== MEDIA_HOST || !/^[0-9a-f]{64}$/.test(token)) {
      return gone(404);
    }
    const method = request.method.toUpperCase();
    if (method !== "GET" && method !== "HEAD") {
      return gone(405, { Allow: "GET, HEAD" });
    }
    const capability = this.#capabilities.get(tokenKey(token));
    if (capability === undefined) {
      return gone(404);
    }
    // A token is bound to the window it was minted for. Another window that learned it, or a request that cannot
    // say who sent it when the shell requires that, gets the same 404 as a token that never existed.
    const requester = request.requester ?? null;
    if ((requester !== null && requester !== capability.windowId) || (requester === null && this.#deps.requireRequester === true)) {
      return gone(404);
    }
    const opened = this.#open(capability);
    if (opened === null) {
      return gone(404);
    }
    const { fd, size, owned, name } = opened;
    const range = parseRange(request.range, size);
    if (range.kind === "unsatisfiable") {
      if (owned) {
        closeQuietly(fd);
      }
      return gone(416, { "Content-Range": `bytes */${String(size)}`, "Accept-Ranges": "bytes" });
    }
    const [start, end] = range.kind === "partial" ? [range.start, range.end] : [0, size - 1];
    const length = size === 0 ? 0 : end - start + 1;
    const headers: Record<string, string> = {
      ...BASE_HEADERS,
      "Content-Type": servedMediaType(name),
      "Content-Length": String(length),
      "Accept-Ranges": "bytes",
    };
    if (range.kind === "partial") {
      headers["Content-Range"] = `bytes ${String(start)}-${String(end)}/${String(size)}`;
    }
    const status = range.kind === "partial" ? 206 : 200;
    if (method === "HEAD" || length === 0) {
      if (owned) {
        closeQuietly(fd);
      }
      return { status, headers, body: null };
    }
    return {
      status,
      headers,
      body: () => createReadStream("", { fd, start, end, autoClose: owned }),
    };
  }

  /** Open the capability's file now: a grant's own descriptor, or a root entry re-proved and opened without following links. */
  #open(capability: Capability): { fd: number; size: number; owned: boolean; name: string } | null {
    if (capability.source.kind === "grant") {
      const reader = this.#deps.grants.readerFor(capability.source.key, capability.source.index);
      return reader === null ? null : { fd: reader.fd, size: reader.sizeBytes, owned: false, name: reader.name };
    }
    const resolved = this.#deps.roots.resolve(capability.windowId, capability.source.entryId);
    if (!resolved.ok || resolved.result.kind !== "file") {
      return null;
    }
    const file = openRegularFile(resolved.result.absolute);
    return file.ok ? { fd: file.fd, size: file.facts.sizeBytes, owned: true, name: resolved.result.name } : null;
  }
}
