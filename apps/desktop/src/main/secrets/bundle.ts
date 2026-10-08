/**
 * The digest of a cookie bundle (ADP-009, DOMAIN.md §13.9 B).
 *
 * The same function as the runtime's `bundle_digest` (`crates/machine/src/secrets/imports.rs`), so the desktop can name a bundle by
 * a digest the server recomputes and refuses to seal if it differs: SHA-256 over the canonical line of each cookie, in
 * `(domain, path, name)` byte order, one line each followed by a newline. A canonical line is the JSON array
 * `[domain, path, name, value, secure, http_only, same_site, expires]` with no spaces. A test holds the two implementations to the same
 * answer for the same bundle.
 */

import { createHash } from "node:crypto";

import type { BundleCookie } from "./chromium.js";

function canonical(cookie: BundleCookie): string {
  return JSON.stringify([cookie.domain, cookie.path, cookie.name, cookie.value, cookie.secure, cookie.http_only, cookie.same_site, cookie.expires]);
}

function byteOrder(a: string, b: string): number {
  return Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));
}

/** The bundle's digest as lowercase hex. */
export function bundleDigest(cookies: readonly BundleCookie[]): string {
  const lines = cookies
    .map((cookie) => ({ cookie, line: canonical(cookie) }))
    .sort((a, b) => byteOrder(a.cookie.domain, b.cookie.domain) || byteOrder(a.cookie.path, b.cookie.path) || byteOrder(a.cookie.name, b.cookie.name) || byteOrder(a.line, b.line));
  const hash = createHash("sha256");
  for (const { line } of lines) {
    hash.update(line, "utf8");
    hash.update("\n");
  }
  return hash.digest("hex");
}

/** The bare, sorted, unique domains of a bundle (no leading dot), as an approval lists them. */
export function bundleDomains(cookies: readonly BundleCookie[]): string[] {
  return [...new Set(cookies.map((cookie) => cookie.domain.replace(/^\./, "").toLowerCase()))].sort(byteOrder);
}
