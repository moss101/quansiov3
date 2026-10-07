/**
 * Composing the password manager and the import against the shell (ADP-009, DOMAIN.md §13.9).
 *
 * Everything Electron-shaped is passed in, so `app.ts` supplies the platform and a test supplies a person: the encrypted file's cipher,
 * the native question, the server call. What this file adds on top of the pieces is the production reading of a live page for the
 * fill path (the browser-session projection) and the decoding of the server's answer to a staged bundle.
 */

import type { MessageBox } from "../dialogs.js";
import type { SecretCipher } from "../token-store.js";
import type { ManagedWindow } from "../windows.js";
import { KeychainKeys, type ChromiumKeys } from "./chromium.js";
import type { LocalSecretsConfig } from "./config.js";
import { crossSiteDialog, importDialog, proceeds } from "./dialogs.js";
import { PasswordFiller, type CrossSiteConfirmation, type PageProbe, type PageView, type PageWriter } from "./fill.js";
import { SecretsHost } from "./host.js";
import { BrowserImportService, type ImportConfirmation, type StageBundle, type StagedBundle } from "./import.js";
import { originOf } from "./origins.js";
import { PasswordVault, VAULT_FILE } from "./vault.js";
import { join } from "node:path";

/** The slice of the server client the composition uses. */
export interface SecretsServer {
  invoke(name: string, commandId: string, params: Record<string, unknown>): Promise<{ readonly result: unknown }>;
  browserSession(sessionId: string): Promise<unknown>;
}

export interface SecretsOptions {
  readonly config: LocalSecretsConfig;
  readonly userData: string;
  readonly home: string;
  readonly cipher: SecretCipher;
  /** Asks the person with a native message box and answers the button's index. */
  readonly ask: (box: MessageBox) => Promise<number>;
  readonly server: SecretsServer;
  readonly workspaceId: () => string;
  readonly newCommandId: () => string;
  /** The Safe Storage keys; the keychain by default, a test's own otherwise. */
  readonly keys?: ChromiumKeys;
  /** The takeover input path. Absent in this build: a fill answers `ADAPTER_UNAVAILABLE`. */
  readonly writer?: PageWriter | null;
  readonly now?: () => number;
}

/**
 * The page of a live session, as the server's last browser-session projection says it is.
 *
 * The projection names the session's current URL and each tab's document revision. It does not say which frame a login field is in, so
 * the frame origin is taken to be the top frame's: a fill into a field in a cross-origin frame is not something this reading can
 * detect, which is why the production writer, when it exists, must report its own frame origin and the filler compares it.
 */
export class ServerPageProbe implements PageProbe {
  readonly #server: SecretsServer;

  constructor(server: SecretsServer) {
    this.#server = server;
  }

  async view(sessionId: string): Promise<PageView | null> {
    const projection: unknown = await this.#server.browserSession(sessionId);
    const record = projection !== null && typeof projection === "object" ? (projection as Record<string, unknown>) : {};
    const url = typeof record["current_url"] === "string" ? record["current_url"] : null;
    const origin = url === null ? null : originOf(url);
    const tabs = Array.isArray(record["tabs"]) ? (record["tabs"] as unknown[]) : [];
    const first = tabs[0] !== null && typeof tabs[0] === "object" ? (tabs[0] as Record<string, unknown>) : {};
    const revision = typeof first["document_revision"] === "string" ? first["document_revision"] : "";
    return { sessionId, topOrigin: origin, frameOrigin: origin, documentRevision: revision };
  }
}

/** Decode the server's answer to `ImportBrowserSession`: counts, domains and a time, never a value. */
export function decodeStaged(result: unknown): StagedBundle {
  const body = result !== null && typeof result === "object" ? (result as Record<string, unknown>) : {};
  const text = (key: string): string => (typeof body[key] === "string" ? body[key] : "");
  const number = (key: string): number => (typeof body[key] === "number" ? body[key] : 0);
  return {
    importId: text("import_id"),
    digest: text("digest"),
    cookieCount: number("cookie_count"),
    domains: Array.isArray(body["domains"]) ? (body["domains"] as unknown[]).filter((domain): domain is string => typeof domain === "string") : [],
    moreDomains: number("more_domains"),
    expiresAt: text("expires_at"),
  };
}

export interface Composed {
  readonly host: SecretsHost;
  readonly vault: PasswordVault;
  readonly importer: BrowserImportService;
  readonly filler: PasswordFiller;
}

/** Build the password manager, the filler and the importer, and the host that answers their channels. */
export function composeSecrets(options: SecretsOptions): Composed {
  const vault = new PasswordVault({ path: join(options.userData, VAULT_FILE), cipher: options.cipher, config: options.config, ...(options.now ? { now: options.now } : {}) });
  const confirmation: CrossSiteConfirmation = {
    confirm: async (_window: ManagedWindow, ask) => proceeds(await options.ask(crossSiteDialog(ask))),
  };
  const filler = new PasswordFiller({
    vault,
    config: options.config,
    probe: new ServerPageProbe(options.server),
    writer: options.writer ?? null,
    confirmation,
    ...(options.now ? { now: options.now } : {}),
  });
  const importConfirmation: ImportConfirmation = {
    confirm: async (_window: ManagedWindow, ask) => proceeds(await options.ask(importDialog(ask))),
  };
  const stage: StageBundle = async (cookies, digest) => {
    const response = await options.server.invoke("ImportBrowserSession", options.newCommandId(), {
      workspace_id: options.workspaceId(),
      cookies,
      digest,
    });
    return decodeStaged(response.result);
  };
  const importer = new BrowserImportService({
    config: options.config,
    home: options.home,
    keys: options.keys ?? new KeychainKeys(),
    vault,
    confirmation: importConfirmation,
    stage,
    ...(options.now ? { now: options.now } : {}),
  });
  return { host: new SecretsHost({ config: options.config, vault, filler, importer }), vault, importer, filler };
}
