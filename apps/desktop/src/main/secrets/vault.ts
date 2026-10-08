/**
 * The human-only password manager (ADP-009, DOMAIN.md §13.9 A).
 *
 * Passwords the person saves for their own use in the managed browser are kept in one file in the app's data directory,
 * encrypted with the platform credential store's key (`safeStorage`: Keychain on macOS, DPAPI on Windows, the secret service on
 * Linux) — the same custody as the session secret. When the platform can not encrypt, **nothing is written**: there is never a
 * plaintext file. The file is a device-local cache of the person's own data, not an authority, and no server, worker or model
 * ever reads it.
 *
 * Three properties are the point, and each has a test:
 *
 * * **The list never carries a password.** {@link PasswordVault.list}, {@link PasswordVault.suggest} and {@link PasswordVault.get}
 *   return metadata (`id`, `origin`, `username`, times). The only way to a password is {@link PasswordVault.password}, which
 *   returns a {@link Secret} and is called by the fill path inside main and by nothing a window can reach.
 * * **A file that can not be read is never overwritten.** A vault whose file exists and does not decrypt (another keychain, a
 *   truncated write) is `locked`: reads answer empty and writes are refused, so a recoverable file is not replaced by a new one.
 * * **A write is atomic and private.** Temporary file, mode 0600, rename.
 */

import { randomBytes } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

import type { SecretCipher } from "../token-store.js";
import type { LocalSecretsConfig } from "./config.js";
import { originOf } from "./origins.js";
import { Secret } from "./secret.js";

export const VAULT_FILE = "password-vault.bin";
const SCHEMA_VERSION = 1;

/** What a window may know about a saved login: never the password. */
export interface PasswordEntry {
  readonly id: string;
  readonly origin: string;
  readonly username: string;
  readonly createdAt: number;
  readonly updatedAt: number;
}

export type VaultRefusal =
  | "unavailable"
  | "locked"
  | "invalid_origin"
  | "empty"
  | "too_long"
  | "too_many"
  | "not_found";

/** A refusal with a stable reason. */
export class VaultError extends Error {
  readonly reason: VaultRefusal;

  constructor(reason: VaultRefusal, message: string) {
    super(message);
    this.name = "VaultError";
    this.reason = reason;
  }
}

interface StoredEntry extends PasswordEntry {
  readonly password: string;
}

export interface VaultOptions {
  readonly path: string;
  readonly cipher: SecretCipher;
  readonly config: LocalSecretsConfig;
  readonly now?: () => number;
  readonly newId?: () => string;
}

function defaultId(): string {
  return `pw_${randomBytes(16).toString("hex")}`;
}

function metadata(entry: StoredEntry): PasswordEntry {
  return { id: entry.id, origin: entry.origin, username: entry.username, createdAt: entry.createdAt, updatedAt: entry.updatedAt };
}

export class PasswordVault {
  readonly #o: VaultOptions;
  #entries: StoredEntry[] | null = null;
  #locked = false;

  constructor(options: VaultOptions) {
    this.#o = options;
  }

  /** Whether the platform can keep the file encrypted. When it can not, the vault holds and saves nothing. */
  available(): boolean {
    return this.#o.cipher.isEncryptionAvailable();
  }

  /** Whether a file exists that this device could not read. */
  locked(): boolean {
    this.#load();
    return this.#locked;
  }

  count(): number {
    return this.#load().length;
  }

  list(): PasswordEntry[] {
    return this.#load().map(metadata);
  }

  get(id: string): PasswordEntry | null {
    const found = this.#load().find((entry) => entry.id === id);
    return found === undefined ? null : metadata(found);
  }

  /** The saved logins for exactly this origin (scheme, host and port), newest first, at most `suggestionsMax`. */
  suggest(pageOrigin: string): PasswordEntry[] {
    const origin = originOf(pageOrigin);
    if (origin === null) {
      return [];
    }
    return this.#load()
      .filter((entry) => entry.origin === origin)
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .slice(0, this.#o.config.suggestionsMax)
      .map(metadata);
  }

  /**
   * Save a login, replacing the one already saved for the same origin and username.
   *
   * # Errors
   * {@link VaultError}: `unavailable` (the platform can not encrypt: nothing is written), `locked`, `invalid_origin`, `empty`,
   * `too_long`, `too_many`.
   */
  save(input: { origin: string; username: string; password: string }): PasswordEntry {
    if (!this.available()) {
      throw new VaultError("unavailable", "the platform can not encrypt, so nothing is saved");
    }
    const entries = this.#load();
    if (this.#locked) {
      throw new VaultError("locked", "the saved passwords could not be read on this device, so nothing is changed");
    }
    const origin = originOf(input.origin);
    if (origin === null) {
      throw new VaultError("invalid_origin", "a saved login belongs to an http or https origin");
    }
    if (input.username === "" || input.password === "") {
      throw new VaultError("empty", "a login needs a username and a password");
    }
    const max = this.#o.config.fieldMaxChars;
    if (input.username.length > max || input.password.length > max) {
      throw new VaultError("too_long", `a username or password is at most ${String(max)} characters`);
    }
    const now = (this.#o.now ?? Date.now)();
    const index = entries.findIndex((entry) => entry.origin === origin && entry.username === input.username);
    if (index < 0 && entries.length >= this.#o.config.passwordsMax) {
      throw new VaultError("too_many", `at most ${String(this.#o.config.passwordsMax)} logins are kept`);
    }
    const previous = index < 0 ? undefined : entries[index];
    const saved: StoredEntry = {
      id: previous?.id ?? (this.#o.newId ?? defaultId)(),
      origin,
      username: input.username,
      password: input.password,
      createdAt: previous?.createdAt ?? now,
      updatedAt: now,
    };
    const next = index < 0 ? [...entries, saved] : entries.map((entry, i) => (i === index ? saved : entry));
    this.#write(next);
    return metadata(saved);
  }

  remove(id: string): boolean {
    const entries = this.#load();
    if (this.#locked) {
      throw new VaultError("locked", "the saved passwords could not be read on this device, so nothing is changed");
    }
    const next = entries.filter((entry) => entry.id !== id);
    if (next.length === entries.length) {
      return false;
    }
    this.#write(next);
    return true;
  }

  /**
   * The password of a saved login, as a {@link Secret}. Main-process code only: no IPC channel returns it, and the caller
   * wipes it once it has been inserted.
   */
  password(id: string): Secret {
    const found = this.#load().find((entry) => entry.id === id);
    if (found === undefined) {
      throw new VaultError("not_found", "no saved login has this id");
    }
    return new Secret(found.password);
  }

  #load(): StoredEntry[] {
    if (this.#entries !== null) {
      return this.#entries;
    }
    this.#entries = [];
    if (!existsSync(this.#o.path)) {
      return this.#entries;
    }
    if (!this.available()) {
      this.#locked = true;
      return this.#entries;
    }
    try {
      const decoded: unknown = JSON.parse(this.#o.cipher.decryptString(readFileSync(this.#o.path)));
      const record = decoded !== null && typeof decoded === "object" ? (decoded as Record<string, unknown>) : {};
      const raw = record["entries"];
      if (record["schema_version"] !== SCHEMA_VERSION || !Array.isArray(raw)) {
        this.#locked = true;
        return this.#entries;
      }
      const parsed: StoredEntry[] = [];
      for (const item of raw as unknown[]) {
        const entry = item !== null && typeof item === "object" ? (item as Record<string, unknown>) : {};
        const { id, origin, username, password, createdAt, updatedAt } = entry;
        if (
          typeof id !== "string" ||
          typeof origin !== "string" ||
          typeof username !== "string" ||
          typeof password !== "string" ||
          typeof createdAt !== "number" ||
          typeof updatedAt !== "number"
        ) {
          this.#locked = true;
          return (this.#entries = []);
        }
        parsed.push({ id, origin, username, password, createdAt, updatedAt });
      }
      this.#entries = parsed;
    } catch {
      this.#locked = true;
      this.#entries = [];
    }
    return this.#entries;
  }

  #write(entries: StoredEntry[]): void {
    const payload = JSON.stringify({ schema_version: SCHEMA_VERSION, entries });
    const encrypted = this.#o.cipher.encryptString(payload);
    mkdirSync(dirname(this.#o.path), { recursive: true, mode: 0o700 });
    const temporary = `${this.#o.path}.tmp`;
    writeFileSync(temporary, encrypted, { mode: 0o600 });
    renameSync(temporary, this.#o.path);
    this.#entries = entries;
  }

  /** Forget everything (sign-out of the device, or the person's reset). */
  clear(): void {
    rmSync(this.#o.path, { force: true });
    rmSync(`${this.#o.path}.tmp`, { force: true });
    this.#entries = [];
    this.#locked = false;
  }
}
