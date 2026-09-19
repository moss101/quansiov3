/**
 * Electron main process (APP-003).
 *
 * The one module that composes the shell against Electron: a single window with the
 * renderer security baseline applied through real handlers, the preload bridge, deep links,
 * update hooks and the durable session. Runtime authority stays in `quansio-server`; this
 * process holds the session scope and opens the stream socket on the renderer's behalf.
 *
 * Everything decision-shaped lives in the sibling modules (`security.ts`, `ipc.ts`,
 * `deeplinks.ts`, `updates.ts`, `session-file.ts`) so the tests can drive it without an
 * Electron binary; this file wires them.
 */

import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { app, autoUpdater, BrowserWindow, ipcMain, protocol, session, shell } from "electron";
import WebSocket from "ws";

import type { SocketFactory, SocketLike } from "../client/stream.js";
import { deepLinkFromArgv, DEEP_LINK_SCHEME } from "./deeplinks.js";
import { DesktopBridge, registerBridgeHandlers } from "./ipc.js";
import {
  navigationAllowed,
  permissionRequestDecision,
  preloadPath,
  RENDERER_ENTRY_URL,
  RENDERER_WEB_PREFERENCES,
  windowOpenDecision,
  withContentSecurityPolicy,
} from "./security.js";
import { SessionFile } from "./session-file.js";
import { parseUpdateChannel, updateFeedUrl, UpdateHooks } from "./updates.js";

/** Where the desktop gets its session scope until APP-002 supplies it from the keychain. */
export interface ShellEnvironment {
  readonly serverOrigin: string;
  readonly tenantId: string;
  readonly userId: string;
  readonly workspaceId: string;
  readonly updateChannel: "stable" | "beta";
  readonly signed: boolean;
}

export function shellEnvironment(env: NodeJS.ProcessEnv): ShellEnvironment {
  const required = (name: string): string => {
    const value = env[name]?.trim();
    if (!value) {
      throw new Error(`${name} is not set; the desktop has no session scope (APP-002 supplies it from the keychain)`);
    }
    return value;
  };
  return {
    serverOrigin: env["QUANSIO_SERVER_ORIGIN"]?.trim() || "http://127.0.0.1:8080",
    tenantId: required("QUANSIO_TENANT_ID"),
    userId: required("QUANSIO_USER_ID"),
    workspaceId: required("QUANSIO_WORKSPACE_ID"),
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
    return fetch(pathToFileURL(join(rendererRoot, relative)).href);
  });
}

export async function startDesktop(): Promise<{ window: BrowserWindow; bridge: DesktopBridge }> {
  const environment = shellEnvironment(process.env);
  const sessionFile = new SessionFile(app.getPath("userData"));
  const updates = new UpdateHooks(
    environment.updateChannel,
    autoUpdater,
    updateFeedUrl(environment.serverOrigin, environment.updateChannel, process.platform, process.arch),
    environment.signed,
  );

  const defaultSession = session.defaultSession;
  defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback(withContentSecurityPolicy({ responseHeaders: details.responseHeaders, url: details.url }));
  });
  defaultSession.setPermissionRequestHandler((_contents, permission, callback) => {
    callback(permissionRequestDecision(permission));
  });
  defaultSession.setPermissionCheckHandler((_contents, permission) => permissionRequestDecision(permission));
  serveRenderer();

  const window = new BrowserWindow({
    width: 1280,
    height: 860,
    show: false,
    webPreferences: { ...RENDERER_WEB_PREFERENCES, preload: preloadPath(bundleRoot) },
  });
  window.webContents.on("will-navigate", (event, url) => {
    if (!navigationAllowed(url)) {
      event.preventDefault();
    }
  });
  window.webContents.setWindowOpenHandler(({ url }) => {
    const decision = windowOpenDecision(url);
    if (decision.openExternally) {
      void shell.openExternal(url);
    }
    return { action: decision.action };
  });

  const bridge = new DesktopBridge({
    serverOrigin: environment.serverOrigin,
    scope: { tenantId: environment.tenantId, userId: environment.userId },
    workspaceId: environment.workspaceId,
    session: sessionFile,
    openSocket,
    updates,
    consumer: `desktop:${String(process.pid)}`,
    push: (channel, payload) => {
      if (!window.isDestroyed()) {
        window.webContents.send(channel, payload);
      }
    },
  });
  registerBridgeHandlers(ipcMain, bridge);
  updates.onStatus((status) => {
    if (!window.isDestroyed()) {
      window.webContents.send("update.status", status);
    }
  });

  app.on("open-url", (event, url) => {
    event.preventDefault();
    bridge.openDeepLink(url);
  });
  app.on("second-instance", (_event, argv) => {
    const link = deepLinkFromArgv(argv);
    if (link) {
      bridge.openDeepLink(link);
    }
    window.show();
  });
  app.on("before-quit", () => {
    bridge.close();
  });

  window.once("ready-to-show", () => {
    window.show();
  });
  await window.loadURL(RENDERER_ENTRY_URL);
  return { window, bridge };
}

/** Process entry: single instance, protocol client, then the window. */
export function main(): void {
  if (!app.requestSingleInstanceLock()) {
    app.quit();
    return;
  }
  registerRendererScheme();
  app.setAsDefaultProtocolClient(DEEP_LINK_SCHEME);
  app.whenReady().then(async () => {
    const { bridge } = await startDesktop();
    // The first instance's own argv may carry the deep link on Windows/Linux.
    const link = deepLinkFromArgv(process.argv);
    if (link) {
      bridge.openDeepLink(link);
    }
  }).catch((error: unknown) => {
    process.stderr.write(`quansio desktop: ${error instanceof Error ? error.message : String(error)}\n`);
    app.exit(1);
  });
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") {
      app.quit();
    }
  });
}

main();
