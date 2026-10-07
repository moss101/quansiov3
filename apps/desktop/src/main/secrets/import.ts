/**
 * Importing from another browser (ADP-009, DOMAIN.md §13.9 B): the person's act, one type at a time.
 *
 * **Two types, two opt-ins.** Saved logins and cookies are separate requests with separate native questions; nothing is read until the
 * person answers the one for that type, and an answer for one type never covers the other. The default button is Cancel, and a question
 * left open past `import_approval_ttl_ms` is a no.
 *
 * * **Logins** go into this app's password manager (`PasswordVault`) on this device and nowhere else: not to the server, not to a worker,
 *   not to a model. A login that already exists for the same origin and username is replaced by the import.
 * * **Cookies** are *staged*: sealed on the server as one bundle (`ImportBrowserSession`) and applied to the agent's browser only by
 *   the approved `browser.import_session` call, which is asked every time (effect class `browser.session.import`, tier 4). Staging gives
 *   the agent nothing; the bundle's digest, count and domains are what the approval shows.
 *
 * What the renderer may name is a family and a profile directory **from the scan** and, for cookies, domains the scan listed. It names
 * no path, and the answers carry counts, domains and times: never a value.
 */

import type { LocalSecretsConfig } from "./config.js";
import { bundleDigest, bundleDomains } from "./bundle.js";
import {
  ImportError,
  countLogins,
  discoverProfiles,
  familyOf,
  readCookies,
  readLogins,
  summarizeCookies,
  supportDirectory,
  type BundleCookie,
  type ChromiumKeys,
  type ProfileInfo,
  type ReadLimits,
} from "./chromium.js";
import type { ImportAsk } from "./dialogs.js";
import { zero } from "./secret.js";
import { originOf } from "./origins.js";
import { type PasswordVault, VaultError } from "./vault.js";
import type { ManagedWindow } from "../windows.js";

/** Asks the person. `main` supplies Electron's message box; the default answer is no. */
export interface ImportConfirmation {
  confirm(window: ManagedWindow, ask: ImportAsk, ttlMs: number): Promise<boolean>;
}

/** What the server answered when a bundle was staged: counts, domains and times. */
export interface StagedBundle {
  readonly importId: string;
  readonly digest: string;
  readonly cookieCount: number;
  readonly domains: readonly string[];
  readonly moreDomains: number;
  readonly expiresAt: string;
}

/** Sends a bundle to the server (`ImportBrowserSession`). */
export type StageBundle = (cookies: readonly BundleCookie[], digest: string) => Promise<StagedBundle>;

export interface ScanProfile {
  readonly family: string;
  readonly familyLabel: string;
  readonly profile: string;
  readonly displayName: string;
  /** Cookie counts without decrypting anything, or `null` when the profile has no cookie database or it could not be read. */
  readonly cookies: { readonly total: number; readonly domains: readonly { readonly domain: string; readonly count: number }[] } | null;
  readonly logins: number | null;
}

export type Answer<T> =
  | { readonly ok: true; readonly result: T }
  | { readonly ok: false; readonly error: { readonly code: string; readonly message: string; readonly details?: { readonly reason: string } } };

function done<T>(result: T): Answer<T> {
  return { ok: true, result };
}

function refuse(code: string, message: string, reason?: string): Answer<never> {
  return { ok: false, error: { code, message, ...(reason === undefined ? {} : { details: { reason } }) } };
}

const REFUSALS: Readonly<Record<string, readonly [string, string]>> = {
  unsupported: ["ADAPTER_UNAVAILABLE", "this device or this browser's storage cannot be imported from"],
  not_found: ["NOT_FOUND", "the profile's data is not there"],
  too_large: ["VALIDATION_SCHEMA", "the profile's data is larger than an import reads"],
  too_many: ["VALIDATION_SCHEMA", "there is more than one import carries; choose fewer domains"],
  keychain_denied: ["POLICY_DENIED", "the keychain did not give out the browser's key"],
  unreadable: ["INTERNAL", "the profile's data could not be read"],
};

function refusalOf(error: unknown): Answer<never> {
  if (error instanceof ImportError) {
    const [code, message] = REFUSALS[error.reason] ?? ["INTERNAL", "the import failed"];
    return refuse(code, message, error.reason);
  }
  throw error;
}

export interface ImportServiceOptions {
  readonly config: LocalSecretsConfig;
  /** The home folder whose browsers are scanned. */
  readonly home: string;
  readonly keys: ChromiumKeys;
  readonly vault: PasswordVault;
  readonly confirmation: ImportConfirmation;
  readonly stage: StageBundle;
  readonly now?: () => number;
  /** How many domains a scan lists per profile. */
  readonly scanDomains?: number;
}

/** The domains a confirmation lists before it says "and N more". */
const ASK_DOMAINS = 5;

export class BrowserImportService {
  readonly #o: ImportServiceOptions;

  constructor(options: ImportServiceOptions) {
    this.#o = options;
  }

  get #limits(): ReadLimits {
    return { maxDbBytes: this.#o.config.importMaxDbBytes, maxRows: Math.max(this.#o.config.importMaxCookies, this.#o.config.importMaxLogins) };
  }

  #profiles(): ProfileInfo[] {
    return discoverProfiles(supportDirectory(this.#o.home));
  }

  /** The profile the person named, only if the scan lists it. */
  #profile(family: string, profile: string): ProfileInfo | null {
    return this.#profiles().find((candidate) => candidate.family === family && candidate.profile === profile) ?? null;
  }

  #off(): Answer<never> | null {
    const c = this.#o.config;
    return c.enabled && c.importEnabled ? null : refuse("POLICY_DENIED", "importing from another browser is switched off");
  }

  /** The browsers and profiles this device has, with what each holds. Reads no value and never touches the keychain. */
  scan(): Answer<{ readonly profiles: readonly ScanProfile[] }> {
    const off = this.#off();
    if (off !== null) {
      return off;
    }
    const listed: ScanProfile[] = [];
    for (const profile of this.#profiles()) {
      let cookies: ScanProfile["cookies"] = null;
      let logins: number | null = null;
      try {
        const summary = summarizeCookies(profile, this.#limits);
        cookies = { total: summary.total, domains: summary.domains.slice(0, this.#o.scanDomains ?? 25) };
      } catch (error) {
        if (!(error instanceof ImportError)) {
          throw error;
        }
      }
      try {
        logins = countLogins(profile, this.#limits);
      } catch (error) {
        if (!(error instanceof ImportError)) {
          throw error;
        }
      }
      listed.push({ family: profile.family, familyLabel: profile.familyLabel, profile: profile.profile, displayName: profile.displayName, cookies, logins });
    }
    return done({ profiles: listed });
  }

  async #asked(window: ManagedWindow, ask: ImportAsk): Promise<boolean> {
    const now = this.#o.now ?? Date.now;
    const started = now();
    const ttl = this.#o.config.importApprovalTtlMs;
    const yes = await this.#o.confirmation.confirm(window, ask, ttl);
    // A question answered after its time is a question nobody saw in time.
    return yes && now() - started <= ttl;
  }

  /** Import the profile's saved logins into the password manager. */
  async importLogins(window: ManagedWindow, request: { readonly family: string; readonly profile: string }): Promise<Answer<{ readonly imported: number; readonly skipped: number }>> {
    const off = this.#off();
    if (off !== null) {
      return off;
    }
    const profile = this.#profile(request.family, request.profile);
    const family = familyOf(request.family);
    if (profile === null || family === null || profile.loginsPath === null) {
      return refuse("NOT_FOUND", "no such profile with saved logins");
    }
    if (!this.#o.vault.available()) {
      return refuse("ADAPTER_UNAVAILABLE", "the platform cannot keep a password manager encrypted, so nothing is imported", "vault_unavailable");
    }
    if (this.#o.vault.locked()) {
      return refuse("CONFLICT_STATE", "the saved passwords could not be read on this device, so nothing is changed", "vault_locked");
    }
    let count: number;
    try {
      count = countLogins(profile, this.#limits);
    } catch (error) {
      return refusalOf(error);
    }
    if (count === 0) {
      return done({ imported: 0, skipped: 0 });
    }
    // The question comes before the key: nothing is decrypted, and the keychain is not asked, unless the person says yes.
    if (!(await this.#asked(window, { kind: "logins", browser: profile.familyLabel, profile: profile.displayName, count, domains: [], moreDomains: 0 }))) {
      return refuse("POLICY_DENIED", "the person did not confirm the import", "declined");
    }
    let key: Buffer;
    try {
      key = await this.#o.keys.key(family);
    } catch (error) {
      return refusalOf(error);
    }
    try {
      const read = readLogins(profile, key, this.#limits);
      let imported = 0;
      let skipped = read.skipped;
      try {
        for (const login of read.logins) {
          const origin = originOf(login.origin);
          try {
            if (origin === null) {
              skipped += 1;
              continue;
            }
            this.#o.vault.save({ origin, username: login.username, password: login.password.toString("utf8") });
            imported += 1;
          } catch (error) {
            if (!(error instanceof VaultError)) {
              throw error;
            }
            skipped += 1;
          }
        }
      } finally {
        for (const login of read.logins) {
          zero(login.password);
        }
      }
      return done({ imported, skipped });
    } catch (error) {
      return refusalOf(error);
    } finally {
      zero(key);
    }
  }

  /** Stage the cookies of the chosen domains for the agent's browser. Nothing is applied. */
  async stageCookies(
    window: ManagedWindow,
    request: { readonly family: string; readonly profile: string; readonly domains: readonly string[] },
  ): Promise<Answer<StagedBundle>> {
    const off = this.#off();
    if (off !== null) {
      return off;
    }
    const profile = this.#profile(request.family, request.profile);
    const family = familyOf(request.family);
    if (profile === null || family === null || profile.cookiesPath === null) {
      return refuse("NOT_FOUND", "no such profile with cookies");
    }
    const chosen = new Set(request.domains.map((domain) => domain.toLowerCase()));
    if (chosen.size === 0) {
      return refuse("VALIDATION_SCHEMA", "choose at least one domain");
    }
    let summary;
    try {
      summary = summarizeCookies(profile, this.#limits);
    } catch (error) {
      return refusalOf(error);
    }
    const known = new Map(summary.domains.map((entry) => [entry.domain, entry.count]));
    let count = 0;
    for (const domain of chosen) {
      const held = known.get(domain);
      if (held === undefined) {
        return refuse("VALIDATION_SCHEMA", "a chosen domain is not one the profile holds cookies for");
      }
      count += held;
    }
    if (count > this.#o.config.importMaxCookies) {
      return refuse("VALIDATION_SCHEMA", `an import carries at most ${String(this.#o.config.importMaxCookies)} cookies; choose fewer domains`, "too_many");
    }
    const listed = [...chosen].sort();
    if (!(await this.#asked(window, { kind: "cookies", browser: profile.familyLabel, profile: profile.displayName, count, domains: listed.slice(0, ASK_DOMAINS), moreDomains: Math.max(0, listed.length - ASK_DOMAINS) }))) {
      return refuse("POLICY_DENIED", "the person did not confirm the import", "declined");
    }
    let key: Buffer;
    try {
      key = await this.#o.keys.key(family);
    } catch (error) {
      return refusalOf(error);
    }
    let read;
    try {
      read = readCookies(profile, chosen, key, this.#limits, this.#o.now);
    } catch (error) {
      return refusalOf(error);
    } finally {
      zero(key);
    }
    if (read.cookies.length === 0) {
      return refuse("NOT_FOUND", "none of the chosen cookies could be read", "nothing_readable");
    }
    // The bundle leaves for the server with its digest; the server recomputes it and seals nothing that differs.
    const digest = bundleDigest(read.cookies);
    const staged = await this.#o.stage(read.cookies, digest);
    const domains = bundleDomains(read.cookies);
    return done({ ...staged, domains: staged.domains.length > 0 ? staged.domains : domains });
  }
}
