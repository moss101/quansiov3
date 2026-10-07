/**
 * CSP/preload test (APP-003, acceptance 1): the renderer security baseline, asserted on the
 * very functions `main/app.ts` installs, plus the source-level guarantee that nothing the
 * renderer can load imports a Node host API.
 */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  createDesktopShell,
  navigationAllowed,
  permissionRequestDecision,
  preloadPath,
  rendererSecurityBaselineHolds,
  MEDIA_RESPONSE_CSP,
  RENDERER_CSP,
  RENDERER_ENTRY_URL,
  RENDERER_WEB_PREFERENCES,
  STRINGS,
  windowOpenDecision,
  withContentSecurityPolicy,
} from "../src/index.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

describe("renderer security baseline", () => {
  it("disables Node in the renderer and isolates the preload", () => {
    expect(rendererSecurityBaselineHolds()).toBe(true);
    const shell = createDesktopShell({ serverOrigin: "https://127.0.0.1:8443" });
    expect(shell.webPreferences.nodeIntegration).toBe(false);
    expect(shell.webPreferences.nodeIntegrationInWorker).toBe(false);
    expect(shell.webPreferences.nodeIntegrationInSubFrames).toBe(false);
    expect(shell.webPreferences.contextIsolation).toBe(true);
    expect(shell.webPreferences.sandbox).toBe(true);
    expect(shell.webPreferences.webviewTag).toBe(false);
    expect(shell.csp).toContain("script-src 'self'");
    expect(shell.csp).toContain("object-src 'none'");
    expect(shell.csp).not.toContain("unsafe-eval");
    expect(shell.rendererEntry).toBe(RENDERER_ENTRY_URL);
    expect(shell.commandUrl("CancelRun")).toBe("https://127.0.0.1:8443/v1/commands/CancelRun");
    expect(shell.streamUrl).toBe("wss://127.0.0.1:8443/v1/stream");
    expect(preloadPath("/bundle/app/")).toBe("/bundle/app/preload.cjs");
    expect(RENDERER_WEB_PREFERENCES.preload).toBe("preload.cjs");
  });

  it("injects the CSP on every renderer response, replacing whatever the response carried", () => {
    const relaxed = { responseHeaders: { "content-security-policy": ["default-src *"], "X-Other": ["1"] }, url: RENDERER_ENTRY_URL };
    const { responseHeaders } = withContentSecurityPolicy(relaxed);
    expect(responseHeaders["Content-Security-Policy"]).toEqual([RENDERER_CSP]);
    expect(Object.keys(responseHeaders).filter((key) => key.toLowerCase() === "content-security-policy")).toHaveLength(1);
    expect(responseHeaders["X-Other"]).toEqual(["1"]);
    expect(withContentSecurityPolicy({ url: RENDERER_ENTRY_URL }).responseHeaders["Content-Security-Policy"]).toEqual([RENDERER_CSP]);
  });

  it("lets the page show local media and nothing else of that scheme, and keeps a media response sandboxed (ADP-005)", () => {
    const directives = Object.fromEntries(RENDERER_CSP.split(";").map((part) => part.trim().split(/\s+/)).map(([name, ...values]) => [name, values])) as Record<string, string[]>;
    // Only images and audio/video elements may load it: not script, style, frames, fetch or anything by default.
    const allowing = Object.entries(directives).filter(([, values]) => values.includes("quansio-media:")).map(([name]) => name);
    expect(allowing.sort()).toEqual(["img-src", "media-src"]);
    expect(RENDERER_CSP).not.toContain("*");
    // A media response is a file, not a page: the page's policy does not replace the sandbox, and nothing else is added.
    const media = withContentSecurityPolicy({ url: `quansio-media://m/${"0".repeat(64)}/a.png`, responseHeaders: { "Content-Security-Policy": ["default-src *"] } });
    expect(media.responseHeaders["Content-Security-Policy"]).toEqual([MEDIA_RESPONSE_CSP]);
    expect(MEDIA_RESPONSE_CSP).toBe("sandbox");
    // The page cannot navigate to it or open it in a window.
    expect(navigationAllowed(`quansio-media://m/${"0".repeat(64)}/a.png`)).toBe(false);
    expect(windowOpenDecision(`quansio-media://m/${"0".repeat(64)}/a.png`)).toEqual({ action: "deny", openExternally: false });
  });

  it("denies device permissions and pins navigation to the bundled renderer", () => {
    for (const permission of ["media", "camera", "microphone", "geolocation", "notifications", "openExternal", "fullscreen"]) {
      expect(permissionRequestDecision(permission)).toBe(false);
    }
    expect(navigationAllowed(RENDERER_ENTRY_URL)).toBe(true);
    expect(navigationAllowed("app://desktop/threads/thr_x")).toBe(true);
    expect(navigationAllowed("https://example.com/")).toBe(false);
    expect(navigationAllowed("file:///etc/passwd")).toBe(false);
    expect(navigationAllowed("app://elsewhere/index.html")).toBe(false);
    expect(navigationAllowed("not a url")).toBe(false);
    expect(windowOpenDecision("https://example.com/docs")).toEqual({ action: "deny", openExternally: true });
    expect(windowOpenDecision("javascript:alert(1)")).toEqual({ action: "deny", openExternally: false });
    expect(windowOpenDecision("file:///tmp/x")).toEqual({ action: "deny", openExternally: false });
  });

  it("keeps product copy out of the renderer modules", () => {
    expect(STRINGS.appName).toBe("Quansio");
    expect(STRINGS.reconnecting.length).toBeGreaterThan(0);
    const app = readFileSync(join(ROOT, "src/renderer/App.tsx"), "utf8");
    // Every visible string in the renderer is a lookup, not a literal.
    expect(app).not.toMatch(/>\s*[A-Z][a-z]+ [a-z]+[^<{]*</);
  });

  it("does not import Node host APIs from anything the renderer can load", () => {
    const rendererFiles = readdirSync(join(ROOT, "src/renderer"))
      .filter((name) => name.endsWith(".ts") || name.endsWith(".tsx"))
      .map((name) => join(ROOT, "src/renderer", name));
    const files = [join(ROOT, "src/preload/bridge.ts"), join(ROOT, "src/preload/index.ts"), join(ROOT, "src/i18n/strings.ts"), ...rendererFiles];
    for (const file of files) {
      const text = readFileSync(file, "utf8");
      expect(text, file).not.toMatch(/from ["']node:/);
      expect(text, file).not.toMatch(/require\(/);
      expect(text, file).not.toMatch(/nodeIntegration:\s*true/);
    }
    // Only the main process and the preload script may import Electron.
    const electronImporters = readdirSync(join(ROOT, "src"), { recursive: true })
      .map(String)
      .filter((name) => /\.tsx?$/.test(name))
      .filter((name) => /from ["']electron["']/.test(readFileSync(join(ROOT, "src", name), "utf8")))
      .sort();
    expect(electronImporters).toEqual(["main/app.ts", "preload/index.ts"]);
  });
});
