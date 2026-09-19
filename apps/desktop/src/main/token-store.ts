/**
 * Refresh-token custody (APP-002, DOMAIN.md §2): the OS keychain on desktop.
 *
 * Electron's `safeStorage` encrypts with a key held by the platform credential store
 * (Keychain on macOS, DPAPI on Windows, the secret service on Linux), which is how "stored in
 * the OS keychain" is realised without a native bridge of our own. The access token lives in
 * memory only; the refresh token is the only thing at rest, and only ever encrypted. When the
 * platform cannot encrypt, nothing is persisted and the user signs in again next launch —
 * never a plaintext file.
 */

import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

/** The slice of Electron's `safeStorage` the store uses. */
export interface SecretCipher {
  isEncryptionAvailable(): boolean;
  encryptString(plainText: string): Buffer;
  decryptString(encrypted: Buffer): string;
}

export interface StoredSession {
  readonly serverOrigin: string;
  readonly refreshToken: string;
  readonly refreshExpiresAt: string;
  readonly userId: string;
  readonly tenantId: string;
}

export const SESSION_SECRET_FILE = "session-secret.bin";
const SCHEMA_VERSION = 1;

export class TokenStore {
  readonly path: string;
  readonly #cipher: SecretCipher;

  constructor(userDataDir: string, cipher: SecretCipher) {
    this.path = join(userDataDir, SESSION_SECRET_FILE);
    this.#cipher = cipher;
  }

  /** Whether the platform can keep a refresh token at rest. */
  persistent(): boolean {
    return this.#cipher.isEncryptionAvailable();
  }

  /** Keep `session`; returns false (and keeps nothing) when the platform cannot encrypt. */
  save(session: StoredSession): boolean {
    if (!this.#cipher.isEncryptionAvailable()) {
      return false;
    }
    const payload = JSON.stringify({ schema_version: SCHEMA_VERSION, ...session });
    const encrypted = this.#cipher.encryptString(payload);
    mkdirSync(dirname(this.path), { recursive: true, mode: 0o700 });
    const temporary = `${this.path}.tmp`;
    writeFileSync(temporary, encrypted, { mode: 0o600 });
    renameSync(temporary, this.path);
    return true;
  }

  /** The stored session, or null when absent, undecryptable or from another schema. */
  load(): StoredSession | null {
    if (!existsSync(this.path) || !this.#cipher.isEncryptionAvailable()) {
      return null;
    }
    try {
      const decoded: unknown = JSON.parse(this.#cipher.decryptString(readFileSync(this.path)));
      if (!decoded || typeof decoded !== "object") {
        return null;
      }
      const record = decoded as Record<string, unknown>;
      if (record["schema_version"] !== SCHEMA_VERSION) {
        return null;
      }
      const text = (key: string): string | null => {
        const value = record[key];
        return typeof value === "string" ? value : null;
      };
      const serverOrigin = text("serverOrigin");
      const refreshToken = text("refreshToken");
      const refreshExpiresAt = text("refreshExpiresAt");
      const userId = text("userId");
      const tenantId = text("tenantId");
      if (!serverOrigin || !refreshToken || !refreshExpiresAt || !userId || !tenantId) {
        return null;
      }
      return { serverOrigin, refreshToken, refreshExpiresAt, userId, tenantId };
    } catch {
      return null;
    }
  }

  /** Forget the stored session (sign-out, or a refresh token the server rejected). */
  clear(): void {
    rmSync(this.path, { force: true });
    rmSync(`${this.path}.tmp`, { force: true });
  }
}

/** A cipher for tests and for hosts without a credential store: keeps nothing at rest. */
export const NO_ENCRYPTION: SecretCipher = {
  isEncryptionAvailable: () => false,
  encryptString: () => {
    throw new Error("encryption is not available");
  },
  decryptString: () => {
    throw new Error("encryption is not available");
  },
};
