/**
 * Desktop shell composition (APP-003).
 *
 * Main process wiring: security baseline, preload path, deep-link and update hooks.
 * It does not own runs, effects or secrets.
 */

import {
  RENDERER_CSP,
  RENDERER_WEB_PREFERENCES,
  permissionAllowed,
} from "./security.js";
import { DesktopSessionStore } from "../renderer/session.js";
import { commandUrl, streamUrl } from "../client/server.js";

export interface ShellConfig {
  readonly serverOrigin: string;
  readonly tenantId: string;
}

export interface DesktopShell {
  readonly webPreferences: typeof RENDERER_WEB_PREFERENCES;
  readonly csp: string;
  readonly session: DesktopSessionStore;
  readonly commandUrl: (name: string) => string;
  readonly streamUrl: string;
  permissionAllowed: (permission: string) => boolean;
}

/** Compose the shell the Electron main process would start. */
export function createDesktopShell(config: ShellConfig): DesktopShell {
  const session = new DesktopSessionStore();
  return {
    webPreferences: RENDERER_WEB_PREFERENCES,
    csp: RENDERER_CSP,
    session,
    commandUrl: (name: string) => commandUrl(config.serverOrigin, name),
    streamUrl: streamUrl(config.serverOrigin),
    permissionAllowed,
  };
}
