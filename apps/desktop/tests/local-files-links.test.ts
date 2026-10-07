/**
 * Deep links carry no path (ADP-005, DOMAIN.md 13.7 A): `quansio://attach[/thr_…]` asks main for a native dialog and names
 * nothing; a link that carries a path, a file URL or a path-like parameter is refused whole, never stripped and followed.
 */

import { describe, expect, it } from "vitest";

import { parseDeepLink } from "../src/main/deeplinks.js";

import { OTHER_THREAD, THREAD } from "./shell-fakes.js";

describe("the attach link", () => {
  it("asks for a dialog, for no thread or for a thread by its id", () => {
    expect(parseDeepLink("quansio://attach")).toEqual({ kind: "attach-files", threadId: null });
    expect(parseDeepLink("quansio://attach/")).toEqual({ kind: "attach-files", threadId: null });
    expect(parseDeepLink(`quansio://attach/${THREAD}`)).toEqual({ kind: "attach-files", threadId: THREAD });
    expect(parseDeepLink(`quansio://attach/${OTHER_THREAD}`)).toEqual({ kind: "attach-files", threadId: OTHER_THREAD });
  });

  it("refuses a link that carries a path, however the path is spelled", () => {
    const refused = [
      "quansio://attach?path=/etc/passwd",
      "quansio://attach?paths=/etc/passwd,/etc/hosts",
      "quansio://attach?file=/etc/passwd",
      "quansio://attach?files=%2Fetc%2Fpasswd",
      "quansio://attach?localFilePaths=%5B%22%2Fetc%2Fpasswd%22%5D",
      "quansio://attach?filePath=/etc/passwd",
      "quansio://attach?url=file:///etc/passwd",
      "quansio://attach?src=file:///etc/passwd",
      `quansio://attach/${THREAD}?localFilePaths=%2Fetc%2Fpasswd`,
      `quansio://attach/${THREAD}?path=/etc/passwd`,
      "quansio://attach/%2Fetc%2Fpasswd",
      "quansio://attach//etc/passwd",
      "quansio://attach/etc/passwd",
      "quansio://attach/../../etc/passwd",
      "quansio://attach/file:///etc/passwd",
      "quansio://attach/~/Documents",
      `quansio://attach/${THREAD}/etc/passwd`,
      `quansio://attach/${THREAD}#/etc/passwd`,
      "quansio://attach#localFilePaths=/etc/passwd",
      "quansio://user:pw@attach",
      "quansio://file/etc/passwd",
      "quansio://files/etc/passwd",
      "quansio://open?path=/etc/passwd",
      "quansio://attach/thr_short",
      `quansio://attach/run_${THREAD.slice(4)}`,
    ];
    for (const link of refused) {
      expect(parseDeepLink(link), link).toBeNull();
    }
  });

  it("a query on any other link is refused whole too, so no intent can smuggle a path", () => {
    for (const link of [
      `quansio://thread/${THREAD}?localFilePaths=/etc/passwd`,
      `quansio://thread/${THREAD}?path=/etc/passwd`,
      "quansio://run/run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA?file=/etc/passwd",
      "quansio://artifact/art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA?path=/tmp/x",
      "quansio://workspace/ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA?paths=/tmp",
      "quansio://auth/callback?code=c&state=s&path=/etc/passwd",
      "quansio://auth/callback?code=c&state=s&localFilePaths=/etc/passwd",
    ]) {
      expect(parseDeepLink(link), link).toBeNull();
    }
    // The OAuth return trip is unchanged.
    expect(parseDeepLink("quansio://auth/callback?code=c&state=s")).toEqual({ kind: "oauth-callback", code: "c", state: "s" });
  });
});
