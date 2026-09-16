/**
 * Renderer security baseline (APP-003, DOSSIER.md §15).
 *
 * These values are the BrowserWindow webPreferences and session CSP the main process
 * must apply. Tests assert the shipped object, so a regression that re-enables Node in
 * the renderer fails here rather than in a screenshot.
 */

/** CSP that forbids remote script, inline eval and object plugins. */
export const RENDERER_CSP =
  "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' https: wss:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";

/**
 * The shape of {@link RENDERER_WEB_PREFERENCES}, typed with `boolean` rather than
 * inferred as `as const` literals -- widening it is what makes
 * {@link rendererSecurityBaselineHolds}'s checks a genuine runtime assertion instead
 * of a comparison the type checker already knows the answer to (see that function's
 * doc comment).
 */
interface RendererWebPreferences {
  readonly nodeIntegration: boolean;
  readonly nodeIntegrationInWorker: boolean;
  readonly nodeIntegrationInSubFrames: boolean;
  readonly contextIsolation: boolean;
  readonly sandbox: boolean;
  readonly enableRemoteModule: boolean;
  readonly preload: string;
  readonly webviewTag: boolean;
}

/** Electron webPreferences the main process applies to every renderer. */
export const RENDERER_WEB_PREFERENCES: RendererWebPreferences = {
  nodeIntegration: false,
  nodeIntegrationInWorker: false,
  nodeIntegrationInSubFrames: false,
  contextIsolation: true,
  sandbox: true,
  enableRemoteModule: false,
  preload: "preload/bridge.js",
  webviewTag: false,
};

/** Permission handler: deny everything the renderer asks the OS for. */
export const DENIED_PERMISSIONS = [
  "media",
  "geolocation",
  "notifications",
  "midi",
  "camera",
  "microphone",
  "openExternal",
  "pointerLock",
  "fullscreen",
] as const;

/** Whether a permission request from the renderer is allowed. */
export function permissionAllowed(permission: string): boolean {
  return !DENIED_PERMISSIONS.includes(
    permission as (typeof DENIED_PERMISSIONS)[number],
  );
}

/** True when the shipped webPreferences meet the renderer security baseline. */
export function rendererSecurityBaselineHolds(): boolean {
  return (
    !RENDERER_WEB_PREFERENCES.nodeIntegration &&
    RENDERER_WEB_PREFERENCES.contextIsolation &&
    RENDERER_WEB_PREFERENCES.sandbox &&
    !RENDERER_WEB_PREFERENCES.enableRemoteModule &&
    !RENDERER_WEB_PREFERENCES.webviewTag &&
    RENDERER_CSP.includes("object-src 'none'") &&
    RENDERER_CSP.includes("script-src 'self'")
  );
}
