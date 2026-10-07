/**
 * The human-only password manager (ADP-009, DOMAIN.md §13.9 A): what is kept, how, and what can never come out of it.
 *
 * The cipher here is a test double for Electron's `safeStorage` (the platform keychain is not available to a unit test); it is
 * deliberately a real transformation, so the tests can prove the file is not the password. Test values only.
 */

import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspect } from "node:util";

import { afterEach, describe, expect, it } from "vitest";

import { loadLocalSecretsConfig } from "../src/main/secrets/config.js";
import { crossSiteDialog, importDialog, proceeds } from "../src/main/secrets/dialogs.js";
import { originOf, sameOrigin } from "../src/main/secrets/origins.js";
import { Secret, isZeroed, zero } from "../src/main/secrets/secret.js";
import { PasswordVault, VAULT_FILE, VaultError } from "../src/main/secrets/vault.js";
import type { SecretCipher } from "../src/main/token-store.js";

const CONFIG = loadLocalSecretsConfig(join(__dirname, "..", "..", "..", "config", "local-secrets.yaml"));
const PASSWORD = "c0rrect-h0rse-battery-staple";
const dirs: string[] = [];

afterEach(() => {
  for (const dir of dirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

function directory(): string {
  const dir = mkdtempSync(join(tmpdir(), "quansio-vault-"));
  dirs.push(dir);
  return dir;
}

/** A reversible, non-identity transformation standing in for the keychain-backed cipher. */
function cipher(available = true, key = 0x5a): SecretCipher & { calls: number } {
  const state = { calls: 0 };
  return Object.assign(state, {
    isEncryptionAvailable: () => available,
    encryptString: (text: string) => {
      state.calls += 1;
      return Buffer.from(Buffer.from(text, "utf8").map((byte) => byte ^ key));
    },
    decryptString: (bytes: Buffer) => {
      const plain = Buffer.from(bytes.map((byte) => byte ^ key)).toString("utf8");
      JSON.parse(plain);
      return plain;
    },
  });
}

function vault(dir: string, c: SecretCipher = cipher(), config = CONFIG): PasswordVault {
  let counter = 0;
  return new PasswordVault({ path: join(dir, VAULT_FILE), cipher: c, config, now: () => 1_000 + counter, newId: () => `pw_${String((counter += 1)).padStart(32, "0")}` });
}

describe("origins", () => {
  it("are exact on scheme, host and port, and a subdomain is not its parent", () => {
    expect(originOf("https://Example.com/path?q=1#x")).toBe("https://example.com");
    expect(originOf("https://example.com:8443/")).toBe("https://example.com:8443");
    expect(sameOrigin("https://example.com/a", "https://example.com/b")).toBe(true);
    for (const other of ["http://example.com", "https://example.com:8443", "https://login.example.com", "https://example.com.evil.test"]) {
      expect(sameOrigin("https://example.com", other), other).toBe(false);
    }
    for (const notWeb of ["ftp://example.com", "javascript:alert(1)", "file:///etc/passwd", "about:blank", "https://user:pw@example.com", "example.com", ""]) {
      expect(originOf(notWeb), notWeb).toBeNull();
      expect(sameOrigin(notWeb, notWeb), `${notWeb} matches nothing, not even itself`).toBe(false);
    }
  });
});

describe("a secret", () => {
  it("has no rendering: formatting, serializing and inspecting it all say redacted", () => {
    const secret = new Secret(PASSWORD);
    for (const text of [String(secret), JSON.stringify({ secret }), inspect(secret), JSON.stringify([secret])]) {
      expect(text).not.toContain(PASSWORD);
      expect(text).toContain("redacted");
    }
    expect(secret.expose()).toBe(PASSWORD);
    expect(secret.length).toBe(PASSWORD.length);
  });

  it("is overwritten by wipe, and a wiped secret gives nothing", () => {
    const secret = new Secret(PASSWORD);
    secret.wipe();
    expect(secret.wiped).toBe(true);
    expect(secret.length).toBe(0);
    expect(() => secret.expose()).toThrow(/wiped/);
    const key = Buffer.from("derived-key-bytes");
    expect(isZeroed(key)).toBe(false);
    expect(isZeroed(zero(key))).toBe(true);
  });
});

describe("the vault", () => {
  it("keeps metadata in every list and the password only for the fill path", () => {
    const dir = directory();
    const v = vault(dir);
    const saved = v.save({ origin: "https://example.com/login", username: "ada", password: PASSWORD });
    expect(saved).toMatchObject({ origin: "https://example.com", username: "ada" });
    for (const view of [v.list(), v.suggest("https://example.com/other"), v.get(saved.id), saved]) {
      expect(JSON.stringify(view)).not.toContain(PASSWORD);
    }
    const password = v.password(saved.id);
    expect(password.expose()).toBe(PASSWORD);
    expect(String(password)).toBe("[redacted]");
    password.wipe();
  });

  it("writes an encrypted private file, never the password, and replaces the login for the same origin and user", () => {
    const dir = directory();
    const c = cipher();
    const v = vault(dir, c);
    v.save({ origin: "https://example.com", username: "ada", password: PASSWORD });
    v.save({ origin: "https://example.com", username: "ada", password: "a-newer-password" });
    expect(v.count()).toBe(1);
    const path = join(dir, VAULT_FILE);
    const bytes = readFileSync(path);
    expect(bytes.includes(Buffer.from(PASSWORD))).toBe(false);
    expect(bytes.includes(Buffer.from("a-newer-password"))).toBe(false);
    expect(bytes.includes(Buffer.from("ada"))).toBe(false);
    expect(statSync(path).mode & 0o777).toBe(0o600);
    expect(existsSync(`${path}.tmp`)).toBe(false);
    // A fresh vault over the same file reads it back through the cipher.
    const again = vault(dir, c);
    expect(again.list().map((entry) => entry.username)).toEqual(["ada"]);
    expect(again.password(again.list()[0]?.id ?? "").expose()).toBe("a-newer-password");
  });

  it("offers a login to its own origin only", () => {
    const v = vault(directory());
    v.save({ origin: "https://example.com", username: "ada", password: PASSWORD });
    v.save({ origin: "https://example.com", username: "grace", password: PASSWORD });
    v.save({ origin: "https://login.example.com", username: "ada", password: PASSWORD });
    expect(v.suggest("https://example.com/checkout").map((entry) => entry.username).sort()).toEqual(["ada", "grace"]);
    expect(v.suggest("https://login.example.com").map((entry) => entry.origin)).toEqual(["https://login.example.com"]);
    for (const page of ["http://example.com", "https://example.com:8443", "https://example.com.evil.test", "about:blank", "javascript:1", ""]) {
      expect(v.suggest(page), page).toEqual([]);
    }
  });

  it("saves nothing when the platform cannot encrypt: no file, no plaintext", () => {
    const dir = directory();
    const v = vault(dir, cipher(false));
    expect(v.available()).toBe(false);
    expect(() => v.save({ origin: "https://example.com", username: "ada", password: PASSWORD })).toThrow(VaultError);
    expect(existsSync(join(dir, VAULT_FILE))).toBe(false);
    expect(v.list()).toEqual([]);
  });

  it("never overwrites a file it cannot read: it is locked, reads are empty, writes are refused", () => {
    const dir = directory();
    const path = join(dir, VAULT_FILE);
    const first = vault(dir, cipher(true, 0x5a));
    first.save({ origin: "https://example.com", username: "ada", password: PASSWORD });
    const before = readFileSync(path);
    // Another keychain: the file does not decrypt.
    const other = vault(dir, cipher(true, 0x33));
    expect(other.locked()).toBe(true);
    expect(other.list()).toEqual([]);
    expect(() => other.save({ origin: "https://example.com", username: "grace", password: "x-y-z-1" })).toThrow(/could not be read/);
    expect(() => other.remove("pw_x")).toThrow(VaultError);
    expect(readFileSync(path).equals(before)).toBe(true);
    // A truncated file is locked too, and is not replaced.
    writeFileSync(path, before.subarray(0, 5));
    const truncated = vault(dir, cipher());
    expect(truncated.locked()).toBe(true);
    expect(() => truncated.save({ origin: "https://example.com", username: "grace", password: "x-y-z-1" })).toThrow(VaultError);
    expect(readFileSync(path).length).toBe(5);
  });

  it("refuses what breaks its bounds", () => {
    const v = vault(directory(), cipher(), { ...CONFIG, passwordsMax: 2, fieldMaxChars: 16 });
    const refused = (input: { origin: string; username: string; password: string }): string => {
      try {
        v.save(input);
      } catch (error) {
        return error instanceof VaultError ? error.reason : "other";
      }
      return "saved";
    };
    expect(refused({ origin: "ftp://example.com", username: "ada", password: "pw" })).toBe("invalid_origin");
    expect(refused({ origin: "https://example.com", username: "", password: "pw" })).toBe("empty");
    expect(refused({ origin: "https://example.com", username: "ada", password: "" })).toBe("empty");
    expect(refused({ origin: "https://example.com", username: "ada", password: "x".repeat(17) })).toBe("too_long");
    expect(refused({ origin: "https://a.example", username: "ada", password: "pw" })).toBe("saved");
    expect(refused({ origin: "https://b.example", username: "ada", password: "pw" })).toBe("saved");
    expect(refused({ origin: "https://c.example", username: "ada", password: "pw" })).toBe("too_many");
    expect(v.count()).toBe(2);
  });

  it("removes a login and forgets everything on clear", () => {
    const dir = directory();
    const v = vault(dir);
    const saved = v.save({ origin: "https://example.com", username: "ada", password: PASSWORD });
    expect(v.remove(saved.id)).toBe(true);
    expect(v.remove(saved.id)).toBe(false);
    expect(() => v.password(saved.id)).toThrow(VaultError);
    v.save({ origin: "https://example.com", username: "ada", password: PASSWORD });
    v.clear();
    expect(existsSync(join(dir, VAULT_FILE))).toBe(false);
    expect(v.list()).toEqual([]);
  });
});

describe("the questions", () => {
  it("default to Cancel and name origins or counts, never a value", () => {
    const cross = crossSiteDialog({ entryOrigin: "https://example.com", username: "ada", pageOrigin: "https://evil.test" });
    expect(cross.defaultId).toBe(0);
    expect(cross.cancelId).toBe(0);
    expect(proceeds(cross.defaultId)).toBe(false);
    expect(cross.message).toContain("https://example.com");
    expect(cross.message).toContain("https://evil.test");
    const logins = importDialog({ kind: "logins", browser: "Google Chrome", profile: "Person 1", count: 12, domains: [], moreDomains: 0 });
    const cookies = importDialog({ kind: "cookies", browser: "Google Chrome", profile: "Person 1", count: 300, domains: ["a.example", "b.example"], moreDomains: 4 });
    for (const box of [logins, cookies]) {
      expect(box.defaultId).toBe(0);
      expect(box.cancelId).toBe(0);
      expect(box.buttons[0]).toBe("Cancel");
      expect(proceeds(box.cancelId)).toBe(false);
    }
    expect(logins.message).toContain("12");
    expect(cookies.message).toContain("300");
    expect(cookies.message).toContain("a.example");
    expect(cookies.message).toContain("4 more");
    expect(proceeds(1)).toBe(true);
  });
});
