/**
 * ADP-004 desktop shell smoke: the real Electron binary running the built desktop app, against the real server on a
 * scratch PostgreSQL (`harness.ts`). It drives the shell the way the contract describes it (DOMAIN.md 13.6) and asserts what
 * the operating system and the server then say:
 *
 * - every window type is created with the sandboxed baseline and shows what main says it is;
 * - a pop-out acts only on its own thread, a quick-entry window starts a thread through the real `PostMessage`, the activity
 *   indicator lists what needs the person and a dismissal removes it;
 * - IPC from an untrusted URL, or from a window the shell did not create, is a logged no-op that reaches nothing;
 * - a shortcut the system declines rolls back, and nothing is registered that was not configured;
 * - an update and a quit never interrupt running work without the person's answer, and an unreadable status is asked about.
 *
 * Without the dev stack, the server binaries or a macOS Electron binary the suite is reported `BLOCKED_EXTERNAL` (harness.ts),
 * never passed. What a headless run cannot show is listed in `evidence/ADP-004/NOTES.md` (the tray, a global shortcut fired from another
 * application, the power source changing, a real second display, mouse click-through).
 */

import { createServer, type Server } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import type { PreloadApi } from "../../apps/desktop/src/preload/bridge.js";

import { desktopBlocked, emailOf, launchDesktop, OWNER, signInDesktop, Stack, waitFor, type Desktop } from "./harness.js";

declare global {
  interface Window {
    readonly quansio: PreloadApi;
  }
}

const blocked = desktopBlocked();
const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const PRELOAD = join(ROOT, "apps", "desktop", "dist", "app", "preload.cjs");

/** The test hooks `app.ts` exposes when `QUANSIO_DESKTOP_TEST_HOOKS=1`; types only, erased before the callbacks run in main. */
interface Box {
  readonly title: string;
  readonly message: string;
  readonly detail: string;
  readonly buttons: string[];
}
interface Shell {
  host: { onGlobalShortcut(id: string): void };
  activity: { apply(event: unknown): boolean; items(): { key: string; kind: string }[] };
  hooks: { messageBox: (box: Box) => Promise<number> };
  quit: { revoke(): void; permitted: boolean };
  updateGate: { requestInstall(): Promise<{ kind: string }> };
  updates: { status(): { state: string } };
  windows: { ofType(type: string): unknown[] };
}
interface Dialogs {
  asked: Box[];
  answer: number;
  installs: number;
}

const approvalRequested = (id: string): unknown => ({ type: "approval.requested", aggregate_id: id, payload: { approval_id: id } });
const turn = (type: "turn.started" | "turn.completed", id: string): unknown => ({ type, aggregate_id: id, payload: { turn_id: id, run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" } });

describe.skipIf(blocked !== null)("desktop shell (real Electron, real server)", () => {
  let stack: Stack;
  let desktop: Desktop;
  let outsider: Server;
  let outsiderUrl = "";
  let mainThread = "";
  let otherThread = "";

  const main = (): Desktop["page"] => desktop.page;
  const windowOf = (part: string) => desktop.app.windows().find((page) => page.url().includes(part));

  /** Replace the message box so a dialog is recorded and answered instead of shown. */
  async function answerDialogsWith(answer: number): Promise<void> {
    await desktop.app.evaluate((_electron, index) => {
      const shell = (globalThis as unknown as { __quansioShell: Shell & { dialogs?: Dialogs } }).__quansioShell;
      const dialogs: Dialogs = shell.dialogs ?? { asked: [], answer: 0, installs: 0 };
      dialogs.answer = index;
      shell.dialogs = dialogs;
      shell.hooks.messageBox = (box) => {
        dialogs.asked.push(box);
        return Promise.resolve(dialogs.answer);
      };
    }, answer);
  }

  const dialogsAsked = (): Promise<Box[]> =>
    desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: { dialogs?: Dialogs } }).__quansioShell.dialogs?.asked ?? []);

  beforeAll(async () => {
    stack = await Stack.start("shell");
    outsider = createServer((_request, response) => {
      response.setHeader("content-type", "text/html");
      response.end("<!doctype html><title>outside</title><p>not the app</p>");
    });
    await new Promise<void>((resolve) => outsider.listen(0, "127.0.0.1", resolve));
    const address = outsider.address();
    outsiderUrl = `http://127.0.0.1:${typeof address === "object" && address ? String(address.port) : "0"}/`;
    desktop = await launchDesktop(stack.origin, {
      // Signed so the real updater is wired; hooks so the run can see and steer the shell; the global toggle cleared so the
      // run registers nothing on this machine that was not asked for.
      env: { QUANSIO_DESKTOP_TEST_HOOKS: "1", QUANSIO_DESKTOP_SIGNED: "1" },
      profileFiles: { "shell-preferences.json": JSON.stringify({ version: 1, shortcuts: { toggle_window: null }, keepAwake: null, installWhenIdle: null, installOnQuit: false, indicator: {} }) },
    });
    await signInDesktop(desktop.page, stack, emailOf(OWNER));
    const first = (await main().evaluate(() => window.quansio.postMessage({ content: "hello from the main window" }))) as { thread_id: string };
    mainThread = first.thread_id;
    const second = (await main().evaluate(() => window.quansio.postMessage({ content: "a second conversation" }))) as { thread_id: string };
    otherThread = second.thread_id;
  }, 240_000);

  afterAll(async () => {
    await desktop.close();
    await stack.drop();
    await new Promise<void>((resolve) => outsider.close(() => { resolve(); }));
  }, 60_000);

  it("creates the windows with the sandboxed baseline, each knowing only what main says it is", async () => {
    const opened = await main().evaluate((threadId) => window.quansio.openPopout({ threadId }), mainThread);
    expect(opened).toEqual({ ok: true, result: { reused: false } });
    const popout = await waitFor(() => windowOf("w=popout"), "the pop-out window");
    // The pop-out shows the real conversation, read through its own-thread channels.
    await popout.getByText("hello from the main window").waitFor({ timeout: 30_000 });
    const context = (await popout.evaluate(() => window.quansio.windowContext())) as { ok: boolean; result: { type: string; launch: { threadId: string; tab: string; sidebar: boolean } } };
    expect(context.result.type).toBe("popout");
    expect(context.result.launch).toMatchObject({ threadId: mainThread, tab: "conversation", sidebar: false });
    // The same thread opens the same window again.
    expect(await main().evaluate((threadId) => window.quansio.openPopout({ threadId }), mainThread)).toEqual({ ok: true, result: { reused: true } });

    await desktop.app.evaluate(() => { (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.host.onGlobalShortcut("quick_entry"); });
    const quick = await waitFor(() => windowOf("w=quick_entry"), "the quick-entry window");
    await quick.getByRole("textbox").waitFor({ timeout: 30_000 });

    await desktop.app.evaluate((_electron, frame) => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.activity.apply(frame), approvalRequested("apr_SMOKE"));
    const indicator = await waitFor(() => windowOf("w=indicator"), "the activity indicator window");
    await indicator.getByText("Needs your approval").waitFor({ timeout: 30_000 });

    const windows = await desktop.app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows().map((window) => {
        const preferences = (window.webContents as unknown as { getLastWebPreferences(): Record<string, unknown> }).getLastWebPreferences();
        return {
          url: window.webContents.getURL(),
          sandbox: preferences.sandbox === true,
          contextIsolation: preferences.contextIsolation === true,
          nodeIntegration: preferences.nodeIntegration === true,
          webSecurity: preferences.webSecurity !== false,
          webviewTag: preferences.webviewTag === true,
          alwaysOnTop: window.isAlwaysOnTop(),
          focusable: window.isFocusable(),
          resizable: window.isResizable(),
          allWorkspaces: window.isVisibleOnAllWorkspaces(),
        };
      }),
    );
    expect(windows.map((window) => new URL(window.url).searchParams.get("w") ?? "main").sort()).toEqual(["indicator", "main", "popout", "quick_entry"]);
    for (const window of windows) {
      expect(window.url.startsWith("app://desktop/")).toBe(true);
      expect(window).toMatchObject({ sandbox: true, contextIsolation: true, nodeIntegration: false, webSecurity: true, webviewTag: false });
    }
    const byType = (type: string) => windows.find((window) => window.url.includes(`w=${type}`));
    expect(byType("quick_entry")).toMatchObject({ alwaysOnTop: true, resizable: false });
    expect(byType("indicator")).toMatchObject({ alwaysOnTop: true, focusable: false, allWorkspaces: true });
  }, 180_000);

  it("a quick-entry window starts a new thread through the real PostMessage and hides itself", async () => {
    const before = ((await main().evaluate(() => window.quansio.listThreads())) as { threads: unknown[] }).threads.length;
    await desktop.app.evaluate(() => { (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.host.onGlobalShortcut("quick_entry"); });
    const quick = await waitFor(() => windowOf("w=quick_entry"), "the quick-entry window");
    await quick.getByRole("textbox").fill("Quick capture from anywhere");
    await quick.getByRole("textbox").press("Enter");
    await waitFor(async () => {
      const threads = ((await main().evaluate(() => window.quansio.listThreads())) as { threads: unknown[] }).threads;
      return threads.length === before + 1;
    }, "the new thread on the server");
    await waitFor(async () => !(await desktop.app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().some((window) => window.webContents.getURL().includes("w=quick_entry") && window.isVisible()))), "quick entry to hide");
  }, 120_000);

  it("the activity indicator lists what needs the person and a dismissal removes it", async () => {
    const indicator = await waitFor(() => windowOf("w=indicator"), "the activity indicator window");
    await indicator.getByText("Needs your approval").waitFor({ timeout: 30_000 });
    expect(await indicator.getByText("Waiting for you").count()).toBe(1);
    await indicator.getByRole("button", { name: "Dismiss" }).click();
    await waitFor(async () => (await desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.activity.items().length)) === 0, "the list to empty");
    await indicator.getByText("Nothing needs you.").waitFor({ timeout: 30_000 });
  }, 120_000);

  it("a pop-out cannot touch another thread, and a quick-entry window cannot reach settings", async () => {
    const popout = await waitFor(() => windowOf("w=popout"), "the pop-out window");
    const other = await popout.evaluate((threadId) => window.quansio.threadMessages(threadId), otherThread);
    expect(other).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    const posted = await popout.evaluate((threadId) => window.quansio.postMessage({ threadId, content: "into another thread" }), otherThread);
    expect(posted).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    const own = await popout.evaluate((threadId) => window.quansio.threadMessages(threadId), mainThread);
    expect(JSON.stringify(own)).toContain("hello from the main window");

    await desktop.app.evaluate(() => { (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.host.onGlobalShortcut("quick_entry"); });
    const quick = await waitFor(() => windowOf("w=quick_entry"), "the quick-entry window");
    const settings = await quick.evaluate(() => window.quansio.updateWorkspace("ws_01J8Z3K6F1N8VQ2X5W9Y0QA8AA", { name: "taken over" }));
    expect(settings).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    const bindings = await quick.evaluate(() => window.quansio.setShortcut("find", "mod+shift+x"));
    expect(bindings).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    const command = await quick.evaluate(() => window.quansio.invokeCommand({ name: "PostMessage", commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params: { content: "x" } }));
    expect(command).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
  }, 120_000);

  it("IPC from an untrusted URL, or from a window the shell did not create, reaches nothing and answers nothing", async () => {
    const threadsBefore = ((await main().evaluate(() => window.quansio.listThreads())) as { threads: unknown[] }).threads.length;
    const results = await desktop.app.evaluate(
      async ({ BrowserWindow }, input: { preload: string; urls: string[] }) => {
        const out: { url: string; answers: string[] }[] = [];
        for (const url of input.urls) {
          // A window made outside the shell, with the real preload, on a page the shell would never load.
          const stranger = new BrowserWindow({ show: false, webPreferences: { preload: input.preload, sandbox: true, contextIsolation: true } });
          await stranger.loadURL(url);
          const answers = (await stranger.webContents.executeJavaScript(
            `Promise.all([
              window.quansio.sessionState().then((v) => String(v)),
              window.quansio.postMessage({ content: "sent by an untrusted page" }).then((v) => String(v)),
              window.quansio.updateWorkspace("ws_01J8Z3K6F1N8VQ2X5W9Y0QA8AA", { name: "taken over" }).then((v) => String(v)),
              window.quansio.shellPreferences().then((v) => String(v)),
              window.quansio.setShortcut("find", "mod+shift+x").then((v) => String(v)),
              window.quansio.exportDiagnostics().then((v) => String(v)),
            ])`,
          )) as string[];
          out.push({ url, answers });
          stranger.destroy();
        }
        return out;
      },
      { preload: PRELOAD, urls: ["data:text/html,<title>x</title>", outsiderUrl, "app://desktop/index.html"] },
    );
    for (const result of results) {
      expect(result.answers, result.url).toEqual(["undefined", "undefined", "undefined", "undefined", "undefined", "undefined"]);
    }
    const log = desktop.stderr.join("");
    expect(log).toContain("ignored messages.post: untrusted sender (null)");
    expect(log).toContain(`ignored messages.post: untrusted sender (${new URL(outsiderUrl).origin})`);
    expect(log).toContain("ignored messages.post: not a window the shell created");
    expect(log).not.toContain("sent by an untrusted page");
    // Nothing the untrusted pages tried reached the server.
    const threadsAfter = ((await main().evaluate(() => window.quansio.listThreads())) as { threads: unknown[] }).threads.length;
    expect(threadsAfter).toBe(threadsBefore);
    // The signed-in main window still gets real answers.
    expect(await main().evaluate(() => window.quansio.sessionState())).toMatchObject({ kind: "signed-in" });
  }, 120_000);

  it("registers only what was configured, rolls back a shortcut the system declines and pauses shortcuts while one is recorded", async () => {
    const registered = (accelerator: string): Promise<boolean> =>
      desktop.app.evaluate(({ globalShortcut }, key) => globalShortcut.isRegistered(key), accelerator);
    // The toggle was cleared in this profile and quick entry never had a binding: nothing is registered.
    expect(await registered("Command+Option+Q")).toBe(false);
    const initial = (await main().evaluate(() => window.quansio.shortcuts())) as { result: { shortcuts: { id: string; binding: string | null; registered: boolean }[] } };
    expect(initial.result.shortcuts.filter((entry) => entry.registered)).toEqual([]);

    expect(await main().evaluate(() => window.quansio.setShortcut("quick_entry", "mod+alt+shift+k"))).toMatchObject({ ok: true });
    expect(await registered("Command+Option+Shift+K")).toBe(true);

    // Another holder takes a key; asking for it is declined by the system, and the previous binding stays registered.
    await desktop.app.evaluate(({ globalShortcut }) => globalShortcut.register("Command+Option+Shift+J", () => undefined));
    const declined = await main().evaluate(() => window.quansio.setShortcut("quick_entry", "mod+alt+shift+j"));
    expect(declined).toMatchObject({ ok: false, error: { code: "registration_failed" } });
    expect(await registered("Command+Option+Shift+K")).toBe(true);
    const after = (await main().evaluate(() => window.quansio.shortcuts())) as { result: { shortcuts: { id: string; binding: string | null; registered: boolean }[] } };
    expect(after.result.shortcuts.find((entry) => entry.id === "quick_entry")).toMatchObject({ binding: "mod+alt+shift+k", registered: true });
    await desktop.app.evaluate(({ globalShortcut }) => {
      globalShortcut.unregister("Command+Option+Shift+J");
    });

    // A conflict with the menu's own accelerators, and a binding that is not valid, are typed refusals.
    expect(await main().evaluate(() => window.quansio.setShortcut("find", "mod+w"))).toMatchObject({ ok: false, error: { code: "conflict" } });
    expect(await main().evaluate(() => window.quansio.setShortcut("find", "Mod+W"))).toMatchObject({ ok: false, error: { code: "invalid" } });

    // Recording pauses the global shortcut and the menu's accelerators, and ending it brings them back.
    const accelerators = (): Promise<number> =>
      desktop.app.evaluate(({ Menu }) => {
        interface Entry {
          accelerator?: unknown;
          submenu?: { items: Entry[] } | null;
        }
        const count = (items: Entry[]): number => items.reduce((total, item) => total + (item.accelerator ? 1 : 0) + (item.submenu ? count(item.submenu.items) : 0), 0);
        return count((Menu.getApplicationMenu()?.items ?? []));
      });
    const before = await accelerators();
    expect(before).toBeGreaterThan(10);
    expect(await main().evaluate(() => window.quansio.beginShortcutRecording())).toMatchObject({ ok: true });
    expect(await registered("Command+Option+Shift+K")).toBe(false);
    await waitFor(async () => (await accelerators()) === 0, "the menu's accelerators to pause");
    expect(await main().evaluate(() => window.quansio.endShortcutRecording())).toMatchObject({ ok: true, result: { ended: true } });
    expect(await registered("Command+Option+Shift+K")).toBe(true);
    await waitFor(async () => (await accelerators()) === before, "the menu's accelerators to return");

    // Clearing a binding releases it; nothing is left registered.
    expect(await main().evaluate(() => window.quansio.setShortcut("quick_entry", null))).toMatchObject({ ok: true });
    expect(await registered("Command+Option+Shift+K")).toBe(false);

    const menu = await desktop.app.evaluate(({ Menu }) => Menu.getApplicationMenu()?.items.map((item) => item.label) ?? []);
    expect(menu).toEqual(expect.arrayContaining(["File", "Edit", "View", "Window"]));
  }, 120_000);

  it("never restarts for an update over running work without the person's answer", async () => {
    await answerDialogsWith(0);
    await desktop.app.evaluate(({ autoUpdater }) => {
      const shell = (globalThis as unknown as { __quansioShell: Shell & { dialogs: Dialogs } }).__quansioShell;
      // The platform's installer is replaced: this run proves whether the shell asks it to restart, not Squirrel itself.
      (autoUpdater as unknown as { quitAndInstall: () => void }).quitAndInstall = () => {
        shell.dialogs.installs += 1;
      };
      (autoUpdater as unknown as { emit: (event: string, ...args: unknown[]) => void }).emit("update-downloaded", {}, "notes", "2.0.0");
    });
    expect(await desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.updates.status().state)).toBe("downloaded");
    const installs = (): Promise<number> => desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: { dialogs: Dialogs } }).__quansioShell.dialogs.installs);

    // A Turn is running (this desktop saw it start): the question is asked, "Later" leaves it, nothing restarts.
    await desktop.app.evaluate((_electron, frame) => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.activity.apply(frame), turn("turn.started", "trn_SMOKE1"));
    const deferred = await desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.updateGate.requestInstall());
    expect(deferred).toEqual({ kind: "deferred" });
    expect(await installs()).toBe(0);
    const asked = await dialogsAsked();
    expect(asked).toHaveLength(1);
    expect(asked[0]).toMatchObject({ message: "1 turn is running.", buttons: ["Later", "Install when idle", "Restart anyway"] });
    expect(asked[0]?.detail).toMatch(/pauses running work/);

    // Only an explicit "Restart anyway" restarts over running work.
    await answerDialogsWith(2);
    expect(await desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.updateGate.requestInstall())).toEqual({ kind: "installing" });
    expect(await installs()).toBe(1);
    await desktop.app.evaluate(() => { (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.quit.revoke(); });

    // Idle, the server and this desktop agree, and the restart needs no question.
    await desktop.app.evaluate((_electron, frame) => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.activity.apply(frame), turn("turn.completed", "trn_SMOKE1"));
    const asks = (await dialogsAsked()).length;
    expect(await desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.updateGate.requestInstall())).toEqual({ kind: "installing" });
    expect(await installs()).toBe(2);
    expect((await dialogsAsked()).length).toBe(asks);
    await desktop.app.evaluate(() => { (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.quit.revoke(); });
  }, 120_000);

  it("never quits over running work without the person's answer, asks when the status cannot be read, and quits quietly when idle", async () => {
    await answerDialogsWith(0);
    const quitAsks = async (): Promise<Box[]> => (await dialogsAsked()).filter((box) => box.title === "Quit Quansio?");
    const requestQuit = (): Promise<void> => desktop.app.evaluate(({ app }) => { app.quit(); });
    const alive = (): Promise<number> => desktop.app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().length);

    // A Turn in flight: quitting asks, and "Keep working" leaves the app running with its windows.
    await desktop.app.evaluate((_electron, frame) => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.activity.apply(frame), turn("turn.started", "trn_SMOKE2"));
    await requestQuit();
    await waitFor(async () => (await quitAsks()).length === 1, "the quit question");
    const question = (await quitAsks())[0];
    expect(question).toMatchObject({ message: "1 turn is running.", buttons: ["Keep working", "Quit and pause"] });
    expect(question?.detail).toMatch(/paused/);
    expect(await alive()).toBeGreaterThan(0);
    expect(await desktop.app.evaluate(() => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.quit.permitted)).toBe(false);

    // The server cannot be reached and nothing local is running: the status is unknown, which is asked about, not assumed idle.
    await desktop.app.evaluate((_electron, frame) => (globalThis as unknown as { __quansioShell: Shell }).__quansioShell.activity.apply(frame), turn("turn.completed", "trn_SMOKE2"));
    await stack.down();
    await requestQuit();
    await waitFor(async () => (await quitAsks()).length === 2, "the second quit question");
    expect((await quitAsks())[1]?.message).toMatch(/could not check whether work is running/);
    expect(await alive()).toBeGreaterThan(0);

    // The server is back and reports nothing running: quitting needs no question and the app exits.
    await stack.up();
    const exited = new Promise<void>((resolve) => desktop.app.process().once("exit", () => { resolve(); }));
    await requestQuit();
    await exited;
  }, 240_000);
});

if (blocked !== null) {
  it.skip(blocked, () => undefined);
}
