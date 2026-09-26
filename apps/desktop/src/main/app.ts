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

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { app, autoUpdater, BrowserWindow, dialog, ipcMain, net, Notification, protocol, safeStorage, session, shell } from "electron";
import WebSocket from "ws";

import type { SocketFactory, SocketLike } from "../client/stream.js";
import { deepLinkFromArgv, DEEP_LINK_SCHEME } from "./deeplinks.js";
import { DesktopBridge, registerBridgeHandlers, type DesktopAuth } from "./ipc.js";
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
import { TokenStore } from "./token-store.js";
import { parseUpdateChannel, updateFeedUrl, UpdateHooks } from "./updates.js";

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
    minWidth: 720,
    minHeight: 520,
    show: false,
    // A macOS window like its peers: no title bar, the traffic lights inset over the sidebar
    // (the renderer leaves room for them and marks where the window is dragged from).
    ...(process.platform === "darwin" ? { titleBarStyle: "hiddenInset" as const, trafficLightPosition: { x: 18, y: 18 } } : {}),
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

  const auth: DesktopAuth = environment.harness
    ? { kind: "trusted-headers", scope: { tenantId: environment.harness.tenantId, userId: environment.harness.userId }, workspaceId: environment.harness.workspaceId }
    : { kind: "session", tokens: new TokenStore(app.getPath("userData"), safeStorage) };
  const bridge = new DesktopBridge({
    serverOrigin: environment.serverOrigin,
    auth,
    // OAuth: the authorization URL opens in the system browser, never in the renderer.
    openExternal: (url) => shell.openExternal(url),
    // Native desktop notifications (APP-010): only for events the person's preference sends
    // to the desktop channel; clicking one brings the window forward.
    notifyNative: (notice) => {
      if (Notification.isSupported()) {
        const native = new Notification({ title: notice.title, body: notice.body, silent: false });
        native.on("click", () => {
          window.show();
        });
        native.show();
      }
    },
    // Artifact files (APP-008): saved where the user chooses, or written under userData and
    // handed to the OS handler; the bridge refuses pages, scripts and programs before this.
    files: {
      saveAs: async (suggestedName, bytes) => {
        const chosen = await dialog.showSaveDialog(window, { defaultPath: suggestedName });
        if (chosen.canceled || !chosen.filePath) {
          return null;
        }
        writeFileSync(chosen.filePath, bytes, { mode: 0o600 });
        return chosen.filePath;
      },
      openWithOs: async (suggestedName, bytes) => {
        const directory = join(app.getPath("userData"), "artifacts");
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

  const openLink = (url: string): void => {
    const intent = bridge.openDeepLink(url);
    if (intent?.kind === "oauth-callback") {
      // The provider's return trip: main completes the exchange; the renderer only learns
      // the resulting session state.
      bridge.completeOAuth(intent.code, intent.state).catch((error: unknown) => {
        process.stderr.write(`quansio desktop: OAuth sign-in failed: ${error instanceof Error ? error.message : String(error)}\n`);
      });
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
    }
    window.show();
  });

  app.on("before-quit", () => {
    bridge.close();
  });

  window.once("ready-to-show", () => {
    window.show();
  });
  await bridge.resume();
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
      const intent = bridge.openDeepLink(link);
      if (intent?.kind === "oauth-callback") {
        await bridge.completeOAuth(intent.code, intent.state);
      }
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
