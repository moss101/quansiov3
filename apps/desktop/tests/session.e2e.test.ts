/**
 * Account E2E (APP-002 regressions found by QA-008): the real Electron app against the real
 * server returns a person to sign-in when their session ends — by the Sign out button, or because
 * an administrator revoked it while the app was open — and lets a workspace administrator change
 * the workspace's and their own settings, showing the effective result and what the change sets in
 * motion (the desktop had no settings surface at all).
 *
 * Before the fix the shell stayed on screen in both cases: the renderer only listened for the
 * session while signing in, and a revoked token was surfaced as a raw `AUTH_INVALID_TOKEN` on the
 * next action (with the event stream still open until CORE-009's re-check).
 *
 * Needs the Electron binary, the built app, `QUANSIO_TEST_POSTGRES_URL` and the server binaries;
 * without them it skips with `BLOCKED_EXTERNAL`.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { generateKeyPairSync } from "node:crypto";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { _electron as electron, type ElectronApplication, type Page } from "playwright-core";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const WORKSPACE_ROOT = join(ROOT, "..", "..");
const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");
const ELECTRON = join(ROOT, "node_modules", "electron", "dist", "Electron.app", "Contents", "MacOS", "Electron");
const MAIN = join(ROOT, "dist", "app", "main.cjs");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0SESEE";
const OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0SESE1";
const MEMBER = "usr_01J8Z3K6F1N8VQ2X5W9Y0SESE2";
const VIEWER = "usr_01J8Z3K6F1N8VQ2X5W9Y0SESE3";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0SESEE";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0SESEE";

const blocked =
  process.platform !== "darwin"
    ? "BLOCKED_EXTERNAL: the session E2E drives the macOS Electron binary"
    : !existsSync(ELECTRON) || !existsSync(MAIN)
      ? `BLOCKED_EXTERNAL: ${ELECTRON} and ${MAIN} are needed (install with electron's build script allowed; build the app)`
      : !ADMIN_URL
        ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
        : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
          ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed`
          : null;

const email = (user: string): string => `${user}@example.com`.toLowerCase();

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close(() => {
        if (port) {
          resolve(port);
        } else {
          reject(new Error("no port"));
        }
      });
    });
  });
}

async function until(probe: () => Promise<boolean>, what: string, ms = 30_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await probe())) {
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

describe.skipIf(blocked !== null)("desktop account (real Electron, real server)", () => {
  let database: string;
  let child: ChildProcess;
  let origin: string;
  const log: string[] = [];

  const tokenFor = async (address: string, mark: number): Promise<string> => {
    let token = "";
    await until(() => {
      const found = new RegExp(`magic-link for ${address.replace(/[.]/g, "\\.")} .*token=(qml_[A-Za-z0-9_-]+)`).exec(log.join("").slice(mark));
      token = found?.[1] ?? "";
      return Promise.resolve(token !== "");
    }, `a magic link for ${address}`);
    return token;
  };

  const signIn = async (page: Page, address: string): Promise<void> => {
    await page.locator('input[type="email"]').fill(address);
    const mark = log.join("").length;
    await page.getByRole("button", { name: "Email me a sign-in link" }).click();
    await page.getByRole("textbox", { name: "Paste the sign-in token" }).fill(await tokenFor(address, mark));
    await page.getByRole("button", { name: "Continue" }).click();
    await page.getByRole("navigation", { name: "Surfaces" }).waitFor({ state: "visible", timeout: 30_000 });
  };

  const launch = async (): Promise<{ app: ElectronApplication; page: Page; done: () => Promise<void> }> => {
    const userData = mkdtempSync(join(tmpdir(), "quansio-desktop-session-"));
    const app = await electron.launch({
      executablePath: ELECTRON,
      args: [MAIN, "--use-mock-keychain", `--user-data-dir=${userData}`],
      env: { ...process.env, QUANSIO_SERVER_ORIGIN: origin },
      timeout: 60_000,
    });
    const page = await app.firstWindow();
    return { app, page, done: async () => { await app.close(); rmSync(userData, { recursive: true, force: true }); } };
  };

  beforeAll(async () => {
    database = `quansio_desktop_session_${String(process.pid)}`;
    const output = execFileSync(
      SEED_BIN,
      ["create", "--database", database, "--tenant", TENANT, "--user", OWNER, "--workspace", WORKSPACE, "--node", NODE, "--member", `${MEMBER}:member:editor`, "--member", `${VIEWER}:member:viewer`],
      { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL }, encoding: "utf8" },
    );
    const url = output.split("\n").find((line) => line.startsWith("database_url="))?.slice("database_url=".length) ?? "";
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    const env: Record<string, string | undefined> = {
      ...process.env,
      QUANSIO_DATABASE_URL: url,
      QUANSIO_BIND: `127.0.0.1:${String(port)}`,
      QUANSIO_AUTH_SIGNING_KEY_PKCS8_B64: generateKeyPairSync("ec", { namedCurve: "P-256" }).privateKey.export({ type: "pkcs8", format: "der" }).toString("base64"),
    };
    delete env["QUANSIO_TRUST_SCOPE_HEADERS"];
    child = spawn(SERVER_BIN, [], { env, stdio: ["ignore", "ignore", "pipe"] });
    child.stderr?.on("data", (chunk: Buffer) => log.push(chunk.toString()));
    await until(async () => {
      try {
        return (await fetch(`${origin}/v1/health`)).ok;
      } catch {
        return false;
      }
    }, "the server", 90_000);
  }, 180_000);

  afterAll(() => {
    child.kill("SIGTERM");
    try {
      execFileSync(SEED_BIN, ["drop", "--database", database], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
    } catch {
      /* named by pid */
    }
  });

  it("returns to sign-in when the person signs out", async () => {
    const desktop = await launch();
    try {
      await signIn(desktop.page, email(OWNER));
      await desktop.page.getByRole("button", { name: "Sign out" }).click();
      await desktop.page.getByRole("heading", { name: "Sign in" }).waitFor({ timeout: 15_000 });
      expect(await desktop.page.getByRole("navigation", { name: "Surfaces" }).count()).toBe(0);
    } finally {
      await desktop.done();
    }
  }, 120_000);

  it("returns to sign-in when an administrator revokes the session", async () => {
    const desktop = await launch();
    try {
      await signIn(desktop.page, email(MEMBER));
      // The owner revokes the member's sessions through the API while the app is open.
      const mark = log.join("").length;
      await fetch(`${origin}/v1/auth/magic-link/start`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ email: email(OWNER) }) });
      const completed = (await (await fetch(`${origin}/v1/auth/magic-link/complete`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ token: await tokenFor(email(OWNER), mark) }) })).json()) as { access_token: string };
      const revoked = await fetch(`${origin}/v1/commands/RevokeUserSessions`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${completed.access_token}` },
        body: JSON.stringify({ command_id: "cmd_01J8Z3K6F1N8VQ2X5W9Y0SESE9", params: { user_id: MEMBER } }),
      });
      expect(revoked.ok).toBe(true);
      await desktop.page.getByRole("heading", { name: "Sign in" }).waitFor({ timeout: 20_000 });
    } finally {
      await desktop.done();
    }
  }, 120_000);
  it("changes workspace and personal settings and shows the effective result", async () => {
    const desktop = await launch();
    try {
      const page = desktop.page;
      await signIn(page, email(OWNER));
      await page.getByRole("button", { name: "Settings" }).click();
      const settings = page.getByRole("region", { name: "Settings" });
      await settings.getByLabel("Where work runs").selectOption("local_capsule");
      await settings.getByLabel("Keep data for (days)").fill("30");
      await settings.getByRole("button", { name: "Save workspace settings" }).click();
      const status = settings.getByRole("status");
      await until(async () => (await status.textContent())?.includes("re-provisioned") ?? false, "the restart the target change causes");
      await until(async () => (await settings.getByText("local_capsule").count()) > 0, "the effective execution target");
      await expect(settings.locator("dt", { hasText: "retention_days" }).locator("+ dd").textContent()).resolves.toBe("30");
      await settings.getByLabel("Theme").selectOption("dark");
      await settings.getByRole("button", { name: "Save my settings" }).click();
      await until(async () => (await settings.locator("dt", { hasText: "theme" }).locator("+ dd").textContent()) === "dark", "the personal theme");
      // A security control only gets stricter: telemetry stays off whatever the workspace asks.
      await settings.getByLabel("Send diagnostic telemetry").check();
      await settings.getByRole("button", { name: "Save workspace settings" }).click();
      await until(async () => (await status.textContent())?.startsWith("Saved.") ?? false, "the save");
      await expect(settings.locator("dt", { hasText: "telemetry" }).locator("+ dd").textContent()).resolves.toBe("false");
    } finally {
      await desktop.done();
    }
  }, 120_000);

  it("shows a viewer the workspace settings without letting them change them", async () => {
    const desktop = await launch();
    try {
      const page = desktop.page;
      await signIn(page, email(VIEWER));
      await page.getByRole("button", { name: "Settings" }).click();
      const settings = page.getByRole("region", { name: "Settings" });
      await settings.getByText("Only a workspace administrator can change these.").waitFor();
      await expect(settings.getByRole("button", { name: "Save workspace settings" }).isDisabled()).resolves.toBe(true);
      await expect(settings.getByRole("button", { name: "Save my settings" }).isEnabled()).resolves.toBe(true);
    } finally {
      await desktop.done();
    }
  }, 120_000);
  it("names every surface for a person and every entry opens a page (APP-003)", async () => {
    const desktop = await launch();
    try {
      const page = desktop.page;
      await signIn(page, email(OWNER));
      const nav = page.getByRole("navigation", { name: "Surfaces" });
      const entries = (await nav.getByRole("listitem").allTextContents()).map((text) => text.trim());
      expect(entries).toHaveLength(15);
      // No surface is shown by its internal id (lower-case, hyphenated).
      expect(entries.filter((text) => /^[a-z]+(-[a-z]+)+$|^[a-z]+$/.test(text))).toEqual([]);
      const buttons = nav.getByRole("button");
      const count = await buttons.count();
      expect(count).toBe(14);
      for (let index = 0; index < count; index += 1) {
        await buttons.nth(index).click();
        await until(async () => (await page.getByRole("region").count()) > 0, `the page of ${String(index)}`);
        await expect(buttons.nth(index).getAttribute("aria-current")).resolves.toBe("page");
      }
    } finally {
      await desktop.done();
    }
  }, 120_000);
});

if (blocked !== null) {
  it.skip(blocked, () => undefined);
}
