/**
 * QA-008 harness: the real apps, driven as a person drives them, against the real server.
 *
 * - `Stack`: a scratch database seeded by `quansio-dev-seed` (an owner, an approver and a viewer; a
 *   policy that makes writes ask; a live run on an execution target) and `quansio-server` serving the
 *   built web app, restartable on the same port for degraded/offline states. Sign-in is the real
 *   magic-link flow: the token is read from the server's console, where a deployment without mail
 *   prints it.
 * - `launchDesktop`: the real Electron binary running the built desktop app through Playwright's
 *   Electron driver, with a mock keychain and a throwaway profile.
 * - `launchChrome`: the installed Google Chrome through `playwright-core` (`channel: "chrome"`).
 * - `auditAccessibility` / `walkKeyboard`: accessibility checks over the browser's own accessibility
 *   tree (unnamed controls, document language, headings) and a Tab walk (every control reachable,
 *   focus always visible).
 *
 * Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN), `QUANSIO_TEST_SERVER_BIN`,
 * `QUANSIO_TEST_SEED_BIN`. Absent → `BLOCKED_EXTERNAL`.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { generateKeyPairSync } from "node:crypto";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { _electron as electron, chromium, type Browser, type ElectronApplication, type Page } from "playwright-core";

export const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(ROOT, "target", "debug", "quansio-dev-seed");
const DESKTOP = join(ROOT, "apps", "desktop");
const ELECTRON = join(DESKTOP, "node_modules", "electron", "dist", "Electron.app", "Contents", "MacOS", "Electron");
const DESKTOP_MAIN = join(DESKTOP, "dist", "app", "main.cjs");
const WEB_BUNDLE = join(ROOT, "apps", "web", "dist", "web");
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

export const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0QA8AA";
export const OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA8AA";
export const APPROVER = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA8AP";
export const VIEWER = "usr_01J8Z3K6F1N8VQ2X5W9Y0QA8VW";
export const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA8AA";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA8AA";

/** The address the seed stores for a seeded user (lower case, as sign-in normalizes it). */
export function emailOf(user: string): string {
  return `${user}@example.com`.toLowerCase();
}

export function serverBlocked(): string | null {
  if (!ADMIN_URL) {
    return "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running";
  }
  if (!existsSync(SERVER_BIN) || !existsSync(SEED_BIN)) {
    return `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`;
  }
  return null;
}

export function desktopBlocked(): string | null {
  if (process.platform !== "darwin") {
    return "BLOCKED_EXTERNAL: the desktop journeys drive the macOS build; Windows 11 needs its own host (QUANSIO_TEST_WINDOWS_DESKTOP=1)";
  }
  if (!existsSync(ELECTRON)) {
    return `BLOCKED_EXTERNAL: ${ELECTRON} is missing; install dependencies with electron's build script allowed`;
  }
  if (!existsSync(DESKTOP_MAIN)) {
    return `BLOCKED_EXTERNAL: ${DESKTOP_MAIN} is missing; run pnpm --filter @quansio/desktop build`;
  }
  return serverBlocked();
}

export function chromeBlocked(): string | null {
  if (!existsSync(CHROME)) {
    return `BLOCKED_EXTERNAL: Google Chrome is not installed at ${CHROME}`;
  }
  if (!existsSync(join(WEB_BUNDLE, "index.html"))) {
    return `BLOCKED_EXTERNAL: ${WEB_BUNDLE} is missing; run pnpm --filter @quansio/web build`;
  }
  return serverBlocked();
}

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
          reject(new Error("no free port"));
        }
      });
    });
  });
}

export async function waitFor<T>(probe: () => T | undefined | null | false | Promise<T | undefined | null | false>, what: string, timeoutMs = 30_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

/** A scratch database and the server over it. */
export class Stack {
  readonly database: string;
  readonly databaseUrl: string;
  readonly port: number;
  readonly origin: string;
  readonly log: string[] = [];
  /** A stable session-signing key, as a deployment configures one, so sessions survive a restart. */
  readonly #signingKey = generateKeyPairSync("ec", { namedCurve: "P-256" }).privateKey.export({ type: "pkcs8", format: "der" }).toString("base64");
  #child: ChildProcess | null = null;

  private constructor(database: string, databaseUrl: string, port: number) {
    this.database = database;
    this.databaseUrl = databaseUrl;
    this.port = port;
    this.origin = `http://127.0.0.1:${String(port)}`;
  }

  static async start(label: string): Promise<Stack> {
    const database = `quansio_qa008_${label}_${String(process.pid)}`;
    const output = execFileSync(
      SEED_BIN,
      [
        "create", "--database", database, "--tenant", TENANT, "--user", OWNER, "--workspace", WORKSPACE, "--node", NODE,
        "--ask-writes", "--live",
        "--member", `${APPROVER}:member:approver`,
        "--member", `${VIEWER}:member:viewer`,
      ],
      { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL }, encoding: "utf8" },
    );
    const line = output.split("\n").find((candidate) => candidate.startsWith("database_url="));
    if (!line) {
      throw new Error(`quansio-dev-seed printed no database_url: ${output}`);
    }
    const stack = new Stack(database, line.slice("database_url=".length), await freePort());
    await stack.up();
    return stack;
  }

  get running(): boolean {
    return this.#child !== null && this.#child.exitCode === null;
  }

  /** Start (or restart) the server on the stack's port. */
  async up(): Promise<void> {
    const env: Record<string, string | undefined> = {
      ...process.env,
      QUANSIO_DATABASE_URL: this.databaseUrl,
      QUANSIO_BIND: `127.0.0.1:${String(this.port)}`,
      QUANSIO_APPROVAL_SIGNING_KEY: "qa008-signing-key",
      QUANSIO_AUTH_SIGNING_KEY_PKCS8_B64: this.#signingKey,
      QUANSIO_WEB_ROOT: WEB_BUNDLE,
      QUANSIO_OBJECT_STORE: "1",
      QUANSIO_SCHEDULER_INTERVAL_MS: "250",
    };
    delete env["QUANSIO_TRUST_SCOPE_HEADERS"];
    delete env["QUANSIO_INTELLIGENCE_ENDPOINT"];
    const child = spawn(SERVER_BIN, [], { env, stdio: ["ignore", "ignore", "pipe"] });
    child.stderr.on("data", (chunk: Buffer) => this.log.push(chunk.toString()));
    this.#child = child;
    await waitFor(async () => {
      if (child.exitCode !== null) {
        throw new Error(`quansio-server exited with ${String(child.exitCode)}: ${this.log.join("").slice(-2000)}`);
      }
      try {
        return (await fetch(`${this.origin}/v1/health`)).ok;
      } catch {
        return false;
      }
    }, "quansio-server to become healthy", 90_000);
  }

  /** Stop the server (the database and every session stay). */
  async down(): Promise<void> {
    const child = this.#child;
    if (!child || child.exitCode !== null) {
      return;
    }
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill("SIGTERM");
    await exited;
  }

  async drop(): Promise<void> {
    await this.down();
    try {
      execFileSync(SEED_BIN, ["drop", "--database", this.database], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
    } catch {
      /* named by pid; a leftover is harmless */
    }
  }

  /** The console mark before a magic link is requested. */
  mark(): number {
    return this.log.join("").length;
  }

  /** The magic-link token printed for `email` after `mark`. */
  async magicLinkToken(email: string, mark: number): Promise<string> {
    return waitFor(() => {
      const text = this.log.join("").slice(mark);
      const found = new RegExp(`magic-link for ${email.replace(/[.+]/g, "\\$&")} .*token=(qml_[A-Za-z0-9_-]+)`, "i").exec(text);
      return found?.[1];
    }, `a magic link for ${email} on the console`);
  }

  /** A bearer session through the API (for arranging state a journey then observes). */
  async session(email: string): Promise<{ access_token: string; tenant_id: string; workspace_id: string; user_id: string }> {
    const mark = this.mark();
    await this.post("/v1/auth/magic-link/start", { email, display_name: "qa008" });
    const token = await this.magicLinkToken(email, mark);
    return (await this.post("/v1/auth/magic-link/complete", { token })) as { access_token: string; tenant_id: string; workspace_id: string; user_id: string };
  }

  async post(path: string, body: unknown, bearer?: string): Promise<unknown> {
    const response = await fetch(`${this.origin}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json", ...(bearer ? { authorization: `Bearer ${bearer}` } : {}) },
      body: JSON.stringify(body),
    });
    const decoded: unknown = await response.json();
    if (!response.ok) {
      throw new Error(`${path}: ${String(response.status)} ${JSON.stringify(decoded)}`);
    }
    return decoded;
  }

  async command(bearer: string, name: string, params: Record<string, unknown>): Promise<Record<string, unknown>> {
    const id = `cmd_01J8Z3K6F1N8VQ2X5W9Y0${Math.random().toString(36).slice(2, 7).toUpperCase().replace(/[ILOU]/g, "X").padEnd(5, "X")}`;
    const envelope = (await this.post(`/v1/commands/${name}`, { command_id: id, params }, bearer)) as { result: Record<string, unknown> };
    return envelope.result;
  }

  async get(bearer: string, path: string): Promise<Record<string, unknown>> {
    const response = await fetch(`${this.origin}${path}`, { headers: { authorization: `Bearer ${bearer}` } });
    return (await response.json()) as Record<string, unknown>;
  }
}

export interface Desktop {
  readonly app: ElectronApplication;
  readonly page: Page;
  close(): Promise<void>;
}

/** The real desktop binary against `origin`, with a mock keychain and a throwaway profile. */
export async function launchDesktop(origin: string): Promise<Desktop> {
  const userData = mkdtempSync(join(tmpdir(), "qa008-desktop-"));
  const app = await electron.launch({
    executablePath: ELECTRON,
    args: [DESKTOP_MAIN, "--use-mock-keychain", `--user-data-dir=${userData}`],
    env: { ...process.env, QUANSIO_SERVER_ORIGIN: origin },
    timeout: 60_000,
  });
  const page = await app.firstWindow();
  await page.waitForLoadState("domcontentloaded");
  return {
    app,
    page,
    close: async () => {
      await app.close();
      rmSync(userData, { recursive: true, force: true });
    },
  };
}

/** Sign in through the desktop's own magic-link screens. */
export async function signInDesktop(page: Page, stack: Stack, email: string, onboarding: "returning" | "new" = "returning"): Promise<void> {
  await page.locator('input[type="email"]').fill(email);
  const mark = stack.mark();
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  const token = await stack.magicLinkToken(email, mark);
  await page.getByRole("textbox", { name: "Paste the sign-in token" }).fill(token);
  await page.getByRole("button", { name: "Continue" }).click();
  if (onboarding === "returning") {
    await page.getByRole("navigation", { name: "Surfaces" }).waitFor({ state: "visible", timeout: 30_000 });
  }
}

export async function launchChrome(): Promise<Browser> {
  return chromium.launch({ channel: "chrome", headless: true, timeout: 60_000 });
}

// ============================================================================ accessibility

const INTERACTIVE = new Set([
  "button", "link", "textbox", "searchbox", "checkbox", "radio", "combobox", "listbox", "menuitem",
  "tab", "switch", "slider", "spinbutton", "option", "treeitem",
]);

export interface AccessibilityReport {
  readonly violations: readonly string[];
  readonly controls: number;
}

interface AxNode {
  readonly role?: { value?: string };
  readonly name?: { value?: string };
  readonly ignored?: boolean;
  readonly properties?: readonly { name: string; value: { value?: unknown } }[];
}

/** Accessibility blockers on the current page, from the browser's own accessibility tree. */
export async function auditAccessibility(page: Page): Promise<AccessibilityReport> {
  const cdp = await page.context().newCDPSession(page);
  const { nodes } = (await cdp.send("Accessibility.getFullAXTree")) as { nodes: AxNode[] };
  await cdp.detach();
  const violations: string[] = [];
  let controls = 0;
  for (const node of nodes) {
    if (node.ignored) {
      continue;
    }
    const role = node.role?.value ?? "";
    const name = (node.name?.value ?? "").trim();
    if (INTERACTIVE.has(role)) {
      controls += 1;
      if (!name) {
        violations.push(`a ${role} has no accessible name`);
      }
    }
    if ((role === "image" || role === "img") && !name) {
      violations.push("an image has no text alternative");
    }
  }
  const document = await page.evaluate(() => ({
    lang: window.document.documentElement.lang,
    title: window.document.title,
    headings: window.document.querySelectorAll("h1").length,
    positiveTabindex: Array.from(window.document.querySelectorAll("[tabindex]")).filter((element) => Number(element.getAttribute("tabindex")) > 0).length,
  }));
  if (!document.lang) {
    violations.push("the document declares no language");
  }
  if (!document.title.trim()) {
    violations.push("the document has no title");
  }
  if (document.headings === 0) {
    violations.push("the page has no top-level heading");
  }
  if (document.positiveTabindex > 0) {
    violations.push(`${String(document.positiveTabindex)} element(s) override the tab order with a positive tabindex`);
  }
  return { violations, controls };
}

export interface KeyboardReport {
  readonly reached: readonly string[];
  readonly unreachable: readonly string[];
  readonly invisibleFocus: readonly string[];
}

/** Tab through the page: every enabled, visible control must take focus, and focus must show. */
export async function walkKeyboard(page: Page, maxSteps = 200): Promise<KeyboardReport> {
  const expected = await page.evaluate(() => {
    const selector = "a[href], button, input, select, textarea, [tabindex]";
    return Array.from(document.querySelectorAll<HTMLElement>(selector))
      .filter((element) => {
        const disabled = (element as HTMLButtonElement).disabled;
        const hidden = element.getAttribute("tabindex") === "-1" || element.offsetParent === null || (element as HTMLInputElement).type === "hidden";
        return !disabled && !hidden;
      })
      .map((element, index) => {
        element.dataset["qa008Key"] = String(index);
        return `${element.tagName.toLowerCase()}:${(element.getAttribute("aria-label") ?? (element.textContent || element.getAttribute("placeholder")) ?? "").trim().slice(0, 40)}#${String(index)}`;
      });
  });
  await page.evaluate(() => {
    (document.activeElement as HTMLElement | null)?.blur();
    window.scrollTo(0, 0);
    // A control's resting shadow (a raised button) is not a focus indicator: remember it, so only
    // a shadow that appears or changes on focus counts as showing focus.
    for (const element of document.querySelectorAll<HTMLElement>("[data-qa008-key]")) {
      element.dataset["qa008Shadow"] = window.getComputedStyle(element).boxShadow;
    }
  });
  await page.locator("body").focus();
  const reached = new Set<string>();
  const invisible = new Set<string>();
  let first: string | null = null;
  for (let step = 0; step < maxSteps; step += 1) {
    await page.keyboard.press("Tab");
    const focused = await page.evaluate(() => {
      const element = document.activeElement as HTMLElement | null;
      if (!element || element === document.body) {
        return null;
      }
      const style = window.getComputedStyle(element);
      const outline = style.outlineStyle !== "none" && style.outlineWidth !== "0px";
      const ring = style.boxShadow !== "none" && style.boxShadow !== (element.dataset["qa008Shadow"] ?? "none");
      const visible = outline || ring;
      return { key: element.dataset["qa008Key"] ?? null, visible };
    });
    if (!focused || focused.key === null) {
      continue;
    }
    if (focused.key === first) {
      break;
    }
    first ??= focused.key;
    reached.add(focused.key);
    if (!focused.visible) {
      invisible.add(focused.key);
    }
  }
  const byKey = (key: string): string => expected.find((entry) => entry.endsWith(`#${key}`)) ?? key;
  return {
    reached: [...reached].map(byKey),
    unreachable: expected.filter((entry) => !reached.has(entry.split("#").pop() ?? "")),
    invisibleFocus: [...invisible].map(byKey),
  };
}
