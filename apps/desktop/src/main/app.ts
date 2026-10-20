/**
 * Electron main process (APP-003, ADP-004).
 *
 * The one module that composes the shell against Electron: the windows (main, pop-out, quick entry, activity
 * indicator) with the renderer security baseline applied to every one, the preload bridge behind the IPC trust
 * guard, the application menu, the tray, global shortcuts, keep-awake, the quit confirmation, deep links, update
 * hooks and the durable session. Runtime authority stays in `quansio-server`; this process holds the session scope and
 * opens the stream socket on the renderers' behalf.
 *
 * Everything decision-shaped lives in the sibling modules (`security.ts`, `ipc.ts`, `ipc-trust.ts`, `windows.ts`,
 * `shortcuts.ts`, `quit.ts`, `update-gate.ts`, `shell-host.ts`, …) so the tests can drive it without an Electron
 * binary; this file supplies the Electron adapters those modules are written against and wires them.
 */

import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { Readable } from "node:stream";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  app,
  autoUpdater,
  BrowserWindow,
  dialog,
  globalShortcut,
  ipcMain,
  Menu,
  nativeImage,
  nativeTheme,
  net,
  Notification,
  powerMonitor,
  powerSaveBlocker,
  protocol,
  safeStorage,
  screen,
  session,
  shell,
  systemPreferences,
  Tray,
  type Display,
  type MenuItemConstructorOptions,
  webContents,
} from "electron";
import WebSocket from "ws";

import type { SocketFactory, SocketLike } from "../client/stream.js";
import { ActivityTracker } from "./activity.js";
import { ConnectivityMonitor } from "./connectivity.js";
import { deepLinkFromArgv, parseDeepLink, DEEP_LINK_SCHEME } from "./deeplinks.js";
import { quitChosen, quitDialog, updateChoiceOf, updateDialog, type MessageBox } from "./dialogs.js";
import { DesktopBridge, type DesktopAuth } from "./ipc.js";
import { spawn as spawnProcess } from "node:child_process";
import { WorkerLauncher, discoverChrome, discoverWorkerBinary, isLoopbackOrigin, type ProvisionedWorker } from "./worker-launcher.js";
import { IpcGuard, registerGuardedHandlers, type WindowHandler } from "./ipc-trust.js";
import { loadLocalFilesConfig } from "./local-files/config.js";
import { loadLocalSecretsConfig } from "./secrets/config.js";
import { composeSecrets } from "./secrets/compose.js";
import { LocalFilesHost, type LocalFileDialogs } from "./local-files/host.js";
import { MEDIA_SCHEME } from "./local-files/media.js";
import { newCommandId } from "../client/ids.js";
import { MAX_UPLOAD_BYTES } from "../conversation/composer.js";
import { t } from "../i18n/strings.js";
import { KeepAwake } from "./keep-awake.js";
import type { MenuItem } from "./menu.js";
import { UNPROBED_PERMISSIONS, mapMediaAccess, type PermissionSource } from "./permissions.js";
import { PreferencesFile } from "./preferences.js";
import { QuitGate } from "./quit.js";
import {
  navigationAllowed,
  permissionRequestDecision,
  preloadPath,
  withContentSecurityPolicy,
  windowOpenDecision,
} from "./security.js";
import { SessionFile } from "./session-file.js";
import { loadShellConfig } from "./shell-config.js";
import { ShellHost } from "./shell-host.js";
import { ShortcutRegistry } from "./shortcuts.js";
import { TokenStore } from "./token-store.js";
import { closeBehaviour, trayIconPng, trayMenu, wantsTray } from "./tray.js";
import { UpdateGate } from "./update-gate.js";
import { parseUpdateChannel, updateFeedUrl, UpdateHooks } from "./updates.js";
import {
  WindowManager,
  type ManagedWindow,
  type DisplayAdapter,
  type DisplayInfo,
  type WindowEvent,
  type WindowFactory,
  type WindowHandle,
  type WindowSpec,
} from "./windows.js";
import type { PreloadChannel } from "../preload/bridge.js";

/**
 * The shell's environment. A real desktop signs in (session mode, refresh token in the OS
 * keychain). A development harness may instead pin a tenant/user/workspace with
 * `QUANSIO_TENANT_ID`/`QUANSIO_USER_ID`/`QUANSIO_WORKSPACE_ID` against a server started with
 * `QUANSIO_TRUST_SCOPE_HEADERS=1`; all three or none.
 */
export interface ShellEnvironment {
  readonly serverOrigin: string;
  readonly harness: { readonly tenantId: string; readonly userId: string; readonly workspaceId: string } | null;
  readonly updateChannel: "stable" | "beta";
  readonly signed: boolean;
}

export function shellEnvironment(env: NodeJS.ProcessEnv): ShellEnvironment {
  const tenantId = env["QUANSIO_TENANT_ID"]?.trim() ?? "";
  const userId = env["QUANSIO_USER_ID"]?.trim() ?? "";
  const workspaceId = env["QUANSIO_WORKSPACE_ID"]?.trim() ?? "";
  const provided = [tenantId, userId, workspaceId].filter(Boolean).length;
  if (provided !== 0 && provided !== 3) {
    throw new Error("QUANSIO_TENANT_ID, QUANSIO_USER_ID and QUANSIO_WORKSPACE_ID must be set together (harness mode) or not at all");
  }
  return {
    serverOrigin: env["QUANSIO_SERVER_ORIGIN"]?.trim() || "http://127.0.0.1:8080",
    harness: provided === 3 ? { tenantId, userId, workspaceId } : null,
    updateChannel: parseUpdateChannel(env["QUANSIO_UPDATE_CHANNEL"]),
    signed: env["QUANSIO_DESKTOP_SIGNED"] === "1",
  };
}

/** `ws` satisfies the client's socket slice; headers carry the scope the server resolves. */
export const openSocket: SocketFactory = (url, headers): SocketLike => new WebSocket(url, { headers });

const bundleRoot = dirname(fileURLToPath(import.meta.url));

/** Register the bundled renderer under the `app:` scheme so navigation can be pinned to it. */
function registerRendererScheme(): void {
  protocol.registerSchemesAsPrivileged([
    { scheme: "app", privileges: { standard: true, secure: true, supportFetchAPI: true } },
    // Local media URLs (ADP-005): capability tokens served by main; `stream` so audio and video can seek with Range.
    { scheme: MEDIA_SCHEME, privileges: { standard: true, secure: true, supportFetchAPI: true, stream: true } },
  ]);
}

function serveRenderer(): void {
  const rendererRoot = join(bundleRoot, "renderer");
  protocol.handle("app", (request) => {
    const url = new URL(request.url);
    const relative = url.pathname === "/" ? "/index.html" : url.pathname;
    if (relative.includes("..")) {
      return new Response("not found", { status: 404 });
    }
    // Electron's `net.fetch` reads `file:` URLs; Node's global `fetch` does not ("not
    // implemented"), which left the window blank on every launch.
    return net.fetch(pathToFileURL(join(rendererRoot, relative)).href);
  });
}

/** State the window factory and the quit flow share. */
interface ShellState {
  quitting: boolean;
  trayAvailable: boolean;
}

function displayInfo(display: Display): DisplayInfo {
  return { key: `d${String(display.id)}`, bounds: display.bounds, workArea: display.workArea };
}

const electronDisplays: DisplayAdapter = {
  all: () => screen.getAllDisplays().map(displayInfo),
  primary: () => displayInfo(screen.getPrimaryDisplay()),
  nearest: (point) => displayInfo(screen.getDisplayNearestPoint({ x: Math.round(point.x), y: Math.round(point.y) })),
};

/** Turn the shell's window descriptions into real `BrowserWindow`s, each with the baseline applied. */
function electronWindows(state: ShellState, log: (line: string) => void): WindowFactory {
  return {
    create(spec: WindowSpec): WindowHandle {
      const win = new BrowserWindow({ ...spec.options });
      const contents = win.webContents;
      const webContentsId = contents.id;
      // The page, whatever it does, stays on the bundled document and opens nothing of its own.
      contents.on("will-navigate", (event, url) => {
        if (!navigationAllowed(url)) {
          event.preventDefault();
        }
      });
      contents.on("will-attach-webview", (event) => {
        event.preventDefault();
      });
      contents.setWindowOpenHandler(({ url }) => {
        const decision = windowOpenDecision(url);
        if (decision.openExternally) {
          void shell.openExternal(url);
        }
        return { action: decision.action };
      });
      if (spec.behaviour.clickThrough) {
        win.setIgnoreMouseEvents(true, { forward: true });
      }
      if (process.platform === "darwin") {
        if (spec.behaviour.visibleOnAllWorkspaces) {
          win.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
        }
        if (spec.behaviour.hiddenInMissionControl) {
          win.setHiddenInMissionControl(true);
        }
      }
      if (spec.type === "indicator") {
        win.setAlwaysOnTop(true, "screen-saver");
      }
      // `show` is deferred until the page has painted, so a window never appears blank.
      let ready = false;
      const whenReady = (action: () => void): void => {
        if (ready) {
          action();
        } else {
          win.once("ready-to-show", () => {
            ready = true;
            action();
          });
        }
      };
      win.once("ready-to-show", () => {
        ready = true;
      });
      if (spec.type === "main") {
        // Closing the main window hides it to the tray, or minimizes it where there is none; only a real quit closes it.
        win.on("close", (event) => {
          const behaviour = closeBehaviour({ platform: process.platform, trayAvailable: state.trayAvailable, quitting: state.quitting });
          if (behaviour === "hide") {
            event.preventDefault();
            win.hide();
          } else if (behaviour === "minimize") {
            event.preventDefault();
            win.minimize();
          }
        });
      }
      const listeners: Record<WindowEvent, () => void> = {
        closed: () => undefined,
        blur: () => undefined,
        focus: () => undefined,
        navigated: () => undefined,
        crashed: () => undefined,
        moved: () => undefined,
      };
      const notify = (event: WindowEvent): void => {
        listeners[event]();
      };
      win.on("closed", () => {
        notify("closed");
      });
      win.on("blur", () => {
        notify("blur");
      });
      win.on("focus", () => {
        notify("focus");
      });
      win.on("moved", () => {
        notify("moved");
      });
      contents.on("did-start-navigation", (details) => {
        if (details.isMainFrame && !details.isSameDocument) {
          notify("navigated");
        }
      });
      contents.on("render-process-gone", (_event, details) => {
        log(`quansio desktop: a ${spec.type} window's renderer ended (${details.reason})`);
        notify("crashed");
      });
      return {
        webContentsId,
        loadUrl: (url) => win.loadURL(url),
        show: () => {
          whenReady(() => {
            win.show();
          });
        },
        showInactive: () => {
          whenReady(() => {
            win.showInactive();
          });
        },
        hide: () => {
          win.hide();
        },
        focus: () => {
          whenReady(() => {
            win.focus();
          });
        },
        minimize: () => {
          win.minimize();
        },
        restore: () => {
          win.restore();
        },
        close: () => {
          win.close();
        },
        isDestroyed: () => win.isDestroyed(),
        isVisible: () => win.isVisible(),
        isMinimized: () => win.isMinimized(),
        isFocused: () => win.isFocused(),
        send: (channel, payload) => {
          if (!contents.isDestroyed()) {
            contents.send(channel, payload);
          }
        },
        setAlwaysOnTop: (pinned) => {
          win.setAlwaysOnTop(pinned, pinned ? "floating" : "normal");
        },
        getBounds: () => win.getBounds(),
        setBounds: (bounds) => {
          win.setBounds(bounds);
        },
        setIgnoreMouseEvents: (ignore, forward) => {
          win.setIgnoreMouseEvents(ignore, { forward });
        },
        on: (event, listener) => {
          listeners[event] = listener;
        },
      };
    },
  };
}

/** The native menu for the shell's menu description. */
function electronMenu(items: readonly MenuItem[], click: (item: MenuItem) => void): Menu {
  const convert = (entries: readonly MenuItem[]): MenuItemConstructorOptions[] =>
    entries.map((entry): MenuItemConstructorOptions => {
      if (entry.separator) {
        return { type: "separator" };
      }
      const out: MenuItemConstructorOptions = {};
      if (entry.label !== undefined) {
        out.label = entry.label;
      }
      if (entry.role !== undefined) {
        out.role = entry.role;
      }
      if (entry.accelerator !== undefined) {
        out.accelerator = entry.accelerator;
      }
      if (entry.enabled !== undefined) {
        out.enabled = entry.enabled;
      }
      if (entry.action !== undefined) {
        out.click = () => {
          click(entry);
        };
      }
      if (entry.submenu !== undefined) {
        out.submenu = convert(entry.submenu);
      }
      return out;
    });
  return Menu.buildFromTemplate(convert(items));
}

/** Hooks a test harness may use to drive the shell; present only when `QUANSIO_DESKTOP_TEST_HOOKS=1`. */
interface TestHooks {
  messageBox: (box: MessageBox) => Promise<number>;
  /** The native file and folder dialogs (ADP-005); a run replaces them with what a person would pick. */
  dialogs: LocalFileDialogs;
}

/** The `BrowserWindow` a managed window is, for parenting a native dialog. */
function parentOf(window: ManagedWindow): BrowserWindow | undefined {
  const contents = webContents.fromId(window.handle.webContentsId);
  return (contents ? BrowserWindow.fromWebContents(contents) : null) ?? undefined;
}

/** Electron's own open dialogs for the local-file host: files (several, never a folder) and one folder. */
const nativeDialogs: LocalFileDialogs = {
  pickFiles: async (window) => {
    const options = { title: t("attachDialogTitle"), properties: ["openFile" as const, "multiSelections" as const] };
    const parent = parentOf(window);
    const chosen = parent ? await dialog.showOpenDialog(parent, options) : await dialog.showOpenDialog(options);
    return chosen.canceled ? null : chosen.filePaths;
  },
  pickFolder: async (window) => {
    const options = { title: t("folderDialogTitle"), properties: ["openDirectory" as const] };
    const parent = parentOf(window);
    const chosen = parent ? await dialog.showOpenDialog(parent, options) : await dialog.showOpenDialog(options);
    return chosen.canceled ? null : (chosen.filePaths[0] ?? null);
  },
};

export async function startDesktop(): Promise<{ bridge: DesktopBridge; host: ShellHost; windows: WindowManager; openLink: (url: string) => void }> {
  const environment = shellEnvironment(process.env);
  const userData = app.getPath("userData");
  const config = loadShellConfig(join(bundleRoot, "desktop-shell.yaml"));
  const preferences = new PreferencesFile(userData);
  const sessionFile = new SessionFile(userData);
  const log = (line: string): void => {
    process.stderr.write(`${line}\n`);
  };
  const state: ShellState = { quitting: false, trayAvailable: false };
  const updates = new UpdateHooks(
    environment.updateChannel,
    autoUpdater,
    updateFeedUrl(environment.serverOrigin, environment.updateChannel, process.platform, process.arch),
    environment.signed,
    preferences.current().installOnQuit,
  );
  updates.onInstallOnQuit((enabled) => {
    preferences.update((previous) => ({ ...previous, installOnQuit: enabled }));
  });

  const defaultSession = session.defaultSession;
  defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback(withContentSecurityPolicy({ responseHeaders: details.responseHeaders, url: details.url }));
  });
  defaultSession.setPermissionRequestHandler((_contents, permission, callback) => {
    callback(permissionRequestDecision(permission));
  });
  defaultSession.setPermissionCheckHandler((_contents, permission) => permissionRequestDecision(permission));
  serveRenderer();

  const windows = new WindowManager({
    factory: electronWindows(state, log),
    platform: process.platform,
    preload: preloadPath(bundleRoot),
    config,
    displays: electronDisplays,
    log,
  });
  const hooks: TestHooks = {
    messageBox: async (box) => {
      const options = { type: "warning" as const, title: box.title, message: box.message, detail: box.detail, buttons: [...box.buttons], defaultId: box.defaultId, cancelId: box.cancelId, noLink: true };
      const parent = BrowserWindow.getFocusedWindow() ?? BrowserWindow.getAllWindows().find((candidate) => candidate.isVisible());
      const result = parent ? await dialog.showMessageBox(parent, options) : await dialog.showMessageBox(options);
      return result.response;
    },
    dialogs: nativeDialogs,
  };

  // The one place that knows what is running: the server's maintenance status and this desktop's own stream.
  let host: ShellHost | null = null;
  const probe = (): ReturnType<ShellHost["probe"]> => (host ? host.probe() : Promise.resolve({ kind: "unknown" as const }));
  const quit = new QuitGate({
    probe,
    ask: async (status) => quitChosen(await hooks.messageBox(quitDialog(status))),
  });
  const updateGate = new UpdateGate({
    updates,
    probe,
    ask: async (status) => updateChoiceOf(await hooks.messageBox(updateDialog(status))),
    allowQuit: () => {
      quit.allow();
    },
    revokeQuit: () => {
      quit.revoke();
    },
    pollMs: config.updateIdlePollMs,
    setTimer: (callback, delay) => setTimeout(callback, delay),
    clearTimer: (handle) => {
      clearTimeout(handle as ReturnType<typeof setTimeout>);
    },
  });
  // A failed install leaves the app running: it must not stay permitted to quit without asking.
  updates.onStatus((status) => {
    if (status.state === "error") {
      quit.revoke();
    }
    host?.onUpdateStatus(status);
  });

  const auth: DesktopAuth = environment.harness
    ? { kind: "trusted-headers", scope: { tenantId: environment.harness.tenantId, userId: environment.harness.userId }, workspaceId: environment.harness.workspaceId }
    : { kind: "session", tokens: new TokenStore(userData, safeStorage) };
  const localSecrets = loadLocalSecretsConfig(join(bundleRoot, "local-secrets.yaml"));
  // The worker on this computer (EXEC-002): started from the app against a folder the person picks.
  const worker = new WorkerLauncher({
    provision: async () => {
      const workspace = bridgeRef?.workspaceId() ?? environment.harness?.workspaceId ?? "";
      const answer = await bridgeRef?.server.invoke<ProvisionedWorker>("ProvisionTarget", newCommandId(), { workspace_id: workspace, substrate: "customer_private_worker" });
      if (!answer) {
        throw new Error("the app is not ready");
      }
      return answer.result;
    },
    pickFolder: async () => {
      const parent = BrowserWindow.getFocusedWindow() ?? BrowserWindow.getAllWindows()[0];
      const options = { title: t("workerFolderDialogTitle"), properties: ["openDirectory" as const] };
      const chosen = parent ? await dialog.showOpenDialog(parent, options) : await dialog.showOpenDialog(options);
      return chosen.canceled ? null : (chosen.filePaths[0] ?? null);
    },
    binary: discoverWorkerBinary(process.env, process.resourcesPath, process.env["QUANSIO_REPO_ROOT"] ?? null),
    chrome: discoverChrome(process.platform, process.env),
    stateRoot: join(userData, "worker"),
    protectedPaths: [userData],
    serverIsLocal: isLoopbackOrigin(environment.serverOrigin),
    spawn: (binary, args) => spawnProcess(binary, [...args], { stdio: ["ignore", "ignore", "pipe"] }),
    log,
  });
  let bridgeRef: DesktopBridge | null = null;
  const bridge = new DesktopBridge({
    worker,
    serverOrigin: environment.serverOrigin,
    auth,
    // One-time payment values (ADP-009): the limits the payment form and its channel run on.
    localSecrets,
    // OAuth: the authorization URL opens in the system browser, never in the renderer.
    openExternal: (url) => shell.openExternal(url),
    // The theme preference also drives the native chrome (traffic lights, menus, scrollbars).
    setAppearance: (theme) => {
      nativeTheme.themeSource = theme;
    },
    // Native desktop notifications (APP-010): only for events the person's preference sends to the desktop
    // channel; clicking one brings the main window forward on what it points at (ADP-004).
    notifyNative: (notice) => {
      if (Notification.isSupported()) {
        const native = new Notification({ title: notice.title, body: notice.body, silent: false });
        native.on("click", () => {
          host?.onNoticeClicked(notice);
        });
        native.show();
      }
    },
    // Artifact files (APP-008): saved where the user chooses, or written under userData and
    // handed to the OS handler; the bridge refuses pages, scripts and programs before this.
    files: {
      saveAs: async (suggestedName, bytes) => {
        const parent = BrowserWindow.getFocusedWindow() ?? BrowserWindow.getAllWindows()[0];
        const chosen = parent ? await dialog.showSaveDialog(parent, { defaultPath: suggestedName }) : await dialog.showSaveDialog({ defaultPath: suggestedName });
        if (chosen.canceled || !chosen.filePath) {
          return null;
        }
        writeFileSync(chosen.filePath, bytes, { mode: 0o600 });
        return chosen.filePath;
      },
      openWithOs: async (suggestedName, bytes) => {
        const directory = join(userData, "artifacts");
        mkdirSync(directory, { recursive: true, mode: 0o700 });
        const path = join(directory, suggestedName);
        writeFileSync(path, bytes, { mode: 0o600 });
        const failure = await shell.openPath(path);
        if (failure) {
          throw new Error(failure);
        }
        return path;
      },
    },
    // Native computer-use permissions (ADP-002): what macOS says, read on request. Only the person's click in the
    // guide opens a Settings pane; nothing here grants or requests a permission.
    ...optional("computerPermissions", macPermissionSource()),
    openSystemSettings: (url) => shell.openExternal(url),
    // The person's recorder (ADP-003): a separate short-lived process the desktop starts for one recording.
    ...optional("recorderBinary", recorderBinaryPath()),
    session: sessionFile,
    openSocket,
    updates,
    consumer: `desktop:${String(process.pid)}`,
    // Where each push goes (ADP-004): a deep link waits for the window if it has not subscribed; the stream's
    // frames go to every window that shows a thread; the rest are for the main window.
    push: (channel, payload) => {
      switch (channel) {
        case "deepLink.intent":
          windows.deliver(windows.openMain(), channel, payload);
          break;
        case "stream.frame":
        case "stream.state":
        case "stream.error":
        case "session.state":
          windows.broadcast(["main", "popout"], channel, payload);
          break;
        default:
          windows.broadcast(["main"], channel, payload);
      }
    },
    observe: (frame) => {
      host?.onStreamFrame(frame);
    },
  });
  bridgeRef = bridge;

  const shortcuts = new ShortcutRegistry({
    platform: process.platform,
    maxLength: config.shortcutMaxLength,
    toggleDefault: config.toggleDefault,
    adapter: {
      register: (accelerator, handler) => globalShortcut.register(accelerator, handler),
      unregister: (accelerator) => {
        globalShortcut.unregister(accelerator);
      },
    },
    preferences,
    onGlobal: (id) => {
      host?.onGlobalShortcut(id);
    },
    log,
  });
  const keepAwake = new KeepAwake({
    start: () => powerSaveBlocker.start("prevent-app-suspension"),
    stop: (id) => {
      powerSaveBlocker.stop(id);
    },
  });
  const connectivity = new ConnectivityMonitor({
    graceMs: config.offlineGraceMs,
    minGapMs: config.reconnectMinGapMs,
    reconnect: (force) => bridge.resumeStream(force),
    streamOpen: () => bridge.streamOpen(),
    now: () => Date.now(),
    setTimer: (callback, delay) => setTimeout(callback, delay),
    clearTimer: (handle) => {
      clearTimeout(handle as ReturnType<typeof setTimeout>);
    },
    onState: (connectivityState) => {
      windows.broadcast(["main", "popout"], "shell.connectivity", { state: connectivityState });
    },
  });
  const activity = new ActivityTracker({ maxItems: config.activityMaxItems, completedTtlMs: config.activityCompletedTtlMs, now: () => Date.now(), selfUserId: () => bridge.userId() });

  // The person's own secrets (ADP-009): the password manager and the import from another browser. Main window only; nothing here
  // reaches an agent, and no answer carries a password or a cookie value.
  const secrets = composeSecrets({
    config: localSecrets,
    userData,
    home: homedir(),
    cipher: safeStorage,
    ask: (box) => hooks.messageBox(box),
    server: bridge.server,
    workspaceId: () => bridge.workspaceId() ?? "",
    newCommandId,
  });

  // Local files (ADP-005): grants, granted folders, media URLs, safe edits and the shared watcher. The renderer names
  // nothing but opaque ids; the dialogs are main's.
  const files = new LocalFilesHost({
    config: loadLocalFilesConfig(join(bundleRoot, "local-files.yaml")),
    windows,
    dialogs: { pickFiles: (window) => hooks.dialogs.pickFiles(window), pickFolder: (window) => hooks.dialogs.pickFolder(window) },
    bridge,
    maxUploadBytes: MAX_UPLOAD_BYTES,
    log,
    // Every media request is attributed to the window that made it (below); one that cannot be is refused.
    requireMediaRequester: true,
    // The shell's own data directory (the session, the keys) is never a file or a folder a person's pick can reach.
    protectedPaths: [userData],
  });
  windows.onWindowEvent((window, event) => {
    if (event === "closed") {
      files.windowClosed(window);
    }
  });
  // A media URL is served only to the window it was minted for. `onBeforeRequest` names the sending window before the
  // protocol handler runs: another window is cancelled there, and the handler is told who asked.
  const mediaRequesters = new Map<string, number[]>();
  // The match pattern for every URL of the scheme, spelled with an escape: the source-scan tests strip comments with a
  // regular expression that would read a literal slash-star inside a string as the start of one.
  const everyMediaUrl = `${MEDIA_SCHEME}:\x2f\x2f*\x2f*`;
  defaultSession.webRequest.onBeforeRequest({ urls: [everyMediaUrl] }, (details, callback) => {
    const owner = files.media.windowOf(details.url);
    if (owner === null || details.webContentsId !== owner) {
      callback({ cancel: true });
      return;
    }
    mediaRequesters.set(details.url, [...(mediaRequesters.get(details.url) ?? []), owner]);
    callback({});
  });
  protocol.handle(MEDIA_SCHEME, (request) => {
    const queue = mediaRequesters.get(request.url) ?? [];
    const requester = queue.shift() ?? null;
    if (queue.length === 0) {
      mediaRequesters.delete(request.url);
    }
    const answer = files.mediaResponse({ method: request.method, url: request.url, range: request.headers.get("range"), requester });
    const body = answer.body === null ? null : (Readable.toWeb(answer.body()) as unknown as ReadableStream);
    return new Response(body, { status: answer.status, headers: { ...answer.headers } });
  });

  host = new ShellHost({
    platform: process.platform,
    config,
    preferences,
    windows,
    bridge,
    shortcuts,
    keepAwake,
    quit,
    updateGate,
    updates,
    connectivity,
    activity,
    displays: electronDisplays,
    pointer: () => screen.getCursorScreenPoint(),
    setMenu: (items) => {
      Menu.setApplicationMenu(
        electronMenu(items, (item) => {
          const action = item.action;
          if (action?.kind === "command") {
            host?.command(action.command);
          } else if (action?.kind === "check-for-updates") {
            host?.checkForUpdates();
          } else if (action?.kind === "quit") {
            host?.requestQuit();
          }
        }),
      );
    },
    checkForUpdates: () => {
      updates.checkForUpdates();
    },
    quitApp: () => {
      app.quit();
    },
    log,
  });
  const shellHost: ShellHost = host;

  // Every IPC channel goes through the guard: the sender's URL, its window type and its thread are checked in main
  // before a handler runs (DOMAIN.md §13.6 B).
  const guard = new IpcGuard(windows, log);
  const bridgeHandlers = bridge.handlers();
  const shellHandlers = host.handlers();
  const handlers = {
    ...Object.fromEntries(Object.entries(bridgeHandlers).map(([channel, handler]) => [channel, (_window: unknown, payload: unknown) => handler(payload)])),
    ...shellHandlers,
    ...files.handlers(),
    ...secrets.host.handlers(),
  } as Readonly<Record<PreloadChannel, WindowHandler>>;
  registerGuardedHandlers(ipcMain, guard, handlers);

  const openLink = (url: string): void => {
    // `quansio://attach` asks for a native file dialog and nothing else: a link names no file (ADP-005).
    const parsed = parseDeepLink(url);
    if (parsed?.kind === "attach-files") {
      const main = windows.showMain();
      files.requestAttach(main, parsed.threadId).catch((error: unknown) => {
        log(`quansio desktop: the attach dialog failed: ${error instanceof Error ? error.message : String(error)}`);
      });
      return;
    }
    const intent = bridge.openDeepLink(url);
    if (intent?.kind === "oauth-callback") {
      // The provider's return trip: main completes the exchange; the renderer only learns
      // the resulting session state.
      bridge.completeOAuth(intent.code, intent.state).catch((error: unknown) => {
        process.stderr.write(`quansio desktop: OAuth sign-in failed: ${error instanceof Error ? error.message : String(error)}\n`);
      });
    } else if (intent) {
      windows.showMain();
    }
  };
  app.on("open-url", (event, url) => {
    event.preventDefault();
    openLink(url);
  });
  app.on("second-instance", (_event, argv) => {
    const link = deepLinkFromArgv(argv);
    if (link) {
      openLink(link);
    } else {
      windows.showMain();
    }
  });
  app.on("activate", () => {
    windows.showMain();
  });

  // Quitting asks first while work is running (DOMAIN.md §13.6 E): `before-quit` is cancelled until the answer is in.
  let installStarted = false;
  app.on("before-quit", (event) => {
    if (!quit.permitted) {
      event.preventDefault();
      void quit.confirm().then((go) => {
        if (go) {
          app.quit();
        }
      });
      return;
    }
    state.quitting = true;
    if (updates.installOnQuit && updates.downloaded && !installStarted) {
      // Install-on-quit rides on a quit the person has already been asked about.
      installStarted = true;
      updates.install();
    }
    bridge.close();
  });
  app.on("will-quit", () => {
    files.dispose();
    worker.dispose();
    shortcuts.releaseAll();
    keepAwake.dispose();
    connectivity.dispose();
  });

  // Power and display signals feed the shell; each is one call into the host.
  keepAwake.update({ onAcPower: !powerMonitor.isOnBatteryPower() });
  powerMonitor.on("on-ac", () => {
    shellHost.onPowerSource(true);
  });
  powerMonitor.on("on-battery", () => {
    shellHost.onPowerSource(false);
  });
  powerMonitor.on("resume", () => {
    shellHost.onResume();
  });
  const displaysChanged = (): void => {
    shellHost.onDisplaysChanged();
  };
  screen.on("display-added", displaysChanged);
  screen.on("display-removed", displaysChanged);
  screen.on("display-metrics-changed", displaysChanged);

  // A tray on Windows and Linux (Open, Quit); macOS has the Dock instead.
  if (wantsTray(process.platform)) {
    try {
      const tray = new Tray(nativeImage.createFromBuffer(trayIconPng(32)));
      tray.setToolTip("Quansio");
      tray.setContextMenu(
        Menu.buildFromTemplate(
          trayMenu().map((entry) => ({
            label: entry.label,
            click: () => {
              if (entry.action === "open") {
                windows.showMain();
              } else {
                shellHost.requestQuit();
              }
            },
          })),
        ),
      );
      tray.on("click", () => {
        windows.showMain();
      });
      state.trayAvailable = true;
    } catch (error) {
      log(`quansio desktop: no tray is available (${error instanceof Error ? error.message : String(error)}); closing the window minimizes it`);
    }
  }

  host.start();
  await bridge.resume();
  windows.showMain();
  if (process.env["QUANSIO_DESKTOP_TEST_HOOKS"] === "1") {
    // For the real-Electron smoke tests only: a handle on the shell's parts, and a replaceable message box so a dialog
    // does not block a headless run. Nothing reads it in a normal launch.
    (globalThis as Record<string, unknown>)["__quansioShell"] = { host, windows, quit, updateGate, updates, shortcuts, activity, preferences, bridge, connectivity, keepAwake, hooks, guard, files, openLink };
  }
  return { bridge, host, windows, openLink };
}

/**
 * The `quansio-recorder` executable this build ships (macOS only): `QUANSIO_RECORDER_BIN` for development,
 * otherwise beside the app's resources. Absent, recording says so instead of failing late.
 */
function recorderBinaryPath(): string | undefined {
  if (process.platform !== "darwin") {
    return undefined;
  }
  const candidates = [process.env["QUANSIO_RECORDER_BIN"], join(process.resourcesPath, "quansio-recorder")];
  return candidates.find((path): path is string => typeof path === "string" && path.length > 0 && existsSync(path));
}

/** An option that is present only when it has a value (the options type forbids an explicit undefined). */
function optional<K extends string, V>(key: K, value: V | undefined): { [P in K]?: V } {
  return value === undefined ? {} : ({ [key]: value } as { [P in K]?: V });
}

/**
 * Electron's view of the OS permissions native computer use needs, on macOS. Accessibility, Screen
 * Recording and Microphone have probes; Input Monitoring does not, and is reported as not checked
 * (`UNPROBED_PERMISSIONS`) rather than guessed. The prompt flag is always false: reading a status never
 * asks the person for anything.
 */
function macPermissionSource(): PermissionSource | undefined {
  if (process.platform !== "darwin") {
    return undefined;
  }
  return {
    read: () =>
      Promise.resolve({
        status: {
          accessibility: systemPreferences.isTrustedAccessibilityClient(false) ? "granted" : "denied",
          screen_recording: mapMediaAccess(systemPreferences.getMediaAccessStatus("screen")),
          input_monitoring: "not_determined",
          microphone: mapMediaAccess(systemPreferences.getMediaAccessStatus("microphone")),
        },
        unprobed: UNPROBED_PERMISSIONS,
      }),
    idleSeconds: () => powerMonitor.getSystemIdleTime(),
  };
}

/** Process entry: single instance, protocol client, then the windows. */
export function main(): void {
  if (!app.requestSingleInstanceLock()) {
    app.quit();
    return;
  }
  registerRendererScheme();
  app.setAsDefaultProtocolClient(DEEP_LINK_SCHEME);
  app.whenReady().then(async () => {
    const { openLink } = await startDesktop();
    // The first instance's own argv may carry the deep link on Windows/Linux.
    const link = deepLinkFromArgv(process.argv);
    if (link) {
      openLink(link);
    }
  }).catch((error: unknown) => {
    process.stderr.write(`quansio desktop: ${error instanceof Error ? error.message : String(error)}\n`);
    app.exit(1);
  });
  app.on("window-all-closed", () => {
    // Windows and Linux leave when the last window is gone (the quit confirmation still applies); macOS stays in the Dock.
    if (process.platform !== "darwin") {
      app.quit();
    }
  });
}

main();
