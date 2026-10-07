/**
 * A browser profile made by a **real Google Chrome** (ADP-009): launched headless on a temporary profile with the mock keychain
 * (`--use-mock-keychain`, whose Safe Storage password is `mock_password`), given cookies through its own DevTools protocol, and closed
 * so it flushes them. What the import tests read is the cookie database Chrome itself wrote and encrypted, not a database a test made.
 *
 * The saved-login database is not made by Chrome here (Chrome's password store has no protocol call for adding a login); a test builds
 * one with Chromium's `logins` table and the same encryption, which the cookie database proves the reader handles.
 */

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const CANDIDATES = ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/google-chrome", "/usr/bin/chromium"];

export function chromePath(): string | null {
  return CANDIDATES.find((path) => existsSync(path)) ?? null;
}

/** The password Chrome's mock keychain answers with. */
export const MOCK_KEYCHAIN_PASSWORD = "mock_password";

export interface FixtureCookie {
  readonly name: string;
  readonly value: string;
  readonly domain: string;
  readonly path: string;
  readonly secure: boolean;
  readonly httpOnly: boolean;
  readonly sameSite: "Lax" | "Strict" | "None";
  readonly expires: number;
}

export interface Fixture {
  /** A home folder whose `Library/Application Support/Google/Chrome` is the profile root Chrome wrote. */
  readonly home: string;
  readonly cleanup: () => void;
}

async function until<T>(work: () => Promise<T | null | undefined | false>, ms = 25_000): Promise<T> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const value = await work();
      if (value) {
        return value;
      }
    } catch {
      // not yet
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error("timed out");
}

/** Launch Chrome, set `cookies`, close it, and return the home folder holding the profile. `null` when there is no Chrome. */
export async function makeChromeHome(cookies: readonly FixtureCookie[]): Promise<Fixture | null> {
  const chrome = chromePath();
  if (chrome === null) {
    return null;
  }
  const home = mkdtempSync(join(tmpdir(), "quansio-home-"));
  const root = join(home, "Library", "Application Support", "Google", "Chrome");
  mkdirSync(root, { recursive: true });
  const port = 9600 + Math.floor(Math.random() * 300);
  const child = spawn(
    chrome,
    ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--use-mock-keychain", "--password-store=basic", `--remote-debugging-port=${String(port)}`, `--user-data-dir=${root}`, "about:blank"],
    { stdio: "ignore" },
  );
  const exited = new Promise<void>((resolve) => child.on("exit", () => { resolve(); }));
  try {
    const version = await until(async () => {
      const response = await fetch(`http://127.0.0.1:${String(port)}/json/version`);
      return (await response.json()) as { webSocketDebuggerUrl: string };
    });
    const socket = new WebSocket(version.webSocketDebuggerUrl);
    await new Promise<void>((resolve, reject) => {
      socket.onopen = () => { resolve(); };
      socket.onerror = () => { reject(new Error("the DevTools socket did not open")); };
    });
    let next = 0;
    const pending = new Map<number, (message: Record<string, unknown>) => void>();
    socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as Record<string, unknown>;
      const id = message["id"];
      if (typeof id === "number") {
        pending.get(id)?.(message);
        pending.delete(id);
      }
    };
    const send = (method: string, params: Record<string, unknown> = {}): Promise<Record<string, unknown>> =>
      new Promise((resolve) => {
        next += 1;
        pending.set(next, resolve);
        socket.send(JSON.stringify({ id: next, method, params }));
      });
    const set = await send("Storage.setCookies", { cookies: [...cookies] });
    if (set["error"] !== undefined) {
      throw new Error("Chrome refused the fixture cookies");
    }
    // Give Chrome its flush interval, then close it so the database is complete.
    await new Promise((resolve) => setTimeout(resolve, 1800));
    await send("Browser.close");
    await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 10_000))]);
  } catch (error) {
    child.kill();
    rmSync(home, { recursive: true, force: true });
    throw error;
  }
  return { home, cleanup: () => { rmSync(home, { recursive: true, force: true }); } };
}
