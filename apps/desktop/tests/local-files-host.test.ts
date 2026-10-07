/**
 * The local-file host through the IPC layer (ADP-005, DOMAIN.md 13.7): grants claimed through the same guarded handlers
 * `app.ts` installs, per-window authority, schema validation of every payload, revocation when a window closes, and the
 * folder, preview, edit and watch channels working together over real files.
 *
 * Doubles: the windows (a fake factory), the native dialogs (a function that answers with what a person would pick)
 * and the upload (records what it was sent). The registries, the host, the IPC guard, the filesystem and, for the watch
 * tests, Node's real `fs.watch` and real timers are the shipped code.
 */

import { readFileSync, readFileSync as readConfig, renameSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { FILE_CHANNELS, PRELOAD_ALLOWLIST, type FileChannel, type PreloadChannel } from "../src/preload/bridge.js";
import { IpcGuard, registerGuardedHandlers, type WindowHandler } from "../src/main/ipc-trust.js";
import { LocalFilesHost, type LocalFileDialogs } from "../src/main/local-files/host.js";
import type { LocalFilesConfig } from "../src/main/local-files/config.js";
import { parseShellConfig } from "../src/main/shell-config.js";
import { WindowManager, type ManagedWindow } from "../src/main/windows.js";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { CONFIG, eventually, put, schedulerOf, scratch } from "./local-files-fixtures.js";
import { Clock, FakeDisplays, FakeFactory, FakeWindow, OTHER_THREAD, THREAD, ipcEvent } from "./shell-fakes.js";

const SHELL = parseShellConfig(readConfig(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "config", "desktop-shell.yaml"), "utf8"));

interface Upload {
  readonly name: string;
  readonly mediaType: string;
  readonly bytesBase64: string;
  readonly threadId?: string;
}

interface Answer {
  readonly ok: boolean;
  readonly result?: Record<string, unknown>;
  readonly error?: { code: string; message: string; details: Record<string, unknown> | null };
}

function rig(options: { config?: Partial<LocalFilesConfig>; realTime?: boolean; maxUploadBytes?: number; requireRequester?: boolean } = {}) {
  const factory = new FakeFactory();
  const clock = new Clock();
  const log: string[] = [];
  const windows = new WindowManager({ factory, platform: "darwin", preload: "/app/preload.cjs", config: SHELL, displays: new FakeDisplays(), log: (line) => log.push(line) });
  const main = windows.openMain();
  const opened = windows.openPopout({ threadId: THREAD, tab: "conversation", sidebar: false });
  const other = windows.openPopout({ threadId: OTHER_THREAD, tab: "conversation", sidebar: false });
  const quick = windows.quickEntry();
  const indicator = windows.indicator();
  if (!opened.ok || !other.ok || !quick || !indicator) {
    throw new Error("a window did not open");
  }
  // Every window has subscribed, so what main pushes is delivered rather than queued.
  for (const window of [main, opened.window, other.window, quick, indicator]) {
    windows.consumePending(window);
  }
  const dialogs = { picks: [] as (readonly string[] | null)[], folders: [] as (string | null)[], opened: 0, hold: null as null | (() => void), waiting: null as null | Promise<void> };
  const dialog: LocalFileDialogs = {
    pickFiles: async () => {
      dialogs.opened += 1;
      if (dialogs.waiting) {
        await dialogs.waiting;
      }
      return dialogs.picks.shift() ?? null;
    },
    pickFolder: () => {
      dialogs.opened += 1;
      return Promise.resolve(dialogs.folders.shift() ?? null);
    },
  };
  const uploads: Upload[] = [];
  const failures = new Set<string>();
  const bridge = {
    uploadAttachment: (raw: unknown) => {
      const upload = raw as Upload;
      uploads.push(upload);
      if (failures.has(upload.name)) {
        return Promise.reject(new Error(`the server refused ${upload.name}`));
      }
      return Promise.resolve({ artifact_id: `art_${upload.name}`, title: upload.name, size_bytes: Buffer.from(upload.bytesBase64, "base64").length });
    },
  };
  const config: LocalFilesConfig = { ...CONFIG, ...options.config };
  const home = scratch();
  const host = new LocalFilesHost({
    config,
    windows,
    dialogs: dialog,
    bridge,
    maxUploadBytes: options.maxUploadBytes ?? 8 * 1024 * 1024,
    log: (line) => log.push(line),
    home,
    ...(options.requireRequester === undefined ? {} : { requireMediaRequester: options.requireRequester }),
    ...(options.realTime ? {} : { now: () => clock.now, scheduler: schedulerOf(clock) }),
  });
  windows.onWindowEvent((window, event) => {
    if (event === "closed") {
      host.windowClosed(window);
    }
  });
  const fileHandlers = host.handlers();
  const handlers = Object.fromEntries(
    PRELOAD_ALLOWLIST.map((channel) => [channel, (FILE_CHANNELS as readonly string[]).includes(channel) ? fileHandlers[channel as FileChannel] : ((): Promise<unknown> => Promise.resolve({ ok: true, result: {} }))]),
  ) as Record<PreloadChannel, WindowHandler>;
  const registered = new Map<string, (event: unknown, payload: unknown) => unknown>();
  registerGuardedHandlers({ handle: (channel, listener) => registered.set(channel, listener) }, new IpcGuard(windows, (line) => log.push(line)), handlers);
  const invoke = async (channel: FileChannel, event: unknown, payload: unknown = {}): Promise<unknown> => Promise.resolve(registered.get(channel)?.(event, payload));
  const call = async (channel: FileChannel, window: ManagedWindow, payload: unknown = {}): Promise<Answer> => (await invoke(channel, ipcEvent(window.handle.webContentsId), payload)) as Answer;
  const sent = (window: ManagedWindow, channel: string): unknown[] =>
    (window.handle as FakeWindow).sent.filter((entry) => entry.channel === channel).map((entry) => entry.payload);
  return { windows, main, popout: opened.window, other: other.window, quick, indicator, host, dialogs, uploads, failures, call, invoke, sent, clock, log, home, config };
}

type Rig = ReturnType<typeof rig>;

const tokenOf = (answer: Answer): string => String(((answer.result?.["grant"] ?? {}) as { token?: string }).token);

describe("attaching files from disk", () => {
  it("opens a native dialog on request, holds what was picked, and attaches it through the existing upload on a claim", async () => {
    const r = rig();
    const dir = scratch();
    const a = put(dir, "notes/brief.txt", "the brief");
    const b = put(dir, "photo.png", Buffer.from([137, 80, 78, 71]));
    r.dialogs.picks.push([a, b]);
    const picked = await r.call("files.pick", r.main, { threadId: THREAD });
    expect(picked.ok).toBe(true);
    expect(r.dialogs.opened).toBe(1);
    const grant = picked.result?.["grant"] as { token: string; files: { name: string }[] };
    expect(grant.files.map((file) => file.name)).toEqual(["brief.txt", "photo.png"]);
    expect(JSON.stringify(picked)).not.toContain(dir);
    // Nothing is uploaded by picking.
    expect(r.uploads).toEqual([]);

    const attached = await r.call("files.grant.attach", r.main, { token: grant.token, threadId: THREAD });
    expect(attached).toMatchObject({ ok: true, result: { skipped: [] } });
    expect(r.uploads).toEqual([
      { name: "brief.txt", mediaType: "text/plain", bytesBase64: Buffer.from("the brief").toString("base64"), threadId: THREAD },
      { name: "photo.png", mediaType: "image/png", bytesBase64: Buffer.from([137, 80, 78, 71]).toString("base64"), threadId: THREAD },
    ]);
    expect((attached.result?.["attached"] as { artifact_id: string }[]).map((item) => item.artifact_id)).toEqual(["art_brief.txt", "art_photo.png"]);
    expect(r.host.grants.openDescriptors(), "every descriptor is closed after the attach").toBe(0);
  });

  it("cannot be claimed twice, and a replay uploads nothing", async () => {
    const r = rig();
    r.dialogs.picks.push([put(scratch(), "a.txt", "once")]);
    const token = tokenOf(await r.call("files.pick", r.main, {}));
    expect((await r.call("files.grant.attach", r.main, { token })).ok).toBe(true);
    expect(r.uploads).toHaveLength(1);
    const again = await r.call("files.grant.attach", r.main, { token });
    expect(again).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID", details: { reason: "invalid" } } });
    expect(r.uploads).toHaveLength(1);
  });

  it("cannot be claimed by another window, and the owner can still claim it afterwards", async () => {
    const r = rig();
    r.dialogs.picks.push([put(scratch(), "a.txt", "mine")]);
    const token = tokenOf(await r.call("files.pick", r.popout, { threadId: THREAD }));
    // Another pop-out, on its own thread, and the main window: neither holds this grant.
    expect(await r.call("files.grant.attach", r.other, { token, threadId: OTHER_THREAD })).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID" } });
    expect(await r.call("files.grant.attach", r.main, { token })).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID", details: { reason: "invalid" } } });
    expect(await r.call("files.grant.discard", r.main, { token })).toMatchObject({ ok: true, result: { discarded: false } });
    expect(await r.call("files.grant.preview", r.main, { token, index: 0 })).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID" } });
    expect(r.uploads).toEqual([]);
    expect(await r.call("files.grant.attach", r.popout, { token, threadId: THREAD })).toMatchObject({ ok: true });
    expect(r.uploads).toHaveLength(1);
  });

  it("answers an expired grant as expired to its owner, and invalid to anyone else", async () => {
    const r = rig();
    r.dialogs.picks.push([put(scratch(), "a.txt", "late")]);
    const token = tokenOf(await r.call("files.pick", r.main, {}));
    r.clock.advance(CONFIG.grantTtlMs - 1);
    expect(await r.call("files.grant.preview", r.main, { token, index: 0 })).toMatchObject({ ok: true });
    r.clock.advance(1);
    expect(await r.call("files.grant.attach", r.popout, { token, threadId: THREAD })).toMatchObject({ error: { details: { reason: "invalid" } } });
    expect(await r.call("files.grant.attach", r.main, { token })).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID" } });
    expect(r.uploads).toEqual([]);
  });

  it("reports a file the upload cannot take and goes on with the rest", async () => {
    const r = rig({ maxUploadBytes: 8 });
    const dir = scratch();
    r.failures.add("refused.txt");
    r.dialogs.picks.push([put(dir, "fine.txt", "12345678"), put(dir, "big.txt", "123456789"), put(dir, "empty.txt", ""), put(dir, "refused.txt", "x"), put(dir, "fine2.txt", "ok")]);
    const token = tokenOf(await r.call("files.pick", r.main, {}));
    const answer = await r.call("files.grant.attach", r.main, { token });
    expect(answer.ok).toBe(true);
    expect((answer.result?.["attached"] as { artifact_id: string }[]).map((item) => item.artifact_id)).toEqual(["art_fine.txt", "art_fine2.txt"]);
    expect(answer.result?.["skipped"]).toEqual([
      { name: "big.txt", reason: "too_large_for_upload", message: "attachments are limited to 8 bytes" },
      { name: "empty.txt", reason: "empty", message: "an empty file cannot be attached" },
      { name: "refused.txt", reason: "upload_failed", message: "the server refused refused.txt" },
    ]);
    expect(r.host.grants.openDescriptors()).toBe(0);
  });

  it("refuses a pick that breaks a rule, naming the file and holding nothing", async () => {
    const r = rig();
    const dir = scratch();
    put(dir, "real.txt", "r");
    symlinkSync(join(dir, "real.txt"), join(dir, "alias.txt"));
    r.dialogs.picks.push([join(dir, "real.txt"), join(dir, "alias.txt")]);
    expect(await r.call("files.pick", r.main, {})).toMatchObject({ ok: false, error: { code: "FILE_REFUSED", details: { reason: "symlink", name: "alias.txt" } } });
    r.dialogs.picks.push([dir]);
    expect(await r.call("files.pick", r.main, {})).toMatchObject({ ok: false, error: { code: "FILE_REFUSED", details: { reason: "not_regular", name: expect.any(String) as string } } });
    expect(r.host.grants.liveCount()).toBe(0);
    expect(r.host.grants.openDescriptors()).toBe(0);
    // Cancelling the dialog is not an error.
    r.dialogs.picks.push(null);
    expect(await r.call("files.pick", r.main, {})).toEqual({ ok: true, result: { grant: null } });
  });

  it("opens one dialog per window at a time: a request while one is open is dropped", async () => {
    const r = rig();
    let release: () => void = () => undefined;
    r.dialogs.waiting = new Promise<void>((resolve) => { release = resolve; });
    r.dialogs.picks.push([put(scratch(), "a.txt", "a")]);
    const first = r.call("files.pick", r.main, {});
    const second = await r.call("files.pick", r.main, {});
    expect(second).toMatchObject({ ok: false, error: { code: "RATE_LIMITED" } });
    // The deep link's request is dropped the same way, and is logged.
    await r.host.requestAttach(r.main, null);
    expect(r.dialogs.opened).toBe(1);
    expect(r.log.some((line) => line.includes("already open"))).toBe(true);
    // Another window is not held up by this one.
    r.dialogs.picks.push(null);
    release();
    expect((await first).ok).toBe(true);
    expect(await r.call("files.pick", r.main, {})).toMatchObject({ ok: true });
  });

  it("holds nothing for a window that closed while its dialog was open", async () => {
    const r = rig();
    let release: () => void = () => undefined;
    r.dialogs.waiting = new Promise<void>((resolve) => { release = resolve; });
    r.dialogs.picks.push([put(scratch(), "a.txt", "a")]);
    const pending = r.call("files.pick", r.popout, { threadId: THREAD });
    (r.popout.handle as FakeWindow).close();
    release();
    expect(await pending).toEqual({ ok: true, result: { grant: null } });
    expect(r.host.grants.liveCount()).toBe(0);
    expect(r.host.grants.openDescriptors()).toBe(0);
  });

  it("the attach deep link opens the dialog and hands the page its grant, never a path", async () => {
    const r = rig();
    r.dialogs.picks.push([put(scratch(), "a.txt", "a")]);
    await r.host.requestAttach(r.main, THREAD);
    const pushes = r.sent(r.main, "files.grant") as { grant: { token: string; files: unknown[] }; threadId: string }[];
    expect(pushes).toHaveLength(1);
    expect(pushes[0]?.threadId).toBe(THREAD);
    expect(pushes[0]?.grant.token).toMatch(/^[0-9a-f]{64}$/);
    expect(pushes[0]?.grant.files).toHaveLength(1);
    // The window that received it can claim it; another cannot.
    expect(await r.call("files.grant.attach", r.popout, { token: pushes[0]?.grant.token, threadId: THREAD })).toMatchObject({ ok: false });
    expect(await r.call("files.grant.attach", r.main, { token: pushes[0]?.grant.token, threadId: THREAD })).toMatchObject({ ok: true });
    // A cancelled dialog sends nothing.
    r.dialogs.picks.push(null);
    await r.host.requestAttach(r.main, null);
    expect(r.sent(r.main, "files.grant")).toHaveLength(1);
  });
});

describe("no channel takes a path", () => {
  it("rejects a path where an id is expected, and any field the channel does not name, before anything runs", async () => {
    const r = rig();
    const probes: [FileChannel, unknown][] = [
      ["files.pick", { path: "/etc/passwd" }],
      ["files.pick", { paths: ["/etc/passwd"] }],
      ["files.pick", { threadId: "/etc/passwd" }],
      ["files.grant.attach", { token: "/etc/passwd" }],
      ["files.grant.attach", { token: "0".repeat(64), path: "/etc/passwd" }],
      ["files.grant.preview", { token: "0".repeat(64), index: 0, path: "/etc/passwd" }],
      ["files.root.pick", { path: "/etc" }],
      ["files.root.release", { rootId: "/etc" }],
      ["files.root.entries", { rootId: `lroot_${"0".repeat(32)}`, path: "/etc" }],
      ["files.root.entries", { rootId: `lroot_${"0".repeat(32)}`, entryId: "../../etc" }],
      ["files.media.url", { entryId: "/etc/passwd" }],
      ["files.media.url", { entryId: `ent_${"0".repeat(32)}`, path: "/etc/passwd" }],
      ["files.edit.open", { entryId: "/etc/passwd" }],
      ["files.edit.open", { path: "/etc/passwd" }],
      ["files.edit.save", { editId: `edit_${"0".repeat(32)}`, text: "x", onConflict: "fail", path: "/etc/passwd" }],
      ["files.edit.save", { editId: `edit_${"0".repeat(32)}`, text: "x", onConflict: "force" }],
      ["files.edit.close", { editId: "../x" }],
      ["files.watch.subscribe", { rootId: `lroot_${"0".repeat(32)}`, entryId: "/etc" }],
      ["files.watch.subscribe", { path: "/etc" }],
      ["files.watch.unsubscribe", { subscriptionId: "/etc" }],
    ];
    for (const [channel, payload] of probes) {
      expect(await r.call(channel, r.main, payload), `${channel} ${JSON.stringify(payload)}`).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    }
    expect(r.dialogs.opened).toBe(0);
  });

  it("is a source-level fact too: no payload schema in the host names a path", () => {
    const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "main", "local-files", "host.ts"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");
    for (const schemaLine of source.split("\n").filter((line) => /^const [A-Z_]+ = s\./.test(line))) {
      expect(schemaLine, schemaLine).not.toMatch(/\b(path|paths|file|files|filePath|localFilePaths|directory|folder|url)\b\s*:/);
    }
  });
});

describe("authority per window type", () => {
  it("a pop-out asks for a dialog for its own thread only, and reaches no folder, edit or watch channel", async () => {
    const r = rig();
    r.dialogs.picks.push(null);
    expect(await r.call("files.pick", r.popout, { threadId: OTHER_THREAD })).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    expect(await r.call("files.pick", r.popout, {})).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    expect(await r.call("files.grant.attach", r.popout, { token: "0".repeat(64), threadId: OTHER_THREAD })).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    expect(r.dialogs.opened).toBe(0);
    expect(await r.call("files.pick", r.popout, { threadId: THREAD })).toMatchObject({ ok: true });
    for (const channel of ["files.root.pick", "files.root.list", "files.root.release", "files.root.entries", "files.media.url", "files.edit.open", "files.edit.save", "files.edit.close", "files.watch.subscribe", "files.watch.unsubscribe"] as const) {
      expect(await r.call(channel, r.popout, {}), channel).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    }
  });

  it("quick entry and the indicator reach none of them, and no dialog opens", async () => {
    const r = rig();
    for (const window of [r.quick, r.indicator]) {
      for (const channel of FILE_CHANNELS) {
        expect(await r.call(channel, window, {}), `${window.type} ${channel}`).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
      }
    }
    expect(r.dialogs.opened).toBe(0);
  });

  it("an untrusted sender is a logged no-op on every channel: no dialog, no answer", async () => {
    const r = rig();
    r.dialogs.picks.push([put(scratch(), "a.txt", "a")], [put(scratch(), "b.txt", "b")]);
    const id = r.main.handle.webContentsId;
    for (const event of [
      ipcEvent(id, "file:///tmp/evil.html"),
      ipcEvent(id, "https://evil.example/"),
      ipcEvent(id, "data:text/html,<p>hi</p>"),
      ipcEvent(id, "about:blank"),
      // A subframe of the trusted page: its parent is not null.
      ipcEvent(id, "app://desktop/index.html", { url: "app://desktop/index.html" }),
      // A window the shell did not create, from a trusted-looking URL.
      ipcEvent(9999),
      ipcEvent(id, null),
    ]) {
      for (const channel of FILE_CHANNELS) {
        expect(await r.invoke(channel, event, { threadId: null }), `${channel} from ${JSON.stringify(event)}`).toBeUndefined();
      }
    }
    expect(r.dialogs.opened).toBe(0);
    expect(r.log.filter((line) => line.includes("ignored files.")).length).toBeGreaterThan(0);
  });
});

describe("switched off", () => {
  it("answers every channel POLICY_DENIED, opens no dialog and serves no media", async () => {
    const r = rig({ config: { enabled: false } });
    r.dialogs.picks.push([put(scratch(), "a.txt", "a")]);
    for (const channel of FILE_CHANNELS) {
      expect(await r.call(channel, r.main, {}), channel).toMatchObject({ ok: false, error: { code: "POLICY_DENIED" } });
    }
    expect(r.dialogs.opened).toBe(0);
    await r.host.requestAttach(r.main, null);
    expect(r.dialogs.opened).toBe(0);
    expect(r.host.mediaResponse({ method: "GET", url: `quansio-media://m/${"0".repeat(64)}/a.txt`, range: null }).status).toBe(404);
  });
});

describe("folders, previews, edits and watches", () => {
  async function granted(r: Rig, directory: string): Promise<{ rootId: string; entries: { entryId: string; name: string; kind: string }[] }> {
    r.dialogs.folders.push(directory);
    const root = await r.call("files.root.pick", r.main, {});
    const rootId = (root.result?.["root"] as { rootId: string }).rootId;
    const listing = await r.call("files.root.entries", r.main, { rootId, entryId: null });
    return { rootId, entries: (listing.result?.["entries"] as { entryId: string; name: string; kind: string }[]) };
  }

  it("lists a granted folder by opaque ids, previews a file by a media URL and edits it with a conflict check", async () => {
    const r = rig();
    const base = scratch();
    const folder = join(base, "project");
    const path = put(folder, "readme.md", "# hello\n");
    put(folder, "image.png", "PNGDATA");
    const { rootId, entries } = await granted(r, folder);
    expect(entries.map((entry) => entry.name)).toEqual(["image.png", "readme.md"]);
    expect(JSON.stringify(entries)).not.toContain(base);
    const readme = entries.find((entry) => entry.name === "readme.md")?.entryId ?? "";
    const image = entries.find((entry) => entry.name === "image.png")?.entryId ?? "";

    // A preview is a capability URL; GET serves it and it names no path.
    const url = (await r.call("files.media.url", r.main, { entryId: image })).result?.["url"] as string;
    expect(url).toMatch(/^quansio-media:\/\/m\/[0-9a-f]{64}\/image\.png$/);
    const served = r.host.mediaResponse({ method: "GET", url, range: null });
    expect([served.status, served.headers["Content-Type"]]).toEqual([200, "image/png"]);

    // An edit: open, save, an outside change, a conflict, an overwrite with a backup.
    const opened = await r.call("files.edit.open", r.main, { entryId: readme });
    expect(opened).toMatchObject({ ok: true, result: { text: "# hello\n" } });
    const editId = String(opened.result?.["editId"]);
    expect(await r.call("files.edit.save", r.main, { editId, text: "# hello, edited\n", onConflict: "fail" })).toMatchObject({ ok: true, result: { backup: null } });
    expect(readFileSync(path, "utf8")).toBe("# hello, edited\n");
    writeFileSync(path, "# changed by someone else, longer than before\n");
    const conflict = await r.call("files.edit.save", r.main, { editId, text: "# mine\n", onConflict: "fail" });
    expect(conflict).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT" } });
    expect(readFileSync(path, "utf8")).toBe("# changed by someone else, longer than before\n");
    expect(await r.call("files.edit.save", r.main, { editId, text: "# mine\n", onConflict: "overwrite" })).toMatchObject({ ok: true, result: { backup: "readme.md.quansio-backup-1" } });
    expect(await r.call("files.edit.close", r.main, { editId })).toMatchObject({ ok: true, result: { closed: true } });
    expect(await r.call("files.root.release", r.main, { rootId })).toMatchObject({ ok: true, result: { released: true } });
    // Releasing the root revoked the media URL and the edit sessions.
    expect(r.host.mediaResponse({ method: "GET", url, range: null }).status).toBe(404);
    expect(await r.call("files.edit.open", r.main, { entryId: readme })).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
  });

  it("a path outside the granted roots cannot be named, listed, previewed, edited or watched", async () => {
    const r = rig();
    const base = scratch();
    const folder = join(base, "project");
    put(folder, "inside.txt", "inside");
    put(folder, "dir/inner.txt", "inner");
    const outside = put(base, "outside/secret.txt", "secret");
    const { rootId, entries } = await granted(r, folder);
    const file = entries.find((entry) => entry.name === "inside.txt")?.entryId ?? "";
    const dir = entries.find((entry) => entry.name === "dir")?.entryId ?? "";
    const stranger = `ent_${"1".repeat(32)}`;
    for (const [channel, payload] of [
      ["files.root.entries", { rootId, entryId: stranger }],
      ["files.media.url", { entryId: stranger }],
      ["files.edit.open", { entryId: stranger }],
      ["files.watch.subscribe", { rootId, entryId: stranger }],
      ["files.root.entries", { rootId: `lroot_${"2".repeat(32)}`, entryId: null }],
      ["files.watch.subscribe", { rootId: `lroot_${"2".repeat(32)}`, entryId: null }],
    ] as const) {
      expect(await r.call(channel, r.main, payload), channel).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    }
    // A file is not a folder to watch or list.
    expect(await r.call("files.watch.subscribe", r.main, { rootId, entryId: file })).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(await r.call("files.root.entries", r.main, { rootId, entryId: file })).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    // The folder is replaced by a link that leaves the root: it is outside now, for a listing and a watch.
    renameSync(join(folder, "dir"), join(base, "moved"));
    symlinkSync(join(base, "outside"), join(folder, "dir"));
    expect(await r.call("files.root.entries", r.main, { rootId, entryId: dir })).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(await r.call("files.watch.subscribe", r.main, { rootId, entryId: dir })).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(readFileSync(outside, "utf8")).toBe("secret");
    expect(r.host.watcher.watchCount()).toBe(0);
  });

  it("another window cannot use what this window was given", async () => {
    const r = rig();
    const folder = join(scratch(), "project");
    put(folder, "a.txt", "a");
    const { rootId, entries } = await granted(r, folder);
    const entryId = entries[0]?.entryId ?? "";
    // Only `main` reaches these channels, so the other holder is a second host-level window id: ask the registries directly.
    const otherId = r.popout.handle.webContentsId;
    expect(r.host.roots.resolve(otherId, entryId)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(r.host.roots.list(otherId, rootId, null)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(r.host.edits.open(otherId, entryId)).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(r.host.watcher.subscriberCount()).toBe(0);
  });

  it("delivers typed changes to the window that watches, with entry ids it can open, and is silent about its own saves", async () => {
    const r = rig({ realTime: true, config: { watchDebounceMs: 60 } });
    const folder = join(scratch(), "project");
    const path = put(folder, "doc.txt", "one");
    const { rootId, entries } = await granted(r, folder);
    const subscribed = await r.call("files.watch.subscribe", r.main, { rootId, entryId: null });
    expect(subscribed.ok).toBe(true);
    const subscriptionId = String(subscribed.result?.["subscriptionId"]);
    try {
      // Our own save is not news.
      const opened = await r.call("files.edit.open", r.main, { entryId: entries[0]?.entryId });
      expect((await r.call("files.edit.save", r.main, { editId: String(opened.result?.["editId"]), text: "two, saved by us", onConflict: "fail" })).ok).toBe(true);
      await new Promise((resolve) => setTimeout(resolve, 500));
      expect(r.sent(r.main, "files.changes"), "the change our own save made was not reported back").toEqual([]);
      // Someone else's write is.
      writeFileSync(join(folder, "new.txt"), "added by someone else");
      writeFileSync(path, "three, written by someone else, longer");
      const reports = await eventually(() => {
        const all = r.sent(r.main, "files.changes") as { subscriptionId: string; changes?: { kind: string; name: string; entryId: string | null }[] }[];
        return all.length > 0 ? all : undefined;
      }, "a change report");
      const changes = reports.flatMap((report) => report.changes ?? []);
      expect(reports[0]?.subscriptionId).toBe(subscriptionId);
      expect(changes.map((change) => `${change.kind}:${change.name}`).sort()).toEqual(["add:new.txt", "change:doc.txt"]);
      // The ids in a report are real: the window can open what was added.
      const added = changes.find((change) => change.name === "new.txt");
      expect(await r.call("files.edit.open", r.main, { entryId: added?.entryId })).toMatchObject({ ok: true, result: { text: "added by someone else" } });
      expect(await r.call("files.watch.unsubscribe", r.main, { subscriptionId })).toMatchObject({ ok: true, result: { unsubscribed: true } });
      expect(r.host.watcher.watchCount()).toBe(0);
    } finally {
      r.host.dispose();
    }
  });
});

describe("a window that closes", () => {
  it("loses everything held for it: grants and their files, roots, entries, media tokens, edit sessions and watches", async () => {
    const r = rig();
    const dir = scratch();
    const folder = join(dir, "project");
    put(folder, "doc.txt", "x");
    r.dialogs.picks.push([put(dir, "a.txt", "a")]);
    const token = tokenOf(await r.call("files.pick", r.popout, { threadId: THREAD }));
    const preview = (await r.call("files.grant.preview", r.popout, { token, index: 0 })).result?.["url"] as string;
    r.dialogs.folders.push(folder);
    const root = (await r.call("files.root.pick", r.main, {})).result?.["root"] as { rootId: string };
    const entries = ((await r.call("files.root.entries", r.main, { rootId: root.rootId, entryId: null })).result?.["entries"] as { entryId: string }[]);
    const entryId = entries[0]?.entryId ?? "";
    const mediaUrl = (await r.call("files.media.url", r.main, { entryId })).result?.["url"] as string;
    const edit = await r.call("files.edit.open", r.main, { entryId });
    await r.call("files.watch.subscribe", r.main, { rootId: root.rootId, entryId: null });
    expect([r.host.grants.liveCount(), r.host.media.count(), r.host.edits.count(), r.host.watcher.watchCount()]).toEqual([1, 2, 1, 1]);
    expect(r.host.mediaResponse({ method: "GET", url: preview, range: null }).status).toBe(200);

    // The pop-out closes: its grant, the media URL made from it and its descriptors go.
    (r.popout.handle as FakeWindow).close();
    expect(r.host.grants.liveCount()).toBe(0);
    expect(r.host.grants.openDescriptors()).toBe(0);
    expect(r.host.mediaResponse({ method: "GET", url: preview, range: null }).status).toBe(404);
    expect(r.host.mediaResponse({ method: "GET", url: mediaUrl, range: null }).status).toBe(200);

    // The main window closes: everything else goes.
    (r.main.handle as FakeWindow).close();
    expect([r.host.grants.liveCount(), r.host.media.count(), r.host.edits.count(), r.host.watcher.watchCount(), r.host.roots.rootsOf(r.main.handle.webContentsId).length]).toEqual([0, 0, 0, 0, 0]);
    expect(r.host.mediaResponse({ method: "GET", url: mediaUrl, range: null }).status).toBe(404);
    expect(r.host.roots.resolve(r.main.handle.webContentsId, entryId)).toMatchObject({ ok: false });
    expect(edit.ok).toBe(true);
  });
});
