/**
 * Capability-token media URLs (ADP-005, DOMAIN.md 13.7 C): tokens are bound to a window and a file, answer only `GET` and
 * `HEAD`, carry the sandboxing headers, and are revoked with their window, their grant and their root.
 *
 * Real files and real descriptors; the requests are made through `MediaRegistry.handle`, the function the Electron
 * protocol handler calls, and the bodies are read from the streams it returns.
 */

import { renameSync, rmSync, symlinkSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { GrantRegistry, type GrantView } from "../src/main/local-files/grants.js";
import { MediaRegistry, MEDIA_SCHEME, parseRange, type MediaRequest, type MediaResponse } from "../src/main/local-files/media.js";
import { RootRegistry } from "../src/main/local-files/roots.js";
import { mediaTypeOf, servedMediaType } from "../src/main/local-files/results.js";

import { CONFIG, put, schedulerOf, scratch } from "./local-files-fixtures.js";
import { Clock } from "./shell-fakes.js";

const A = 31;
const B = 32;

function rig(options: { maxTokens?: number; requireRequester?: boolean } = {}) {
  const clock = new Clock();
  let media: MediaRegistry | null = null;
  const grants = new GrantRegistry({
    limits: { ttlMs: 60_000, maxLive: 32, maxFiles: 20, maxFileBytes: 1 << 20, maxTotalBytes: 1 << 22 },
    now: () => clock.now,
    scheduler: schedulerOf(clock),
    onRevoked: (key) => media?.revokeGrant(key),
  });
  const roots = new RootRegistry({ rootsMax: 8, entriesMax: 4096, listMax: 5000 }, (_window, rootId) => media?.revokeRoot(rootId), scratch());
  media = new MediaRegistry({ maxTokens: options.maxTokens ?? CONFIG.mediaMaxTokens, grants, roots, ...(options.requireRequester === undefined ? {} : { requireRequester: options.requireRequester }) });
  return { clock, grants, roots, media, dir: scratch() };
}

function grantOf(grants: GrantRegistry, window: number, ...paths: string[]): GrantView {
  const created = grants.create(window, paths);
  if (!created.ok) {
    throw new Error(`refused: ${JSON.stringify(created.failure)}`);
  }
  return created.grant;
}

function mintForGrant(r: ReturnType<typeof rig>, window: number, grant: GrantView, index = 0): string {
  const held = r.grants.lookup(window, grant.token);
  const file = held?.files[index];
  if (held === null || file === undefined) {
    throw new Error("no such file in the grant");
  }
  return r.media.mint(window, { kind: "grant", key: held.key, index }, file.name);
}

function mintForEntry(r: ReturnType<typeof rig>, window: number, directory: string, name: string): string {
  const root = r.roots.grant(window, directory);
  if (!root.ok) {
    throw new Error("root refused");
  }
  const listing = r.roots.list(window, root.result.rootId, null);
  const entry = listing.ok ? listing.result.entries.find((candidate) => candidate.name === name) : undefined;
  if (entry === undefined) {
    throw new Error(`no entry ${name}`);
  }
  return r.media.mint(window, { kind: "entry", entryId: entry.entryId, rootId: root.result.rootId }, name);
}

async function read(response: MediaResponse): Promise<string> {
  if (response.body === null) {
    return "";
  }
  const chunks: Buffer[] = [];
  for await (const chunk of response.body()) {
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks).toString();
}

const get = (url: string, range: string | null = null, extra: Partial<MediaRequest> = {}): MediaRequest => ({ method: "GET", url, range, ...extra });

describe("a media URL", () => {
  it("serves a granted file with the sandboxing headers, and the URL names no path", async () => {
    const r = rig();
    const path = put(r.dir, "holiday/photo.png", "0123456789");
    const url = mintForGrant(r, A, grantOf(r.grants, A, path));
    expect(url).toMatch(new RegExp(`^${MEDIA_SCHEME}://m/[0-9a-f]{64}/photo.png$`));
    expect(url).not.toContain(r.dir);
    const response = r.media.handle(get(url));
    expect(response.status).toBe(200);
    expect(response.headers).toMatchObject({
      "Content-Type": "image/png",
      "Content-Length": "10",
      "Accept-Ranges": "bytes",
      "Cache-Control": "no-store",
      "Content-Security-Policy": "sandbox",
      "X-Content-Type-Options": "nosniff",
    });
    expect(await read(response)).toBe("0123456789");
  });

  it("serves a file of a granted root the same way, re-opened for each request", async () => {
    const r = rig();
    const path = put(r.dir, "doc/notes.md", "# notes");
    const url = mintForEntry(r, A, join(r.dir, "doc"), "notes.md");
    expect(await read(r.media.handle(get(url)))).toBe("# notes");
    // The file changes on disk: the next request sees the new bytes (nothing is cached).
    put(r.dir, "doc/notes.md", "# changed notes");
    expect(await read(r.media.handle(get(url)))).toBe("# changed notes");
    rmSync(path);
    expect(r.media.handle(get(url)).status).toBe(404);
  });

  it("answers GET and HEAD only", () => {
    const r = rig();
    const url = mintForGrant(r, A, grantOf(r.grants, A, put(r.dir, "a.txt", "abc")));
    const head = r.media.handle({ method: "HEAD", url, range: null });
    expect(head.status).toBe(200);
    expect(head.headers["Content-Length"]).toBe("3");
    expect(head.body).toBeNull();
    for (const method of ["POST", "PUT", "DELETE", "PATCH", "OPTIONS", "TRACE", "CONNECT"]) {
      const response = r.media.handle({ method, url, range: null });
      expect(response.status, method).toBe(405);
      expect(response.headers.Allow).toBe("GET, HEAD");
      expect(response.body, method).toBeNull();
      expect(response.headers["Cache-Control"]).toBe("no-store");
    }
    expect(r.media.handle({ method: "get", url, range: null }).status).toBe(200);
  });

  it("answers a single range, a suffix range and an open range, and refuses the unsatisfiable and the multiple", async () => {
    const r = rig();
    const url = mintForGrant(r, A, grantOf(r.grants, A, put(r.dir, "a.mp4", "0123456789")));
    const partial = r.media.handle(get(url, "bytes=2-5"));
    expect(partial.status).toBe(206);
    expect(partial.headers["Content-Range"]).toBe("bytes 2-5/10");
    expect(partial.headers["Content-Length"]).toBe("4");
    expect(await read(partial)).toBe("2345");
    expect(await read(r.media.handle(get(url, "bytes=7-")))).toBe("789");
    expect(await read(r.media.handle(get(url, "bytes=-3")))).toBe("789");
    expect(await read(r.media.handle(get(url, "bytes=8-100")))).toBe("89");
    for (const range of ["bytes=10-", "bytes=20-30", "bytes=5-2", "bytes=0-1,4-5", "bytes=-0", "bytes=-", "bytes=a-b"]) {
      const response = r.media.handle(get(url, range));
      expect(response.status, range).toBe(416);
      expect(response.headers["Content-Range"], range).toBe("bytes */10");
      expect(response.body, range).toBeNull();
    }
    // A unit that is not bytes is ignored, as the protocol allows, and the whole file is served.
    expect(r.media.handle(get(url, "items=1-2")).status).toBe(200);
    // A HEAD with a range reports the range and sends no body.
    const head = r.media.handle({ method: "HEAD", url, range: "bytes=1-2" });
    expect([head.status, head.headers["Content-Length"], head.body]).toEqual([206, "2", null]);
  });

  it("serves an empty file and refuses any range on it", () => {
    const r = rig();
    const url = mintForEntry(r, A, put(r.dir, "e/empty.txt", "") && join(r.dir, "e"), "empty.txt");
    const whole = r.media.handle(get(url));
    expect([whole.status, whole.headers["Content-Length"], whole.body]).toEqual([200, "0", null]);
    expect(r.media.handle(get(url, "bytes=0-0")).status).toBe(416);
  });

  it("serves only a closed table of media types as themselves: HTML, SVG and script are octet-stream", () => {
    expect(servedMediaType("a.png")).toBe("image/png");
    expect(servedMediaType("clip.mp4")).toBe("video/mp4");
    expect(servedMediaType("song.mp3")).toBe("audio/mpeg");
    expect(servedMediaType("paper.pdf")).toBe("application/pdf");
    expect(servedMediaType("readme.txt")).toBe("text/plain");
    for (const name of ["page.html", "page.htm", "logo.svg", "app.js", "style.css", "data.xml", "run.sh", "noextension", "x.exe", "archive.zip", "doc.json", "a.PNG.html"]) {
      expect(servedMediaType(name), name).toBe("application/octet-stream");
    }
    expect(mediaTypeOf("doc.json")).toBe("application/json");
    expect(mediaTypeOf("a.PNG")).toBe("image/png");
  });

  it("serves an HTML file as octet-stream under a sandboxing policy", () => {
    const r = rig();
    const url = mintForGrant(r, A, grantOf(r.grants, A, put(r.dir, "evil.html", "<script>alert(1)</script>")));
    const response = r.media.handle(get(url));
    expect(response.headers["Content-Type"]).toBe("application/octet-stream");
    expect(response.headers["Content-Security-Policy"]).toBe("sandbox");
    expect(response.headers["X-Content-Type-Options"]).toBe("nosniff");
  });
});

describe("a token that is not a capability", () => {
  it("is 404 with no body whether it is unknown, malformed, from another scheme or host, or has no token at all", () => {
    const r = rig();
    const good = mintForGrant(r, A, grantOf(r.grants, A, put(r.dir, "a.txt", "abc")));
    const token = good.split("/")[3] ?? "";
    for (const url of [
      `${MEDIA_SCHEME}://m/${"0".repeat(64)}/a.txt`,
      `${MEDIA_SCHEME}://m/short/a.txt`,
      `${MEDIA_SCHEME}://m/`,
      `${MEDIA_SCHEME}://other/${token}/a.txt`,
      `https://m/${token}/a.txt`,
      `file:///etc/passwd`,
      "not a url",
      `${MEDIA_SCHEME}://m/${token.toUpperCase()}/a.txt`,
    ]) {
      const response = r.media.handle(get(url));
      expect(response.status, url).toBe(404);
      expect(response.body, url).toBeNull();
      expect(response.headers["Cache-Control"], url).toBe("no-store");
    }
    // The name after the token is only a label: another name serves the same one file, and dot segments in the URL are
    // resolved before the token is read, so they cannot lead to a different token or a path.
    const renamed = r.media.handle(get(`${MEDIA_SCHEME}://m/${token}/another-name.png`));
    expect([renamed.status, renamed.headers["Content-Length"]]).toEqual([200, "3"]);
    expect(r.media.handle(get(`${MEDIA_SCHEME}://m/${token}/../../etc/passwd`)).status).toBe(404);
    expect(r.media.handle(get(`${MEDIA_SCHEME}://m/${token}/%2e%2e/%2e%2e/etc/passwd`)).status).toBe(404);
  });

  it("is bound to its window: another window that learned the URL gets what a token that never existed gets", () => {
    const r = rig();
    const url = mintForGrant(r, A, grantOf(r.grants, A, put(r.dir, "a.txt", "abc")));
    expect(r.media.handle(get(url, null, { requester: A })).status).toBe(200);
    expect(r.media.handle(get(url, null, { requester: B })).status).toBe(404);
    // Without a stated requester it is served unless the shell requires one.
    expect(r.media.handle(get(url)).status).toBe(200);
    const strict = rig({ requireRequester: true });
    const strictUrl = mintForGrant(strict, A, grantOf(strict.grants, A, put(strict.dir, "a.txt", "abc")));
    expect(strict.media.handle(get(strictUrl)).status).toBe(404);
    expect(strict.media.handle(get(strictUrl, null, { requester: A })).status).toBe(200);
    expect(strict.media.handle(get(strictUrl, null, { requester: B })).status).toBe(404);
  });

  it("is revoked when its window is destroyed, and only that window's tokens go", () => {
    const r = rig();
    const mine = mintForGrant(r, A, grantOf(r.grants, A, put(r.dir, "a.txt", "abc")));
    const theirs = mintForGrant(r, B, grantOf(r.grants, B, put(r.dir, "b.txt", "xyz")));
    expect(r.media.count(A)).toBe(1);
    expect(r.media.handle(get(mine)).status).toBe(200);
    r.media.revokeWindow(A);
    expect(r.media.count(A)).toBe(0);
    expect(r.media.handle(get(mine)).status).toBe(404);
    expect(r.media.handle(get(theirs)).status).toBe(200);
  });

  it("is revoked with its grant: when the grant is claimed, discarded or expires", () => {
    const r = rig();
    const claimed = grantOf(r.grants, A, put(r.dir, "claimed.txt", "c"));
    const discarded = grantOf(r.grants, A, put(r.dir, "discarded.txt", "d"));
    const expiring = grantOf(r.grants, A, put(r.dir, "expiring.txt", "e"));
    const urls = [claimed, discarded, expiring].map((grant) => mintForGrant(r, A, grant));
    expect(urls.map((url) => r.media.handle(get(url)).status)).toEqual([200, 200, 200]);
    const claim = r.grants.claim(A, claimed.token);
    expect(r.media.handle(get(urls[0] ?? "")).status).toBe(404);
    expect(r.grants.discard(A, discarded.token)).toBe(true);
    expect(r.media.handle(get(urls[1] ?? "")).status).toBe(404);
    r.clock.advance(60_000);
    expect(r.media.handle(get(urls[2] ?? "")).status).toBe(404);
    if (claim.ok) {
      claim.files.forEach((file) => { file.close(); });
    }
  });

  it("is revoked with its root, and a root's token stops working when the file is replaced by a link out of the root", () => {
    const r = rig();
    const folder = join(r.dir, "project");
    const path = put(folder, "a.txt", "abc");
    const url = mintForEntry(r, A, folder, "a.txt");
    expect(r.media.handle(get(url)).status).toBe(200);
    // Replace the file by a link to a file outside the root.
    const secret = put(r.dir, "outside/secret.txt", "secret");
    rmSync(path);
    symlinkSync(secret, path);
    expect(r.media.handle(get(url)).status).toBe(404);
    // And releasing the root revokes it outright.
    rmSync(path);
    put(folder, "a.txt", "abc");
    expect(r.media.handle(get(url)).status).toBe(200);
    const rootId = r.roots.rootsOf(A)[0]?.rootId ?? "";
    expect(r.roots.release(A, rootId)).toBe(true);
    expect(r.media.count()).toBe(0);
    expect(r.media.handle(get(url)).status).toBe(404);
  });

  it("is limited per window: past the limit the oldest tokens are revoked first", () => {
    const r = rig({ maxTokens: 3 });
    const grant = grantOf(r.grants, A, put(r.dir, "a.txt", "abc"));
    const urls = Array.from({ length: 5 }, () => mintForGrant(r, A, grant));
    expect(r.media.count(A)).toBe(3);
    expect(urls.map((url) => r.media.handle(get(url)).status)).toEqual([404, 404, 200, 200, 200]);
  });

  it("serves a grant's descriptor, so a file swapped after the pick is not what is served", async () => {
    const r = rig();
    const path = put(r.dir, "pic.png", "picked");
    const url = mintForGrant(r, A, grantOf(r.grants, A, path));
    renameSync(put(r.dir, "other.png", "swapped in"), path);
    expect(await read(r.media.handle(get(url)))).toBe("picked");
  });
});

describe("parseRange", () => {
  it("reads one range of a file of ten bytes", () => {
    expect(parseRange(null, 10)).toEqual({ kind: "none" });
    expect(parseRange("", 10)).toEqual({ kind: "none" });
    expect(parseRange("bytes=0-0", 10)).toEqual({ kind: "partial", start: 0, end: 0 });
    expect(parseRange("bytes=0-", 10)).toEqual({ kind: "partial", start: 0, end: 9 });
    expect(parseRange("bytes=9-9", 10)).toEqual({ kind: "partial", start: 9, end: 9 });
    expect(parseRange("bytes=-10", 10)).toEqual({ kind: "partial", start: 0, end: 9 });
    expect(parseRange("bytes=-100", 10)).toEqual({ kind: "partial", start: 0, end: 9 });
    expect(parseRange(" Bytes = 1 - 3 ", 10)).toEqual({ kind: "partial", start: 1, end: 3 });
    expect(parseRange("bytes=10-10", 10)).toEqual({ kind: "unsatisfiable" });
    expect(parseRange("bytes=1-2,5-6", 10)).toEqual({ kind: "unsatisfiable" });
    expect(parseRange("bytes=0-", 0)).toEqual({ kind: "unsatisfiable" });
    expect(parseRange("pages=1-2", 10)).toEqual({ kind: "none" });
    expect(parseRange("bytes=9007199254740993-", 10)).toEqual({ kind: "unsatisfiable" });
  });
});
