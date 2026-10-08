/**
 * Reading another browser's profile (ADP-009, DOMAIN.md §13.9 B).
 *
 * A person who already uses a Chromium-family browser can bring its saved logins into this app's password manager and stage its
 * cookies for the agent's browser. This file is the reading half and nothing else: it finds profiles, counts what they hold
 * **without decrypting anything**, and, when the person has said yes, decrypts what they chose.
 *
 * * **Nothing is read without a profile main found.** The renderer names a browser family and a profile directory *as the scan listed
 *   them*; no path comes from a window.
 * * **The database is read from a copy.** A profile's database is copied (with its write-ahead log) into a private temporary
 *   directory, opened there, and the copy is deleted; the browser's own files are never opened for writing, and a running browser's
 *   lock is not touched.
 * * **The key is the person's own keychain item, asked for by the person.** The platform's "Safe Storage" password is read with the
 *   system's `security` tool, which makes macOS show its own permission prompt; nothing here answers it, scripts it or caches the
 *   key. The derived AES key lives in a `Buffer` that is zeroed as soon as the values are decrypted.
 * * **Counting needs no key.** A scan lists profiles, cookie counts per domain and the number of saved logins from columns that are
 *   not encrypted; it never touches the keychain.
 *
 * Supported: macOS, Chrome, Chromium, Brave and Edge (`v10` values: AES-128-CBC with a key derived from the Safe Storage password
 * by PBKDF2-HMAC-SHA1, 1003 rounds, salt `saltysalt`; newer browsers prefix a cookie's plaintext with the SHA-256 of its host key,
 * which is checked and removed). Other platforms and other storage formats answer `unsupported` rather than guess.
 */

import { execFile } from "node:child_process";
import { createDecipheriv, createHash, pbkdf2Sync } from "node:crypto";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { zero } from "./secret.js";

export type ImportRefusal =
  | "unsupported"
  | "not_found"
  | "too_large"
  | "too_many"
  | "keychain_denied"
  | "unreadable";

/** A refusal with a stable reason and no profile content in its message. */
export class ImportError extends Error {
  readonly reason: ImportRefusal;

  constructor(reason: ImportRefusal, message: string) {
    super(message);
    this.name = "ImportError";
    this.reason = reason;
  }
}

export type FamilyId = "chrome" | "chromium" | "brave" | "edge";

export interface BrowserFamily {
  readonly id: FamilyId;
  readonly label: string;
  /** The profile root under `~/Library/Application Support`. */
  readonly directory: string;
  /** The keychain item holding the Safe Storage password. */
  readonly keychainService: string;
  readonly keychainAccount: string;
}

export const FAMILIES: readonly BrowserFamily[] = [
  { id: "chrome", label: "Google Chrome", directory: "Google/Chrome", keychainService: "Chrome Safe Storage", keychainAccount: "Chrome" },
  { id: "chromium", label: "Chromium", directory: "Chromium", keychainService: "Chromium Safe Storage", keychainAccount: "Chromium" },
  { id: "brave", label: "Brave", directory: "BraveSoftware/Brave-Browser", keychainService: "Brave Safe Storage", keychainAccount: "Brave" },
  { id: "edge", label: "Microsoft Edge", directory: "Microsoft Edge", keychainService: "Microsoft Edge Safe Storage", keychainAccount: "Microsoft Edge" },
];

export function familyOf(id: string): BrowserFamily | null {
  return FAMILIES.find((family) => family.id === id) ?? null;
}

/** One profile of an installed browser, as the scan found it. */
export interface ProfileInfo {
  readonly family: FamilyId;
  readonly familyLabel: string;
  /** The profile's directory name (`Default`, `Profile 2`). */
  readonly profile: string;
  /** The name the browser shows for it, when `Local State` has one. */
  readonly displayName: string;
  readonly cookiesPath: string | null;
  readonly loginsPath: string | null;
}

const PROFILE_NAME = /^(Default|Profile \d{1,3})$/;

/** The Application Support directory of a home folder (macOS). */
export function supportDirectory(home: string): string {
  return join(home, "Library", "Application Support");
}

function displayNames(root: string): Record<string, string> {
  try {
    const state: unknown = JSON.parse(readFileSync(join(root, "Local State"), "utf8"));
    const cache = (state as { profile?: { info_cache?: Record<string, { name?: unknown }> } }).profile?.info_cache ?? {};
    const names: Record<string, string> = {};
    for (const [directory, info] of Object.entries(cache)) {
      if (typeof info.name === "string") {
        names[directory] = info.name.slice(0, 64);
      }
    }
    return names;
  } catch {
    return {};
  }
}

/** Find the profiles under `supportDir`: a directory per profile that has a cookie database or a login database. */
export function discoverProfiles(supportDir: string, families: readonly BrowserFamily[] = FAMILIES): ProfileInfo[] {
  const found: ProfileInfo[] = [];
  for (const family of families) {
    const root = join(supportDir, family.directory);
    if (!existsSync(root)) {
      continue;
    }
    const names = displayNames(root);
    let entries: string[];
    try {
      entries = readdirSync(root);
    } catch {
      continue;
    }
    for (const profile of entries.filter((entry) => PROFILE_NAME.test(entry)).sort()) {
      const directory = join(root, profile);
      const cookies = [join(directory, "Network", "Cookies"), join(directory, "Cookies")].find((path) => existsSync(path)) ?? null;
      const logins = existsSync(join(directory, "Login Data")) ? join(directory, "Login Data") : null;
      if (cookies !== null || logins !== null) {
        found.push({ family: family.id, familyLabel: family.label, profile, displayName: names[profile] ?? profile, cookiesPath: cookies, loginsPath: logins });
      }
    }
  }
  return found;
}

// ------------------------------------------------------------------ the key

/** Where the Safe Storage password comes from. A test supplies its own; production asks the keychain. */
export interface ChromiumKeys {
  /** The derived AES key for a family. The caller zeroes it. */
  key(family: BrowserFamily): Promise<Buffer>;
}

/** PBKDF2-HMAC-SHA1(password, "saltysalt", 1003, 16): how Chromium derives its value-encryption key on macOS. */
export function deriveKey(password: Buffer | string): Buffer {
  return pbkdf2Sync(password, "saltysalt", 1003, 16, "sha1");
}

/** Runs a program and returns its standard output. Injected so a test never touches a real keychain. */
export type RunProgram = (file: string, args: readonly string[]) => Promise<Buffer>;

const runProgram: RunProgram = (file, args) =>
  new Promise((resolve, reject) => {
    execFile(file, [...args], { encoding: "buffer", maxBuffer: 1024 * 1024 }, (error, stdout) => {
      if (error) {
        reject(error instanceof Error ? error : new Error("the program failed"));
      } else {
        resolve(stdout);
      }
    });
  });

/**
 * The macOS keychain, through `/usr/bin/security`. The system shows its own permission prompt for the item; the person answers it.
 * A refusal, a missing item and a missing tool are all `keychain_denied`: nothing is imported and nothing is guessed.
 */
export class KeychainKeys implements ChromiumKeys {
  readonly #run: RunProgram;

  constructor(run: RunProgram = runProgram) {
    this.#run = run;
  }

  async key(family: BrowserFamily): Promise<Buffer> {
    let output: Buffer;
    try {
      output = await this.#run("/usr/bin/security", ["find-generic-password", "-w", "-s", family.keychainService, "-a", family.keychainAccount]);
    } catch {
      throw new ImportError("keychain_denied", `the keychain did not give out the ${family.label} key`);
    }
    // The tool prints the password and a newline.
    const end = output.length > 0 && output[output.length - 1] === 0x0a ? output.length - 1 : output.length;
    const password = output.subarray(0, end);
    try {
      return deriveKey(password);
    } finally {
      zero(output);
    }
  }
}

// ------------------------------------------------------------------ decryption

const IV = Buffer.alloc(16, 0x20);

/**
 * Decrypt one stored value.
 *
 * `null` for a value that is not a `v10`/`v11` frame or does not decrypt (a wrong key, a truncated value): the caller counts it
 * as skipped. When `hostKey` is given (a cookie), the SHA-256 of it that newer browsers put in front of the plaintext is checked and
 * removed; a value with a different 32-byte prefix is left as it is.
 */
export function decryptValue(blob: Uint8Array, key: Buffer, hostKey: string | null): Buffer | null {
  const frame = Buffer.from(blob);
  if (frame.length < 3 + 16 || (frame.subarray(0, 3).toString("latin1") !== "v10" && frame.subarray(0, 3).toString("latin1") !== "v11")) {
    return null;
  }
  try {
    const decipher = createDecipheriv("aes-128-cbc", key, IV);
    const plain = Buffer.concat([decipher.update(frame.subarray(3)), decipher.final()]);
    if (hostKey !== null && plain.length >= 32 && plain.subarray(0, 32).equals(createHash("sha256").update(hostKey).digest())) {
      const stripped = Buffer.from(plain.subarray(32));
      zero(plain);
      return stripped;
    }
    return plain;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ reading

/** What a cookie database holds, without decrypting anything: counts per domain. */
export interface CookieSummary {
  readonly total: number;
  readonly domains: readonly { readonly domain: string; readonly count: number }[];
}

/** A cookie as the runtime's bundle spells it (`ImportCookie`, DOMAIN.md §13.9 B). */
export interface BundleCookie {
  readonly name: string;
  readonly value: string;
  readonly domain: string;
  readonly path: string;
  readonly secure: boolean;
  readonly http_only: boolean;
  readonly same_site: "none" | "lax" | "strict" | "unspecified";
  readonly expires: number | null;
}

export interface ReadLimits {
  readonly maxDbBytes: number;
  readonly maxRows: number;
}

type SqliteModule = typeof import("node:sqlite");
type Database = InstanceType<SqliteModule["DatabaseSync"]>;

/** Open a private copy of a database (with its write-ahead log), run `use`, and delete the copy. Synchronous: the reads are bounded by the configured size. */
function withCopy<T>(path: string, limits: ReadLimits, use: (database: Database) => T): T {
  let size: number;
  try {
    size = statSync(path).size;
  } catch {
    throw new ImportError("not_found", "the profile's database is not there");
  }
  if (size > limits.maxDbBytes) {
    throw new ImportError("too_large", "the profile's database is larger than an import reads");
  }
  const directory = mkdtempSync(join(tmpdir(), "quansio-import-"));
  try {
    copyFileSync(path, join(directory, "db"));
    for (const suffix of ["-wal", "-shm"]) {
      if (existsSync(`${path}${suffix}`)) {
        copyFileSync(`${path}${suffix}`, join(directory, `db${suffix}`));
      }
    }
    // `node:sqlite` is a built-in of the runtime (Electron's Node); it is fetched by name so no bundler tries to resolve it.
    const sqlite = process.getBuiltinModule("node:sqlite") as SqliteModule | undefined;
    if (sqlite === undefined) {
      throw new ImportError("unsupported", "this build cannot read a browser profile's database");
    }
    const database = new sqlite.DatabaseSync(join(directory, "db"));
    try {
      return use(database);
    } catch (error) {
      if (error instanceof ImportError) {
        throw error;
      }
      throw new ImportError("unreadable", "the profile's database could not be read");
    } finally {
      database.close();
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

const bareDomain = (hostKey: string): string => hostKey.replace(/^\./, "").toLowerCase();

/** A column read as text: anything that is not text is the fallback (a database holds what a browser put in it). */
const textOf = (value: unknown, fallback = ""): string => (typeof value === "string" ? value : fallback);

/** A column read as a whole number: anything that is not a number is zero. */
const numberOf = (value: unknown): number => (typeof value === "number" ? value : typeof value === "bigint" ? Number(value) : 0);

/** Cookie counts per domain, most first. No key, no values. */
export function summarizeCookies(profile: ProfileInfo, limits: ReadLimits): CookieSummary {
  if (profile.cookiesPath === null) {
    return { total: 0, domains: [] };
  }
  return withCopy(profile.cookiesPath, limits, (database) => {
    const rows = database.prepare("SELECT host_key AS host, count(*) AS n FROM cookies GROUP BY host_key").all() as { host: string; n: number }[];
    const byDomain = new Map<string, number>();
    let total = 0;
    for (const row of rows) {
      const domain = bareDomain(textOf(row.host));
      byDomain.set(domain, (byDomain.get(domain) ?? 0) + numberOf(row.n));
      total += numberOf(row.n);
    }
    const domains = [...byDomain.entries()].map(([domain, count]) => ({ domain, count })).sort((a, b) => b.count - a.count || a.domain.localeCompare(b.domain));
    return { total, domains };
  });
}

/** How many saved logins a profile holds (not blocked entries). No key, no values. */
export function countLogins(profile: ProfileInfo, limits: ReadLimits): number {
  if (profile.loginsPath === null) {
    return 0;
  }
  return withCopy(profile.loginsPath, limits, (database) => {
    const row = database.prepare("SELECT count(*) AS n FROM logins WHERE blacklisted_by_user = 0").get() as { n: number } | undefined;
    return numberOf(row?.n);
  });
}

const SAME_SITE: Readonly<Record<number, BundleCookie["same_site"]>> = { [-1]: "unspecified", 0: "none", 1: "lax", 2: "strict" };
/** Seconds between 1601-01-01 (Chromium's epoch) and 1970-01-01. */
const EPOCH_DELTA_SECONDS = 11_644_473_600;

export interface CookieRead {
  readonly cookies: BundleCookie[];
  /** Cookies that were expired, were not decryptable, or were not for a chosen domain. */
  readonly skipped: number;
}

/** Read and decrypt the cookies of `domains` (bare, lowercase; a domain cookie and a host cookie of it both count). */
export function readCookies(
  profile: ProfileInfo,
  domains: ReadonlySet<string>,
  key: Buffer,
  limits: ReadLimits,
  now: () => number = () => Date.now(),
): CookieRead {
  if (profile.cookiesPath === null) {
    return { cookies: [], skipped: 0 };
  }
  return withCopy(profile.cookiesPath, limits, (database) => {
    // Chromium's timestamps are microseconds since 1601 and do not fit a double exactly (they pass 2^53), so integers are read as bigints.
    const statement = database.prepare("SELECT host_key, name, value, encrypted_value, path, expires_utc, is_secure, is_httponly, samesite FROM cookies");
    statement.setReadBigInts(true);
    const rows = statement.all() as Record<string, unknown>[];
    const cookies: BundleCookie[] = [];
    let skipped = 0;
    for (const row of rows) {
      const hostKey = textOf(row["host_key"]);
      if (!domains.has(bareDomain(hostKey))) {
        continue;
      }
      const expiresUtc = typeof row["expires_utc"] === "bigint" ? row["expires_utc"] : BigInt(numberOf(row["expires_utc"]));
      const expires = expiresUtc > 0n ? Number(expiresUtc / 1_000_000n) - EPOCH_DELTA_SECONDS : null;
      if (expires !== null && expires * 1000 <= now()) {
        skipped += 1;
        continue;
      }
      const stored = row["encrypted_value"] instanceof Uint8Array ? row["encrypted_value"] : new Uint8Array();
      let value: string;
      if (stored.length > 0) {
        const plain = decryptValue(stored, key, hostKey);
        if (plain === null) {
          skipped += 1;
          continue;
        }
        value = plain.toString("utf8");
        zero(plain);
      } else {
        value = textOf(row["value"]);
      }
      if (cookies.length >= limits.maxRows) {
        throw new ImportError("too_many", "the chosen domains hold more cookies than one import carries");
      }
      cookies.push({
        name: textOf(row["name"]),
        value,
        domain: hostKey,
        path: textOf(row["path"], "/") || "/",
        secure: numberOf(row["is_secure"]) === 1,
        http_only: numberOf(row["is_httponly"]) === 1,
        same_site: SAME_SITE[numberOf(row["samesite"])] ?? "unspecified",
        expires,
      });
    }
    return { cookies, skipped };
  });
}

/** A saved login as the profile holds it. The password is plaintext in memory until the caller stores it and drops it. */
export interface ProfileLogin {
  readonly origin: string;
  readonly username: string;
  readonly password: Buffer;
}

export interface LoginRead {
  readonly logins: ProfileLogin[];
  readonly skipped: number;
}

/** Read and decrypt the saved logins. The caller wipes each `password`. */
export function readLogins(profile: ProfileInfo, key: Buffer, limits: ReadLimits): LoginRead {
  if (profile.loginsPath === null) {
    return { logins: [], skipped: 0 };
  }
  return withCopy(profile.loginsPath, limits, (database) => {
    const rows = database
      .prepare("SELECT origin_url, username_value, password_value FROM logins WHERE blacklisted_by_user = 0")
      .all() as Record<string, unknown>[];
    const logins: ProfileLogin[] = [];
    let skipped = 0;
    for (const row of rows) {
      const stored = row["password_value"] instanceof Uint8Array ? row["password_value"] : new Uint8Array();
      const password = decryptValue(stored, key, null);
      const username = textOf(row["username_value"]);
      if (password === null || username === "") {
        if (password !== null) {
          zero(password);
        }
        skipped += 1;
        continue;
      }
      if (logins.length >= limits.maxRows) {
        for (const login of logins) {
          zero(login.password);
        }
        throw new ImportError("too_many", "the profile holds more logins than one import carries");
      }
      logins.push({ origin: textOf(row["origin_url"]), username, password });
    }
    return { logins, skipped };
  });
}
