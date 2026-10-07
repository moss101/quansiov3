/**
 * Fixtures for the local-file tests (ADP-005): real temporary directories and files, the shipped configuration, and a
 * clock that moves only when the test moves it. Nothing about the filesystem is simulated.
 */

import { chmodSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach } from "vitest";

import { loadLocalFilesConfig, type LocalFilesConfig } from "../src/main/local-files/config.js";
import type { Scheduler } from "../src/main/local-files/grants.js";
import { Clock } from "./shell-fakes.js";

export const CONFIG_PATH = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "config", "local-files.yaml");
export const CONFIG: LocalFilesConfig = loadLocalFilesConfig(CONFIG_PATH);

/** Scheduler over the manual clock. */
export function schedulerOf(clock: Clock): Scheduler {
  return { set: clock.setTimer, clear: clock.clearTimer };
}

const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0)) {
    try {
      chmodSync(directory, 0o700);
    } catch {
      // Gone.
    }
    rmSync(directory, { recursive: true, force: true });
  }
});

/** A fresh real directory (its `realpath`: on macOS the temporary directory is itself behind a link), removed after the test. */
export function scratch(): string {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), "quansio-files-")));
  directories.push(directory);
  return directory;
}

/** Write a file (creating its folders) and return its path. */
export function put(directory: string, relative: string, content: string | Buffer): string {
  const path = join(directory, ...relative.split("/"));
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
  return path;
}

export function link(target: string, at: string): string {
  mkdirSync(dirname(at), { recursive: true });
  symlinkSync(target, at);
  return at;
}

/** Wait until `check` returns something other than `undefined`, polling real time. */
export async function eventually<T>(check: () => T | undefined, what: string, timeoutMs = 10_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = check();
    if (value !== undefined) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}
