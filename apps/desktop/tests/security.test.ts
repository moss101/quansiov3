import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  createDesktopShell,
  createPreloadApi,
  isForbiddenRendererApi,
  preloadAllows,
  rendererSecurityBaselineHolds,
  STRINGS,
} from "../src/index.js";
import { DesktopSessionStore } from "../src/renderer/session.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

describe("renderer security baseline", () => {
  it("disables Node in the renderer and isolates the preload", () => {
    expect(rendererSecurityBaselineHolds()).toBe(true);
    const shell = createDesktopShell({
      serverOrigin: "https://127.0.0.1:8443",
      tenantId: "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    });
    expect(shell.webPreferences.nodeIntegration).toBe(false);
    expect(shell.webPreferences.contextIsolation).toBe(true);
    expect(shell.webPreferences.sandbox).toBe(true);
    expect(shell.csp).toContain("script-src 'self'");
    expect(shell.csp).toContain("object-src 'none'");
    expect(shell.permissionAllowed("media")).toBe(false);
    expect(shell.permissionAllowed("camera")).toBe(false);
  });

  it("exposes only the allowlisted preload channels", () => {
    const calls: string[] = [];
    const api = createPreloadApi((channel) => {
      calls.push(channel);
      return null;
    });
    expect(Object.keys(api).sort()).toEqual(
      [
        "activeRun",
        "invokeCommand",
        "openDeepLink",
        "reconnectStream",
        "subscribeStream",
        "updateStatus",
      ].sort(),
    );
    expect(preloadAllows("commands.invoke")).toBe(true);
    expect(preloadAllows("fs.read")).toBe(false);
    expect(isForbiddenRendererApi("fs")).toBe(true);
    expect(isForbiddenRendererApi("child_process")).toBe(true);
    api.invokeCommand({
      name: "CancelRun",
      commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      params: {},
    });
    expect(calls).toEqual(["commands.invoke"]);
  });

  it("restores the active run and stream cursor after reconnect", () => {
    const store = new DesktopSessionStore();
    store.attach({
      runId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      generation: 4,
      streamCursor: "cursor-4",
      workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    });
    const restored = store.reconnect();
    expect(restored?.runId).toBe("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(restored?.streamCursor).toBe("cursor-4");
    expect(restored?.generation).toBe(4);
  });

  it("keeps product copy out of the renderer modules", () => {
    expect(STRINGS.appName).toBe("Quansio");
    expect(STRINGS.reconnecting.length).toBeGreaterThan(0);
  });

  it("does not import Node host APIs from renderer or preload", () => {
    const files = [
      join(ROOT, "src/preload/bridge.ts"),
      join(ROOT, "src/renderer/session.ts"),
      join(ROOT, "src/i18n/strings.ts"),
    ];
    for (const file of files) {
      const text = readFileSync(file, "utf8");
      expect(text).not.toMatch(/from ["']node:fs["']/);
      expect(text).not.toMatch(/from ["']node:child_process["']/);
      expect(text).not.toMatch(/nodeIntegration:\s*true/);
    }
    const src = join(ROOT, "src");
    for (const name of readdirSync(src)) {
      expect(name).not.toBe("electron-remote");
    }
  });
});
