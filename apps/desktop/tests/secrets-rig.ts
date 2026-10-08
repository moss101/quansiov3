/**
 * A rig for the password manager and the import (ADP-009): the real windows registry, the real guarded IPC, the real vault, filler,
 * importer and host composed by `composeSecrets`, with the platform parts replaced by what a person and a machine would supply:
 * the keychain-backed cipher (a real transformation, not the identity), the native questions (a queue of answers), the server (a
 * recorder) and the Safe Storage key (the mock keychain's, or a test's own).
 */

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";

import { IpcGuard, registerGuardedHandlers, type WindowHandler } from "../src/main/ipc-trust.js";
import type { MessageBox } from "../src/main/dialogs.js";
import { ImportError, deriveKey, type BrowserFamily, type ChromiumKeys } from "../src/main/secrets/chromium.js";
import { composeSecrets, type Composed, type SecretsServer } from "../src/main/secrets/compose.js";
import { loadLocalSecretsConfig, type LocalSecretsConfig } from "../src/main/secrets/config.js";
import type { PageWriter } from "../src/main/secrets/fill.js";
import { zero, type Secret } from "../src/main/secrets/secret.js";
import { parseShellConfig } from "../src/main/shell-config.js";
import type { SecretCipher } from "../src/main/token-store.js";
import { WindowManager, type ManagedWindow } from "../src/main/windows.js";
import { PRELOAD_ALLOWLIST, SECRET_CHANNELS, type PreloadChannel, type SecretChannel } from "../src/preload/bridge.js";

import { Clock, FakeDisplays, FakeFactory, ipcEvent } from "./shell-fakes.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
export const SECRETS_CONFIG: LocalSecretsConfig = loadLocalSecretsConfig(join(ROOT, "config", "local-secrets.yaml"));
const SHELL = parseShellConfig(readFileSync(join(ROOT, "config", "desktop-shell.yaml"), "utf8"));

export interface Answer {
  readonly ok: boolean;
  readonly result?: Record<string, unknown>;
  readonly error?: { code: string; message: string; details: Record<string, unknown> | null };
}

/** A reversible, non-identity stand-in for Electron's `safeStorage`. */
export function testCipher(available = true, key = 0x5a): SecretCipher {
  return {
    isEncryptionAvailable: () => available,
    encryptString: (text) => Buffer.from(Buffer.from(text, "utf8").map((byte) => byte ^ key)),
    decryptString: (bytes) => {
      const plain = Buffer.from(bytes.map((byte) => byte ^ key)).toString("utf8");
      JSON.parse(plain);
      return plain;
    },
  };
}

/** The Safe Storage key of a family, as the keychain would give it; remembers every buffer it handed out so a test can see them zeroed. */
export class RecordedKeys implements ChromiumKeys {
  readonly issued: Buffer[] = [];
  asked: string[] = [];
  deny = false;

  constructor(private readonly password = "mock_password") {}

  key(family: BrowserFamily): Promise<Buffer> {
    this.asked.push(family.id);
    if (this.deny) {
      return Promise.reject(new ImportError("keychain_denied", "the keychain did not give out the key"));
    }
    const key = deriveKey(this.password);
    this.issued.push(key);
    return Promise.resolve(key);
  }
}

export interface PageState {
  topOrigin: string | null;
  frameOrigin: string | null;
  revision: string;
}

export interface Rig {
  readonly windows: WindowManager;
  readonly main: ManagedWindow;
  readonly popout: ManagedWindow;
  readonly quick: ManagedWindow;
  readonly indicator: ManagedWindow;
  readonly composed: Composed;
  readonly clock: Clock;
  readonly asked: MessageBox[];
  /** The buttons the person will press, in order; an empty queue presses the default (Cancel). */
  readonly answers: number[];
  readonly invoked: { name: string; params: Record<string, unknown> }[];
  readonly page: PageState;
  readonly written: { username: string; password: string }[];
  readonly keys: RecordedKeys;
  readonly userData: string;
  readonly home: string;
  /** What the person does while a question is open (e.g. navigate the page). */
  during: (() => void) | null;
  call(channel: SecretChannel, window: ManagedWindow, payload?: unknown): Promise<Answer>;
  cleanup(): void;
}

export interface RigOptions {
  readonly config?: Partial<LocalSecretsConfig>;
  readonly home?: string;
  readonly cipher?: SecretCipher;
  readonly writer?: "absent" | "present";
  readonly keys?: RecordedKeys;
  /** What the server answers to `ImportBrowserSession`. */
  readonly stagedAnswer?: Record<string, unknown>;
}

export function makeRig(options: RigOptions = {}): Rig {
  const factory = new FakeFactory();
  const clock = new Clock();
  const windows = new WindowManager({ factory, platform: "darwin", preload: "/app/preload.cjs", config: SHELL, displays: new FakeDisplays(), log: () => undefined });
  const main = windows.openMain();
  const popout = windows.openPopout({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", tab: "conversation", sidebar: false });
  const quick = windows.quickEntry();
  const indicator = windows.indicator();
  if (!popout.ok || !quick || !indicator) {
    throw new Error("a window did not open");
  }
  for (const window of [main, popout.window, quick, indicator]) {
    windows.consumePending(window);
  }
  const userData = mkdtempSync(join(tmpdir(), "quansio-userdata-"));
  const home = options.home ?? mkdtempSync(join(tmpdir(), "quansio-nohome-"));
  const asked: MessageBox[] = [];
  const answers: number[] = [];
  const invoked: { name: string; params: Record<string, unknown> }[] = [];
  const page: PageState = { topOrigin: "https://example.com", frameOrigin: "https://example.com", revision: "rev-1" };
  const written: { username: string; password: string }[] = [];
  const keys = options.keys ?? new RecordedKeys();
  const state = { during: null as (() => void) | null };
  const server: SecretsServer = {
    invoke: (name, _commandId, params) => {
      invoked.push({ name, params });
      return Promise.resolve({
        result: options.stagedAnswer ?? {
          import_id: "sec_01J8Z3K6F1N8VQ2X5W9Y0IMPRT",
          digest: typeof params["digest"] === "string" ? params["digest"] : "",
          cookie_count: Array.isArray(params["cookies"]) ? params["cookies"].length : 0,
          domains: [],
          more_domains: 0,
          expires_at: "2026-10-07T12:15:00Z",
        },
      });
    },
    browserSession: () =>
      Promise.resolve({
        current_url: page.topOrigin === null ? null : `${page.topOrigin}/signin`,
        tabs: [{ document_revision: page.revision }],
      }),
  };
  const writer: PageWriter = {
    fill: (_view, username: Secret, password: Secret) => {
      written.push({ username: username.expose(), password: password.expose() });
      return Promise.resolve({ filled: true });
    },
  };
  const composed = composeSecrets({
    config: { ...SECRETS_CONFIG, ...options.config },
    userData,
    home,
    cipher: options.cipher ?? testCipher(),
    ask: (box) => {
      asked.push(box);
      state.during?.();
      return Promise.resolve(answers.shift() ?? box.defaultId);
    },
    server,
    workspaceId: () => "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    newCommandId: () => "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    keys,
    writer: options.writer === "present" ? writer : null,
    now: () => clock.now,
  });
  const secretHandlers = composed.host.handlers();
  const handlers = Object.fromEntries(
    PRELOAD_ALLOWLIST.map((channel) => [channel, (SECRET_CHANNELS as readonly string[]).includes(channel) ? secretHandlers[channel as SecretChannel] : ((): Promise<unknown> => Promise.resolve({ ok: true, result: {} }))]),
  ) as Record<PreloadChannel, WindowHandler>;
  const registered = new Map<string, (event: unknown, payload: unknown) => unknown>();
  registerGuardedHandlers({ handle: (channel, listener) => registered.set(channel, listener) }, new IpcGuard(windows, () => undefined), handlers);
  return {
    windows,
    main,
    popout: popout.window,
    quick,
    indicator,
    composed,
    clock,
    asked,
    answers,
    invoked,
    page,
    written,
    keys,
    userData,
    home,
    get during() {
      return state.during;
    },
    set during(value) {
      state.during = value;
    },
    call: async (channel, window, payload = {}) => (await Promise.resolve(registered.get(channel)?.(ipcEvent(window.handle.webContentsId), payload))) as Answer,
    cleanup: () => {
      rmSync(userData, { recursive: true, force: true });
      if (options.home === undefined) {
        rmSync(home, { recursive: true, force: true });
      }
      for (const key of keys.issued) {
        zero(key);
      }
    },
  };
}
