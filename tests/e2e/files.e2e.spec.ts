/**
 * ADP-005 local files, in the real Electron binary running the built desktop app against the real server on a scratch
 * PostgreSQL (`harness.ts`): the preload, the IPC guard, the media protocol, the content security policy and the
 * windows are the shipped ones; the files and folders are real files on this machine's disk.
 *
 * What a person's click on a native dialog would do is the one thing not driven here: the dialogs are answered by the
 * shell's test hook (`__quansioShell.hooks.dialogs`), because Electron's `dialog` cannot be operated headlessly. Everything
 * after the dialog answers, and everything before it, runs the production code.
 *
 * Without the dev stack, the server binaries or a macOS Electron binary the suite is reported `BLOCKED_EXTERNAL`
 * (harness.ts). The attach test's last hop, the artifact bytes reaching the object store, needs MinIO; when it is not
 * running the server refuses the upload and the test says so (`BLOCKED_EXTERNAL`) rather than counting it as passed.
 */

import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import type { PreloadApi } from "../../apps/desktop/src/preload/bridge.js";

import { desktopBlocked, emailOf, launchDesktop, OWNER, signInDesktop, Stack, waitFor, type Desktop } from "./harness.js";

declare global {
  interface Window {
    readonly quansio: PreloadApi;
    __changes?: unknown[];
    __grants?: unknown[];
  }
}

const blocked = desktopBlocked();

/** A real 1x1 PNG. */
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", "base64");

interface Answer {
  ok: boolean;
  result?: Record<string, unknown>;
  error?: { code: string; message: string; details: Record<string, unknown> | null };
}

interface Shell {
  files: {
    grants: { liveCount(): number; openDescriptors(): number };
    media: { count(windowId?: number): number };
    roots: { rootsOf(windowId: number): unknown[] };
    edits: { count(): number };
    watcher: { watchCount(): number };
  };
  hooks: { dialogs: { pickFiles: () => Promise<string[] | null>; pickFolder: () => Promise<string | null> } };
  openLink: (url: string) => void;
  queue?: { files: (string[] | null)[]; folders: (string | null)[]; opened: number };
  windows: { ofType(type: string): { handle: { webContentsId: number } }[] };
}

describe.skipIf(blocked !== null)("local files (real Electron, real server)", () => {
  let stack: Stack;
  let desktop: Desktop;
  let mainThread = "";
  let scratch = "";

  const main = (): Desktop["page"] => desktop.page;
  const windowOf = (part: string) => desktop.app.windows().find((page) => page.url().includes(part));

  /** What a person would pick in the next native dialog. */
  async function willPick(options: { files?: string[] | null; folder?: string | null }): Promise<void> {
    await desktop.app.evaluate((_electron, picks) => {
      const shell = (globalThis as unknown as { __quansioShell: Shell }).__quansioShell;
      const queue = (shell.queue ??= { files: [], folders: [], opened: 0 });
      shell.hooks.dialogs.pickFiles = () => {
        queue.opened += 1;
        return Promise.resolve(queue.files.shift() ?? null);
      };
      shell.hooks.dialogs.pickFolder = () => {
        queue.opened += 1;
        return Promise.resolve(queue.folders.shift() ?? null);
      };
      if (picks.files !== undefined) {
        queue.files.push(picks.files);
      }
      if (picks.folder !== undefined) {
        queue.folders.push(picks.folder);
      }
    }, options);
  }

  const dialogsOpened = (): Promise<number> => desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.queue?.opened ?? 0);
  const held = (): Promise<{ grants: number; descriptors: number; media: number; edits: number; watches: number }> =>
    desktop.app.evaluate(() => {
      const { files } = (globalThis as unknown as { __quansioShell: Shell }).__quansioShell;
      return { grants: files.grants.liveCount(), descriptors: files.grants.openDescriptors(), media: files.media.count(), edits: files.edits.count(), watches: files.watcher.watchCount() };
    });

  /**
   * Whether an `<img>` in `page` loads the URL. A query string is added so the page's own image cache cannot answer for a
   * request that would otherwise have been refused (the token is the path; the query is ignored by the handler).
   */
  const loads = (page: Desktop["page"], url: string): Promise<boolean> =>
    page.evaluate(
      (target) =>
        new Promise<boolean>((resolve) => {
          const image = new Image();
          image.onload = () => { resolve(image.naturalWidth > 0); };
          image.onerror = () => { resolve(false); };
          image.src = `${target}?fresh=${String(Math.random())}`;
        }),
      url,
    );

  beforeAll(async () => {
    scratch = mkdtempSync(join(tmpdir(), "adp005-e2e-"));
    stack = await Stack.start("files");
    desktop = await launchDesktop(stack.origin, { env: { QUANSIO_DESKTOP_TEST_HOOKS: "1", QUANSIO_DESKTOP_SIGNED: "1" } });
    await signInDesktop(desktop.page, stack, emailOf(OWNER));
    const first = (await main().evaluate(() => window.quansio.postMessage({ content: "hello from the main window" }))) as { thread_id: string };
    mainThread = first.thread_id;
    await willPick({});
  }, 240_000);

  afterAll(async () => {
    await desktop.close();
    await stack.drop();
    rmSync(scratch, { recursive: true, force: true });
  }, 60_000);

  it("a grant is bound to the window that asked: another window cannot claim it, preview it or load its media URL", async () => {
    const picture = join(scratch, "pictures", "photo.png");
    mkdirSync(join(scratch, "pictures"), { recursive: true });
    writeFileSync(picture, PNG);
    await main().evaluate((thread) => window.quansio.openPopout({ threadId: thread }), mainThread);
    const popout = await waitFor(() => windowOf("w=popout"), "the pop-out window");
    await popout.getByText("hello from the main window").waitFor({ timeout: 30_000 });

    await willPick({ files: [picture] });
    const picked = (await main().evaluate((thread) => window.quansio.pickLocalFiles({ threadId: thread }), mainThread)) as Answer;
    expect(picked.ok).toBe(true);
    const grant = picked.result?.["grant"] as { token: string; files: { name: string; sizeBytes: number; mediaType: string }[] };
    expect(grant.token).toMatch(/^[0-9a-f]{64}$/);
    expect(grant.files).toEqual([{ index: 0, name: "photo.png", sizeBytes: PNG.length, mediaType: "image/png" }]);
    expect(JSON.stringify(picked)).not.toContain(scratch);

    // A preview URL for the picked file loads in the window it was made for...
    const preview = (await main().evaluate((token) => window.quansio.previewLocalFile(token, 0), grant.token)) as Answer;
    const url = preview.result?.["url"] as string;
    expect(url).toMatch(/^quansio-media:\/\/m\/[0-9a-f]{64}\/photo\.png$/);
    expect(url).not.toContain(scratch);
    expect(await loads(main(), url), "the window it was made for shows it").toBe(true);
    // ...and not in another window, though it knows the URL: the request is cancelled before the file is touched.
    expect(await loads(popout, url), "another window does not").toBe(false);
    // Nor in the main process itself, which is no window at all.
    const fromMain = await desktop.app.evaluate(async ({ net }, target) => {
      try {
        return `status ${String((await net.fetch(target)).status)}`;
      } catch {
        return "refused";
      }
    }, url);
    expect(fromMain).toBe("refused");
    // The page cannot read the bytes with fetch either (a different origin without CORS): it can only embed them.
    const fetched = await main().evaluate((target) => fetch(target).then(() => "read", () => "refused"), url);
    expect(fetched).toBe("refused");

    // The other window's claim, preview and discard of the same token are all the same refusal.
    const stolen = (await popout.evaluate((args) => window.quansio.attachLocalFiles(args), { token: grant.token, threadId: mainThread })) as Answer;
    expect(stolen).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID", details: { reason: "invalid" } } });
    expect(await popout.evaluate((token) => window.quansio.previewLocalFile(token, 0), grant.token)).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID" } });
    expect(await popout.evaluate((token) => window.quansio.discardLocalFiles(token), grant.token)).toMatchObject({ ok: true, result: { discarded: false } });
    expect((await held()).grants).toBe(1);

    // The owner claims it once; a second claim is refused; and claiming revoked the media URL.
    const attached = (await main().evaluate((token) => window.quansio.attachLocalFiles({ token }), grant.token)) as Answer;
    expect(attached.ok).toBe(true);
    const skipped = attached.result?.["skipped"] as { name: string; reason: string; message: string }[];
    const uploaded = attached.result?.["attached"] as unknown[];
    expect(uploaded.length + skipped.length).toBe(1);
    if (skipped.length > 0) {
      // The server's own refusal of the upload (no object store on this machine): the claim worked, the last hop is blocked.
      expect(skipped[0]).toMatchObject({ name: "photo.png", reason: "upload_failed" });
      console.log(`BLOCKED_EXTERNAL: the upload of the claimed file was refused by the server (${skipped[0]?.message ?? ""}); the object store is not running`);
    } else {
      expect(uploaded[0]).toMatchObject({ title: "photo.png", size_bytes: PNG.length });
    }
    expect(await main().evaluate((token) => window.quansio.attachLocalFiles({ token }), grant.token)).toMatchObject({ ok: false, error: { code: "FILE_GRANT_INVALID" } });
    expect(await loads(main(), url), "claiming the grant revoked its media URL").toBe(false);
    expect(await held()).toMatchObject({ grants: 0, descriptors: 0, media: 0 });
  });

  it("a grant is gone with its window: closing a pop-out revokes what it held, and no other window's", async () => {
    const popout = windowOf("w=popout") ?? (await waitFor(() => windowOf("w=popout"), "the pop-out window"));
    const file = join(scratch, "note.txt");
    writeFileSync(file, "a note");
    await willPick({ files: [file] });
    const picked = (await popout.evaluate((thread) => window.quansio.pickLocalFiles({ threadId: thread }), mainThread)) as Answer;
    const token = (picked.result?.["grant"] as { token: string }).token;
    const preview = (await popout.evaluate((value) => window.quansio.previewLocalFile(value, 0), token)) as Answer;
    expect(preview.ok).toBe(true);
    expect(await held()).toMatchObject({ grants: 1, descriptors: 1, media: 1 });
    // Another window's grant stays.
    await willPick({ files: [file] });
    const other = (await main().evaluate((thread) => window.quansio.pickLocalFiles({ threadId: thread }), mainThread)) as Answer;
    expect(other.ok).toBe(true);
    expect((await held()).grants).toBe(2);

    await popout.evaluate(() => window.quansio.closeWindow()).catch(() => undefined);
    await waitFor(async () => (await held()).grants === 1, "the pop-out's grant to be revoked");
    expect(await held()).toMatchObject({ grants: 1, descriptors: 1, media: 0 });
    const otherToken = (other.result?.["grant"] as { token: string }).token;
    expect(await main().evaluate((value) => window.quansio.discardLocalFiles(value), otherToken)).toMatchObject({ ok: true, result: { discarded: true } });
    expect(await held()).toMatchObject({ grants: 0, descriptors: 0 });
  });

  it("a grant that is refused leaves nothing held, and the renderer cannot reach the folder, edit or watch channels from a pop-out", async () => {
    const folder = join(scratch, "a-folder");
    mkdirSync(folder, { recursive: true });
    await willPick({ files: [folder] });
    expect(await main().evaluate(() => window.quansio.pickLocalFiles({}))).toMatchObject({ ok: false, error: { code: "FILE_REFUSED", details: { reason: "not_regular" } } });
    expect(await held()).toMatchObject({ grants: 0, descriptors: 0 });

    await main().evaluate((thread) => window.quansio.openPopout({ threadId: thread }), mainThread);
    const popout = await waitFor(() => windowOf("w=popout"), "the pop-out window");
    const opened = await dialogsOpened();
    expect(await popout.evaluate(() => window.quansio.pickFolder())).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    expect(await popout.evaluate(() => window.quansio.openFileForEdit(`ent_${"0".repeat(32)}`))).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    expect(await popout.evaluate(() => window.quansio.watchFolder(`lroot_${"0".repeat(32)}`))).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    // A pop-out asks for a dialog for its own thread only.
    expect(await popout.evaluate(() => window.quansio.pickLocalFiles({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ" }))).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    expect(await dialogsOpened(), "no dialog opened for a refused call").toBe(opened);
    await popout.evaluate(() => window.quansio.closeWindow()).catch(() => undefined);
  });

  it("no call takes a path: anything but an opaque id is refused before it reaches a handler", async () => {
    const answers = (await main().evaluate(async () => {
      const q = window.quansio as unknown as Record<string, (...args: unknown[]) => Promise<unknown>>;
      return Promise.all([
        q["openFileForEdit"]?.("/etc/passwd"),
        q["fileMediaUrl"]?.("../../etc/passwd"),
        q["folderEntries"]?.("/", null),
        q["watchFolder"]?.("/etc"),
        q["attachLocalFiles"]?.({ token: "/etc/passwd" }),
        q["pickLocalFiles"]?.({ path: "/etc/passwd" }),
      ]);
    })) as Answer[];
    for (const answer of answers) {
      expect(answer).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    }
    // An id nobody was given is outside every root.
    expect(await main().evaluate(() => window.quansio.fileMediaUrl(`ent_${"a".repeat(32)}`))).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(await main().evaluate(() => window.quansio.folderEntries(`lroot_${"a".repeat(32)}`, null))).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
  });

  it("the attach deep link opens the dialog and hands the page a grant; a link with a path is refused and opens nothing", async () => {
    const file = join(scratch, "linked.txt");
    writeFileSync(file, "attached by a link");
    await main().evaluate(() => {
      window.__grants = [];
      window.quansio.onFileGrant((push) => { window.__grants?.push(push); });
    });
    const before = await dialogsOpened();
    // Links that carry a path or a file URL are not even parsed as intents: no dialog opens.
    await desktop.app.evaluate(() => {
      const shell = (globalThis as unknown as { __quansioShell: Shell }).__quansioShell;
      for (const link of ["quansio://attach?path=/etc/passwd", "quansio://attach?localFilePaths=%5B%22/etc/passwd%22%5D", "quansio://attach/file:///etc/passwd"]) {
        shell.openLink(link);
      }
    });
    await new Promise((resolve) => setTimeout(resolve, 500));
    expect(await dialogsOpened()).toBe(before);
    expect(await main().evaluate(() => window.__grants)).toEqual([]);

    await willPick({ files: [file] });
    await desktop.app.evaluate(() => { (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.openLink("quansio://attach"); });
    const pushes = await waitFor(async () => {
      const seen = await main().evaluate(() => window.__grants);
      return seen && seen.length > 0 ? seen : undefined;
    }, "the grant pushed to the page");
    expect(pushes).toHaveLength(1);
    const push = pushes[0] as { grant: { token: string; files: { name: string }[] }; threadId: string | null };
    expect(push.threadId).toBeNull();
    expect(push.grant.files.map((entry) => entry.name)).toEqual(["linked.txt"]);
    expect(JSON.stringify(push)).not.toContain(scratch);
    expect(await dialogsOpened()).toBe(before + 1);
  });

  it("a granted folder: opaque entries, a media URL for a file in it, a conflict-checked edit and a watch that is quiet about its own save", async () => {
    const folder = join(scratch, "project");
    mkdirSync(join(folder, "docs"), { recursive: true });
    writeFileSync(join(folder, "readme.md"), "# project\n");
    writeFileSync(join(folder, "logo.png"), PNG);
    writeFileSync(join(folder, "docs", "inner.txt"), "inner");
    await willPick({ folder });
    const root = (await main().evaluate(() => window.quansio.pickFolder())) as Answer;
    const rootId = (root.result?.["root"] as { rootId: string }).rootId;
    expect(JSON.stringify(root)).not.toContain(scratch);
    const listing = (await main().evaluate((id) => window.quansio.folderEntries(id, null), rootId)) as Answer;
    const entries = listing.result?.["entries"] as { entryId: string; name: string; kind: string }[];
    expect(entries.map((entry) => `${entry.kind}:${entry.name}`)).toEqual(["directory:docs", "file:logo.png", "file:readme.md"]);
    expect(JSON.stringify(listing)).not.toContain(scratch);
    const id = (name: string): string => entries.find((entry) => entry.name === name)?.entryId ?? "";

    // The preview of a file in the folder is a media URL for this window only.
    const media = (await main().evaluate((entryId) => window.quansio.fileMediaUrl(entryId), id("logo.png"))) as Answer;
    expect(await loads(main(), media.result?.["url"] as string)).toBe(true);

    // Watch the folder, then edit through the checked path.
    await main().evaluate((rid) => {
      window.__changes = [];
      window.quansio.onFileChanges((payload) => { window.__changes?.push(payload); });
      return window.quansio.watchFolder(rid);
    }, rootId);
    const opened = (await main().evaluate((entryId) => window.quansio.openFileForEdit(entryId), id("readme.md"))) as Answer;
    const editId = opened.result?.["editId"] as string;
    expect(opened.result?.["text"]).toBe("# project\n");
    const saved = (await main().evaluate((edit) => window.quansio.saveEditedFile({ editId: edit, text: "# project, edited by me\n" }), editId)) as Answer;
    expect(saved).toMatchObject({ ok: true, result: { backup: null } });
    expect(readFileSync(join(folder, "readme.md"), "utf8")).toBe("# project, edited by me\n");
    // Our own save is not reported back, even after the debounce has passed twice over.
    await new Promise((resolve) => setTimeout(resolve, 1000));
    expect(await main().evaluate(() => window.__changes)).toEqual([]);

    // Someone else writes the file and adds one: both are reported, typed, with entry ids that open.
    writeFileSync(join(folder, "readme.md"), "# changed by someone else, longer than before\n");
    writeFileSync(join(folder, "new.txt"), "new");
    const reports = await waitFor(async () => {
      const seen = (await main().evaluate(() => window.__changes)) as { changes?: { kind: string; name: string; entryId: string | null }[] }[] | undefined;
      const names = (seen ?? []).flatMap((report) => (report.changes ?? []).map((change) => `${change.kind}:${change.name}`));
      return names.includes("change:readme.md") && names.includes("add:new.txt") ? seen : undefined;
    }, "the two changes");
    const added = reports.flatMap((report) => report.changes ?? []).find((change) => change.name === "new.txt");
    expect(await main().evaluate((entryId) => window.quansio.openFileForEdit(entryId), added?.entryId ?? "")).toMatchObject({ ok: true, result: { text: "new" } });

    // Saving from the stale baseline is a conflict, naming both versions, and writes nothing.
    const conflict = (await main().evaluate((edit) => window.quansio.saveEditedFile({ editId: edit, text: "# mine\n" }), editId)) as Answer;
    expect(conflict).toMatchObject({ ok: false, error: { code: "FILE_EDIT_CONFLICT" } });
    expect(conflict.error?.details).toMatchObject({ baseline: { size_bytes: Buffer.byteLength("# project, edited by me\n") }, current: { size_bytes: Buffer.byteLength("# changed by someone else, longer than before\n") } });
    expect(readFileSync(join(folder, "readme.md"), "utf8")).toBe("# changed by someone else, longer than before\n");
    // Overwriting keeps the other version in a backup.
    const overwritten = (await main().evaluate((edit) => window.quansio.saveEditedFile({ editId: edit, text: "# mine\n", onConflict: "overwrite" }), editId)) as Answer;
    expect(overwritten).toMatchObject({ ok: true, result: { backup: "readme.md.quansio-backup-1" } });
    expect(readFileSync(join(folder, "readme.md.quansio-backup-1"), "utf8")).toBe("# changed by someone else, longer than before\n");
    expect(readdirSync(folder).sort()).toEqual(["docs", "logo.png", "new.txt", "readme.md", "readme.md.quansio-backup-1"]);

    // A link that leaves the folder is outside it: listing the directory entry that became a link is refused.
    rmSync(join(folder, "docs"), { recursive: true });
    const outside = join(scratch, "outside");
    mkdirSync(outside);
    writeFileSync(join(outside, "secret.txt"), "secret");
    const { symlinkSync } = await import("node:fs");
    symlinkSync(outside, join(folder, "docs"));
    expect(await main().evaluate((args) => window.quansio.folderEntries(args.rid, args.entryId), { rid: rootId, entryId: id("docs") })).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });
    expect(await main().evaluate((args) => window.quansio.watchFolder(args.rid, args.entryId), { rid: rootId, entryId: id("docs") })).toMatchObject({ ok: false, error: { code: "FILE_OUTSIDE_ROOTS" } });

    // Releasing the folder ends the edit session, the watch and the media URL.
    expect(await main().evaluate((rid) => window.quansio.releaseFolder(rid), rootId)).toMatchObject({ ok: true, result: { released: true } });
    expect(await held()).toMatchObject({ media: 0, edits: 0, watches: 0 });
    expect(await loads(main(), media.result?.["url"] as string)).toBe(false);
  });
});
