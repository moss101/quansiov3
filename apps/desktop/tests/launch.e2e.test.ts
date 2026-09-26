/**
 * Desktop launch smoke (APP-003 regression): the real Electron binary starts the built app and
 * the window renders the bundled renderer from the `app:` scheme.
 *
 * Every other desktop test drives the bridge and the renderer modules in-process, so nothing
 * noticed that `app:` was served with Node's `fetch`, which cannot read `file:` URLs: every
 * launch showed a blank window. This test launches the binary itself through Playwright's
 * Electron driver (with a mock keychain and a throwaway user-data directory) and waits for the
 * sign-in form, which only exists once the renderer bundle has loaded and run.
 *
 * Needs the Electron binary (`pnpm install` with `electron` in `allowBuilds`) and the built app
 * (`pnpm --filter @quansio/desktop build`); without either it skips with `BLOCKED_EXTERNAL`.
 */

import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { _electron as electron } from "playwright-core";
import { describe, expect, it } from "vitest";

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const ELECTRON = join(ROOT, "node_modules", "electron", "dist", "Electron.app", "Contents", "MacOS", "Electron");
const MAIN = join(ROOT, "dist", "app", "main.cjs");

const blocked =
  process.platform !== "darwin"
    ? "BLOCKED_EXTERNAL: the launch smoke drives the macOS Electron binary; run it on a macOS host"
    : !existsSync(ELECTRON)
      ? `BLOCKED_EXTERNAL: ${ELECTRON} is missing; install dependencies with electron's build script allowed`
      : !existsSync(MAIN)
        ? `BLOCKED_EXTERNAL: ${MAIN} is missing; run pnpm --filter @quansio/desktop build`
        : null;

describe.skipIf(blocked !== null)("desktop launch (real Electron)", () => {
  it("renders the bundled renderer from the app: scheme", async () => {
    const userData = mkdtempSync(join(tmpdir(), "quansio-desktop-launch-"));
    const app = await electron.launch({
      executablePath: ELECTRON,
      args: [MAIN, "--use-mock-keychain", `--user-data-dir=${userData}`],
      // Nothing listens here: the sign-in form must render without a server.
      env: { ...process.env, QUANSIO_SERVER_ORIGIN: "http://127.0.0.1:9" },
      timeout: 60_000,
    });
    try {
      const page = await app.firstWindow();
      expect(page.url()).toBe("app://desktop/index.html");
      const email = page.locator('input[type="email"]');
      await email.waitFor({ state: "visible", timeout: 30_000 });
      expect(await email.count()).toBe(1);
    } finally {
      await app.close();
      rmSync(userData, { recursive: true, force: true });
    }
  }, 120_000);
});

if (blocked !== null) {
  it.skip(blocked, () => undefined);
}
