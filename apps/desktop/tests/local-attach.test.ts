/**
 * The composer's side of a file grant (ADP-005): the page learns names and a token, asks main to attach, and gets
 * artifacts back. Against a double of the preload API that answers the way main's handlers do (`local-files-host.test.ts`
 * holds those answers to the real host).
 */

import { describe, expect, it } from "vitest";

import { attachFromDisk, attachPushedGrant, type LocalAttachApi } from "../src/conversation/local-attach.js";
import { STRINGS } from "../src/i18n/strings.js";

import { OTHER_THREAD, THREAD } from "./shell-fakes.js";

const TOKEN = "ab".repeat(32);

function api(script: { pick?: unknown; attach?: unknown }) {
  const calls: { channel: string; payload: unknown }[] = [];
  const client: LocalAttachApi = {
    pickLocalFiles: (request) => {
      calls.push({ channel: "pick", payload: request });
      return Promise.resolve(script.pick ?? { ok: true, result: { grant: null } });
    },
    attachLocalFiles: (request) => {
      calls.push({ channel: "attach", payload: request });
      return Promise.resolve(script.attach);
    },
    discardLocalFiles: (token) => {
      calls.push({ channel: "discard", payload: token });
      return Promise.resolve({ ok: true, result: { discarded: true } });
    },
  };
  return { client, calls };
}

const grant = { ok: true, result: { grant: { token: TOKEN, expiresAt: 1, totalBytes: 9, files: [{ index: 0, name: "brief.txt", sizeBytes: 9, mediaType: "text/plain" }] } } };
const uploaded = { ok: true, result: { attached: [{ artifact_id: "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", title: "brief.txt", size_bytes: 9 }], skipped: [] } };

describe("attaching from disk", () => {
  it("asks for a dialog, claims what was picked for the thread, and returns composer attachments", async () => {
    const { client, calls } = api({ pick: grant, attach: uploaded });
    const outcome = await attachFromDisk(client, THREAD);
    expect(outcome).toEqual({ kind: "attached", attachments: [{ artifactId: "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", title: "brief.txt", sizeBytes: 9 }], notices: [] });
    expect(calls).toEqual([
      { channel: "pick", payload: { threadId: THREAD } },
      { channel: "attach", payload: { token: TOKEN, threadId: THREAD } },
    ]);
  });

  it("a cancelled dialog attaches nothing and says nothing", async () => {
    const { client, calls } = api({});
    expect(await attachFromDisk(client, null)).toEqual({ kind: "cancelled" });
    expect(calls.map((call) => call.channel)).toEqual(["pick"]);
  });

  it("says what main refused, and that an expired selection must be picked again", async () => {
    const refused = { ok: false, error: { code: "FILE_REFUSED", message: "a link to a file cannot be attached; pick the file itself", details: { reason: "symlink" } } };
    expect(await attachFromDisk(api({ pick: refused }).client, null)).toEqual({ kind: "failed", message: "a link to a file cannot be attached; pick the file itself" });
    const expired = { ok: false, error: { code: "FILE_GRANT_INVALID", message: "that file grant expired", details: { reason: "expired" } } };
    expect(await attachFromDisk(api({ pick: grant, attach: expired }).client, null)).toEqual({ kind: "failed", message: STRINGS.attachGrantExpired });
  });

  it("reports the files that were not attached by name, next to the ones that were", async () => {
    const partial = {
      ok: true,
      result: { attached: uploaded.result.attached, skipped: [{ name: "huge.bin", reason: "too_large_for_upload", message: "attachments are limited to 8388608 bytes" }] },
    };
    const outcome = await attachFromDisk(api({ pick: grant, attach: partial }).client, THREAD);
    expect(outcome).toMatchObject({ kind: "attached", notices: ["huge.bin: attachments are limited to 8388608 bytes"] });
  });
});

describe("a grant a deep link caused", () => {
  it("is attached to the open thread, or to the thread the link named", async () => {
    const { client, calls } = api({ attach: uploaded });
    expect(await attachPushedGrant(client, { grant: grant.result.grant, threadId: null }, THREAD)).toMatchObject({ kind: "attached" });
    expect(await attachPushedGrant(client, { grant: grant.result.grant, threadId: THREAD }, THREAD)).toMatchObject({ kind: "attached" });
    expect(calls.every((call) => call.channel === "attach")).toBe(true);
  });

  it("is discarded at once when it was for another thread, not left to expire", async () => {
    const { client, calls } = api({ attach: uploaded });
    expect(await attachPushedGrant(client, { grant: grant.result.grant, threadId: OTHER_THREAD }, THREAD)).toEqual({ kind: "cancelled" });
    expect(calls).toEqual([{ channel: "discard", payload: TOKEN }]);
  });

  it("reports a refusal main pushed, and ignores a push with nothing in it", async () => {
    const { client } = api({});
    const error = { code: "RATE_LIMITED", message: "a file dialog is already open", retryable: false, details: null };
    expect(await attachPushedGrant(client, { error, threadId: null }, THREAD)).toEqual({ kind: "failed", message: "a file dialog is already open" });
    expect(await attachPushedGrant(client, {}, THREAD)).toEqual({ kind: "cancelled" });
    expect(await attachPushedGrant(client, null, THREAD)).toEqual({ kind: "cancelled" });
  });
});
