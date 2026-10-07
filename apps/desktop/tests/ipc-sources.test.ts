/**
 * No door around the IPC guard (ADP-004, DOMAIN.md 13.6 B): a source scan, because the property is about what the code
 * contains. Every invoke is installed by `registerGuardedHandlers`; main has no other `ipcMain` listener (an event, a
 * one-shot, a synchronous reply), and the preload offers the page no raw `ipcRenderer`.
 */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "src");

function sources(directory: string): { path: string; code: string }[] {
  return readdirSync(join(SRC, directory), { withFileTypes: true })
    .filter((entry) => entry.isFile() && /\.tsx?$/.test(entry.name))
    .map((entry) => ({
      path: `${directory}/${entry.name}`,
      // Comments are prose; what they say is not what the code does.
      code: readFileSync(join(SRC, directory, entry.name), "utf8").replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, ""),
    }));
}

describe("no door around the guard", () => {
  it("main and the preload have no IPC path the guard does not see", () => {
    const offenders: string[] = [];
    for (const file of [...sources("main"), ...sources("preload")]) {
      if (/ipcMain\s*\.\s*(on|once|handleOnce|addListener)\b/.test(file.code) || /\bsendSync\b|ipcRenderer\s*\.\s*(send|postMessage)\b/.test(file.code)) {
        offenders.push(file.path);
      }
      // `protocol.handle` serves the bundled renderer in app.ts; the guard's own registration is the other.
      if (/\.handle\(/.test(file.code) && !["main/ipc-trust.ts", "main/app.ts"].includes(file.path)) {
        offenders.push(`${file.path} (handle)`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("the only ipcMain use is the guard's registration, and the preload invokes only allowlisted channels", () => {
    const app = sources("main").find((file) => file.path === "main/app.ts")?.code ?? "";
    // The import, and the one registration.
    expect(app.match(/\bipcMain\b/g)?.length).toBe(2);
    expect(app).toContain("registerGuardedHandlers(ipcMain, guard, handlers)");
    const preload = sources("preload").find((file) => file.path === "preload/index.ts")?.code ?? "";
    expect(preload).toContain("preloadAllows(channel)");
    expect(preload).toContain("pushAllows(channel)");
    // The raw ipcRenderer is used inside the preload and exposed to nothing.
    expect(preload).not.toMatch(/exposeInMainWorld\([^)]*ipcRenderer/);
  });

  it("the shell's own modules name no credential store", () => {
    for (const file of ["main/shell-host.ts", "main/windows.ts", "main/ipc-trust.ts", "main/activity.ts", "main/shortcuts.ts", "main/menu.ts", "main/update-gate.ts", "main/quit.ts"]) {
      const code = sources("main").find((candidate) => candidate.path === file)?.code ?? "";
      expect(code.length, file).toBeGreaterThan(0);
      expect(code, file).not.toMatch(/refresh_?token|accessToken|TokenStore|safeStorage|authorization/i);
    }
  });

  it("the local-file host reads no file itself, and nothing in main or the renderer keeps a grant token (ADP-005)", () => {
    // The handlers (host.ts) work through the registries; only the modules that hold descriptors import the filesystem,
    // so there is no handler that could read a path a page supplied. None of them imports Electron.
    const files = sources("main/local-files");
    expect(files.map((file) => file.path).sort()).toEqual(
      ["config", "fs-ops", "grants", "host", "media", "results", "roots", "safe-edit", "watcher"].map((name) => `main/local-files/${name}.ts`),
    );
    for (const file of files) {
      expect(file.code, file.path).not.toMatch(/from "electron"|require\("electron"\)|ipcMain|ipcRenderer|\bwebContents\b/);
      expect(file.code, file.path).not.toMatch(/refresh_?token|accessToken|TokenStore|safeStorage|authorization/i);
    }
    const importsFs = files.filter((file) => /from "node:fs"/.test(file.code)).map((file) => file.path.replace("main/local-files/", ""));
    expect(importsFs.sort()).toEqual(["config.ts", "fs-ops.ts", "media.ts", "roots.ts", "safe-edit.ts", "watcher.ts"]);
    // A grant token is held in memory by the page and main only: the renderer files that handle one never persist anything.
    for (const file of sources("renderer").filter((candidate) => /pickLocalFiles|attachLocalFiles|onFileGrant|discardLocalFiles/.test(candidate.code))) {
      expect(file.code, file.path).not.toMatch(/localStorage|sessionStorage|indexedDB|document\.cookie|caches\./);
    }
  });

  it("the shell never relaxes the page's security: no CORS stripping, no wider CSP, no webSecurity off", () => {
    const text = sources("main").map((file) => file.code).join("\n");
    expect(text).not.toMatch(/webSecurity\s*:\s*false/);
    expect(text).not.toMatch(/access-control-allow-origin/i);
    expect(text).not.toMatch(/disable-web-security|--no-sandbox/);
    // The one place a response header is touched is the CSP wrapper, which replaces the policy with the baseline's.
    expect(text.match(/onHeadersReceived/g)?.length).toBe(1);
  });
});
