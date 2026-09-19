/**
 * Desktop shell description (APP-003).
 *
 * The baseline `app.ts` applies, as data: the webPreferences and CSP every window gets,
 * the permission decision, and the public API endpoints for a server origin. `app.ts` is
 * the only module that touches Electron; this one lets tests and diagnostics see exactly
 * what it will apply without starting it.
 */

import { commandUrl, streamUrl } from "../client/server.js";
import { RENDERER_CSP, RENDERER_ENTRY_URL, RENDERER_WEB_PREFERENCES, permissionAllowed } from "./security.js";

export interface ShellConfig {
  readonly serverOrigin: string;
}

export interface DesktopShell {
  readonly webPreferences: typeof RENDERER_WEB_PREFERENCES;
  readonly csp: string;
  readonly rendererEntry: string;
  readonly commandUrl: (name: string) => string;
  readonly streamUrl: string;
  permissionAllowed: (permission: string) => boolean;
}

/** Describe the shell the Electron main process starts for `config`. */
export function createDesktopShell(config: ShellConfig): DesktopShell {
  return {
    webPreferences: RENDERER_WEB_PREFERENCES,
    csp: RENDERER_CSP,
    rendererEntry: RENDERER_ENTRY_URL,
    commandUrl: (name: string) => commandUrl(config.serverOrigin, name),
    streamUrl: streamUrl(config.serverOrigin),
    permissionAllowed,
  };
}
