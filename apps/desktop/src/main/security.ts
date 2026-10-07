/**
 * Renderer security baseline (APP-003, DOSSIER.md §15).
 *
 * These values are the BrowserWindow webPreferences and session CSP the main process
 * must apply. Tests assert the shipped object, so a regression that re-enables Node in
 * the renderer fails here rather than in a screenshot.
 */

/** CSP that forbids remote script, inline eval and object plugins. */
export const RENDERER_CSP =
  "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: quansio-media:; media-src quansio-media:; connect-src 'self' https: wss:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";

/**
 * The policy a local media response (ADP-005, DOMAIN.md 13.7 C) carries. It is not the page's: a file the person picked
 * is served sandboxed, with no script, no form, no navigation, whatever it contains.
 */
export const MEDIA_RESPONSE_CSP = "sandbox";

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
  /** Same-origin policy stays on; the shell never strips CORS or widens the CSP (ADP-004). */
  readonly webSecurity: boolean;
  readonly allowRunningInsecureContent: boolean;
}

/** Electron webPreferences the main process applies to every renderer. */
export const RENDERER_WEB_PREFERENCES: RendererWebPreferences = {
  nodeIntegration: false,
  nodeIntegrationInWorker: false,
  nodeIntegrationInSubFrames: false,
  contextIsolation: true,
  sandbox: true,
  enableRemoteModule: false,
  preload: "preload.cjs",
  webviewTag: false,
  webSecurity: true,
  allowRunningInsecureContent: false,
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
    RENDERER_WEB_PREFERENCES.webSecurity &&
    !RENDERER_WEB_PREFERENCES.allowRunningInsecureContent &&
    RENDERER_CSP.includes("object-src 'none'") &&
    RENDERER_CSP.includes("script-src 'self'")
  );
}

// ---------------------------------------------------------------------------------------
// Handlers the main process installs. They are written over the narrowest slice of the
// Electron objects they touch so the CSP/preload and permission-boundary tests drive the
// very functions `app.ts` registers, without an Electron binary.
// ---------------------------------------------------------------------------------------

/** Origins the renderer may navigate to: only the bundled renderer itself. */
export const RENDERER_ORIGIN_SCHEME = "app:";

/** The renderer document, served from the bundle under the `app:` scheme. */
export const RENDERER_ENTRY_URL = "app://desktop/index.html";

export interface HeadersReceivedDetails {
  readonly responseHeaders?: Record<string, string[]> | undefined;
  readonly url: string;
}

/** `session.webRequest.onHeadersReceived` handler: every renderer response carries the CSP. */
export function withContentSecurityPolicy(
  details: HeadersReceivedDetails,
): { responseHeaders: Record<string, string[]> } {
  const headers: Record<string, string[]> = {};
  for (const [key, value] of Object.entries(details.responseHeaders ?? {})) {
    if (key.toLowerCase() !== "content-security-policy") {
      headers[key] = value;
    }
  }
  // A local media response is a file, not a page: it is sandboxed, and the page's own policy does not replace that.
  headers["Content-Security-Policy"] = [details.url.startsWith("quansio-media:") ? MEDIA_RESPONSE_CSP : RENDERER_CSP];
  return { responseHeaders: headers };
}

/** `session.setPermissionRequestHandler` decision; `permissionAllowed` is the single source. */
export function permissionRequestDecision(permission: string): boolean {
  return permissionAllowed(permission);
}

/** `will-navigate` guard: the renderer stays on the bundled document. */
export function navigationAllowed(targetUrl: string): boolean {
  try {
    const url = new URL(targetUrl);
    return url.protocol === RENDERER_ORIGIN_SCHEME && url.host === "desktop";
  } catch {
    return false;
  }
}

/** `setWindowOpenHandler` decision: the renderer never opens windows; links go to the OS. */
export function windowOpenDecision(targetUrl: string): { action: "deny"; openExternally: boolean } {
  let openExternally = false;
  try {
    const url = new URL(targetUrl);
    openExternally = url.protocol === "https:";
  } catch {
    openExternally = false;
  }
  return { action: "deny", openExternally };
}

/** The preload script path the window must be created with, relative to the bundle root. */
export function preloadPath(bundleRoot: string): string {
  return `${bundleRoot.replace(/\/+$/, "")}/preload.cjs`;
}
