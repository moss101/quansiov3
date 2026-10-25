/**
 * Importing from another browser (ADP-009, DOMAIN.md §13.9 B), against a profile a **real Google Chrome** wrote.
 *
 * Chrome is launched headless on a temporary profile with its mock keychain, given cookies through its own DevTools protocol and closed;
 * the reader then works on the cookie database Chrome itself encrypted (key from PBKDF2 over the mock keychain's `mock_password`,
 * `v10` frames, the host-key digest in front of the plaintext). The saved-login database is built by the test in Chromium's `logins`
 * shape with the same encryption (Chrome's password store has no protocol call for adding a login).
 *
 * The real macOS keychain is not touched here: its prompt belongs to a person, so that test is `BLOCKED_EXTERNAL` unless
 * `QUANSIO_TEST_MACOS_KEYCHAIN_IMPORT=1` is set on a machine where someone will answer it.
 *
 * Without Chrome the cookie tests print `BLOCKED_EXTERNAL` and return.
 */

import { createCipheriv, createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";

import { bundleDigest } from "../src/main/secrets/bundle.js";
import {
  FAMILIES,
  ImportError,
  KeychainKeys,
  decryptValue,
  deriveKey,
  discoverProfiles,
  familyOf,
  supportDirectory,
  type BundleCookie,
} from "../src/main/secrets/chromium.js";
import { isZeroed } from "../src/main/secrets/secret.js";

import { chromePath, makeChromeHome, MOCK_KEYCHAIN_PASSWORD, type Fixture, type FixtureCookie } from "./chrome-profile-fixture.js";
import { RecordedKeys, makeRig, type Rig } from "./secrets-rig.js";

const SESSION_VALUE = "s3ss10n-canary-7f3a";
const PREFS_VALUE = "dark";
const OTHER_VALUE = "0th3r-canary-55aa";
const EXPIRES = Math.floor(Date.now() / 1000) + 86_400 * 30;

const COOKIES: readonly FixtureCookie[] = [
  { name: "session", value: SESSION_VALUE, domain: "example.test", path: "/", secure: true, httpOnly: true, sameSite: "Lax", expires: EXPIRES },
  { name: "prefs", value: PREFS_VALUE, domain: ".example.test", path: "/app", secure: false, httpOnly: false, sameSite: "Strict", expires: EXPIRES },
  { name: "other", value: OTHER_VALUE, domain: "other.test", path: "/", secure: false, httpOnly: false, sameSite: "Lax", expires: EXPIRES },
];

let fixture: Fixture | null = null;
let missing = false;
let foreign = false;

beforeAll(async () => {
  // The reader decrypts with macOS Safe Storage's key (PBKDF2, 1003 rounds over the keychain's password). Chrome on another
  // platform encrypts with its own key (1 round over "peanuts" with the basic password store), so a profile it wrote is
  // not one this reader is meant to open: the real-Chrome cookie tests are for the macOS host.
  if (process.platform !== "darwin") {
    foreign = true;
    return;
  }
  if (chromePath() === null) {
    missing = true;
    return;
  }
  fixture = await makeChromeHome(COOKIES);
}, 60_000);

afterAll(() => {
  fixture?.cleanup();
});

const rigs: Rig[] = [];
afterEach(() => {
  for (const rig of rigs.splice(0)) {
    rig.cleanup();
  }
});

function withChrome(): Fixture | null {
  if (fixture === null) {
    const why = foreign ? "this is not a macOS host, so Chrome would encrypt with a key the macOS reader does not use" : missing ? "no Chrome on this host" : "the Chrome profile fixture could not be made";
    console.log(`BLOCKED_EXTERNAL: ${why}; the cookie import tests did not run`);
  }
  return fixture;
}

function rig(options: Parameters<typeof makeRig>[0] = {}): Rig {
  const made = makeRig(options);
  rigs.push(made);
  return made;
}

const leftovers = (): string[] => readdirSync(tmpdir()).filter((name) => name.startsWith("quansio-import-"));

describe("a profile a real Chrome wrote", () => {
  it("is found by the scan, which counts cookies per domain without a key or a prompt", async () => {
    const f = withChrome();
    if (f === null) {
      return;
    }
    const r = rig({ home: f.home });
    const before = leftovers();
    const answer = await r.call("import.scan", r.main);
    expect(answer.ok, JSON.stringify(answer)).toBe(true);
    const profiles = answer.result?.["profiles"] as { family: string; profile: string; cookies: { total: number; domains: { domain: string; count: number }[] } | null }[];
    expect(profiles.map((profile) => `${profile.family}/${profile.profile}`)).toEqual(["chrome/Default"]);
    expect(profiles[0]?.cookies?.total).toBe(3);
    expect(profiles[0]?.cookies?.domains).toEqual([
      { domain: "example.test", count: 2 },
      { domain: "other.test", count: 1 },
    ]);
    expect(r.keys.asked, "counting needs no key and never touches the keychain").toEqual([]);
    expect(r.asked).toEqual([]);
    expect(JSON.stringify(answer)).not.toContain(SESSION_VALUE);
    expect(leftovers(), "the private copy of the database is deleted").toEqual(before);
  });

  it("stages the chosen domains: asked first, decrypted with the person's key, sent with its digest, and never echoed", async () => {
    const f = withChrome();
    if (f === null) {
      return;
    }
    const r = rig({ home: f.home });
    r.answers.push(1);
    const answer = await r.call("import.cookies", r.main, { family: "chrome", profile: "Default", domains: ["example.test"] });
    expect(answer.ok, JSON.stringify(answer)).toBe(true);
    // The question named the browser, the count and the domain, with Cancel as the default.
    expect(r.asked).toHaveLength(1);
    expect(r.asked[0]?.message).toContain("2 cookies");
    expect(r.asked[0]?.message).toContain("example.test");
    expect(r.asked[0]?.message).toContain("Google Chrome");
    expect(r.asked[0]?.defaultId).toBe(0);
    // The key was asked for once, after the yes, and is zeroed.
    expect(r.keys.asked).toEqual(["chrome"]);
    expect(r.keys.issued.every(isZeroed)).toBe(true);
    // One command with the cookies exactly as Chrome held them and the digest of that bundle.
    expect(r.invoked).toHaveLength(1);
    const sent = r.invoked[0];
    expect(sent?.name).toBe("ImportBrowserSession");
    const cookies = sent?.params["cookies"] as BundleCookie[];
    expect(cookies.map((cookie) => cookie.name).sort()).toEqual(["prefs", "session"]);
    const session = cookies.find((cookie) => cookie.name === "session");
    expect(session).toMatchObject({ value: SESSION_VALUE, domain: "example.test", path: "/", secure: true, http_only: true, same_site: "lax" });
    expect(session?.expires).toBe(EXPIRES);
    const prefs = cookies.find((cookie) => cookie.name === "prefs");
    expect(prefs).toMatchObject({ value: PREFS_VALUE, domain: ".example.test", path: "/app", secure: false, http_only: false, same_site: "strict" });
    expect(cookies.some((cookie) => cookie.domain.includes("other.test")), "a domain that was not chosen is not read").toBe(false);
    expect(sent?.params["digest"]).toBe(bundleDigest(cookies));
    expect(sent?.params["workspace_id"]).toBe("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    // The answer to the page carries counts and times, never a value.
    for (const value of [SESSION_VALUE, PREFS_VALUE, OTHER_VALUE]) {
      expect(JSON.stringify(answer)).not.toContain(value);
    }
    expect(answer.result?.["cookieCount"]).toBe(2);
  });

  it("treats a no as nothing read, and a yes to cookies as a yes to cookies only", async () => {
    const f = withChrome();
    if (f === null) {
      return;
    }
    const r = rig({ home: f.home });
    const declined = await r.call("import.cookies", r.main, { family: "chrome", profile: "Default", domains: ["example.test"] });
    expect(declined).toMatchObject({ ok: false, error: { code: "POLICY_DENIED", details: { reason: "declined" } } });
    expect(r.keys.asked).toEqual([]);
    expect(r.invoked).toEqual([]);
    expect(r.asked[0]?.title).toBe("Stage cookies for the agent's browser?");
    r.answers.push(1);
    expect((await r.call("import.cookies", r.main, { family: "chrome", profile: "Default", domains: ["example.test"] })).ok).toBe(true);
    // The yes covered cookies. Logins are another type with another question; this profile holds none, so nothing is asked or read.
    const logins = await r.call("import.logins", r.main, { family: "chrome", profile: "Default" });
    expect(logins).toMatchObject({ ok: true, result: { imported: 0, skipped: 0 } });
    expect(r.asked).toHaveLength(2);
    expect(r.keys.asked, "the key was asked for once, for the cookies").toEqual(["chrome"]);
  });

  it("refuses what the scan did not list: a profile, a domain, a traversal, and an over-large choice", async () => {
    const f = withChrome();
    if (f === null) {
      return;
    }
    const r = rig({ home: f.home, config: { importMaxCookies: 1 } });
    const stage = (payload: unknown): Promise<unknown> => r.call("import.cookies", r.main, payload);
    expect(await stage({ family: "chrome", profile: "Profile 9", domains: ["example.test"] })).toMatchObject({ ok: false, error: { code: "NOT_FOUND" } });
    expect(await stage({ family: "brave", profile: "Default", domains: ["example.test"] })).toMatchObject({ ok: false, error: { code: "NOT_FOUND" } });
    expect(await stage({ family: "chrome", profile: "Default", domains: ["nowhere.test"] })).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    expect(await stage({ family: "chrome", profile: "Default", domains: [] })).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    for (const bad of [{ family: "chrome", profile: "../../etc", domains: ["a"] }, { family: "../chrome", profile: "Default", domains: ["a"] }, { family: "chrome", profile: "Default", domains: ["a"], path: "/etc" }, { family: "chrome", profile: "Default" }]) {
      expect(await stage(bad)).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    }
    // example.test holds two cookies; this configuration carries one.
    expect(await stage({ family: "chrome", profile: "Default", domains: ["example.test"] })).toMatchObject({ ok: false, error: { details: { reason: "too_many" } } });
    expect(r.asked, "nothing refused before the question was asked a question").toEqual([]);
    expect(r.invoked).toEqual([]);
  });

  it("imports nothing, and says why, when the keychain does not give out the key or gives out another", async () => {
    const f = withChrome();
    if (f === null) {
      return;
    }
    const denied = rig({ home: f.home });
    denied.keys.deny = true;
    denied.answers.push(1);
    expect(await denied.call("import.cookies", denied.main, { family: "chrome", profile: "Default", domains: ["example.test"] })).toMatchObject({ ok: false, error: { code: "POLICY_DENIED", details: { reason: "keychain_denied" } } });
    expect(denied.invoked).toEqual([]);

    const wrong = rig({ home: f.home, keys: new RecordedKeys("not-the-password") });
    wrong.answers.push(1);
    expect(await wrong.call("import.cookies", wrong.main, { family: "chrome", profile: "Default", domains: ["example.test"] })).toMatchObject({ ok: false, error: { code: "NOT_FOUND", details: { reason: "nothing_readable" } } });
    expect(wrong.invoked).toEqual([]);
    expect(wrong.keys.issued.every(isZeroed)).toBe(true);
  });

  it("skips what has expired, and never opens the browser's own file for writing", async () => {
    const f = withChrome();
    if (f === null) {
      return;
    }
    const r = rig({ home: f.home });
    const database = join(f.home, "Library", "Application Support", "Google", "Chrome", "Default", "Cookies");
    const target = [join(database), join(f.home, "Library", "Application Support", "Google", "Chrome", "Default", "Network", "Cookies")].find((path) => {
      try {
        readFileSync(path);
        return true;
      } catch {
        return false;
      }
    });
    expect(target).toBeDefined();
    const before = createHash("sha256").update(readFileSync(target ?? "")).digest("hex");
    // A day past the cookies' expiry, every one is expired.
    r.clock.now = (EXPIRES + 86_400) * 1000;
    r.answers.push(1);
    const answer = await r.call("import.cookies", r.main, { family: "chrome", profile: "Default", domains: ["example.test"] });
    expect(answer).toMatchObject({ ok: false, error: { details: { reason: "nothing_readable" } } });
    expect(createHash("sha256").update(readFileSync(target ?? "")).digest("hex")).toBe(before);
  });

  it("is switched off by configuration: nothing is scanned, asked or read", async () => {
    const f = withChrome();
    if (f === null) {
      return;
    }
    const r = rig({ home: f.home, config: { importEnabled: false } });
    for (const [channel, payload] of [
      ["import.scan", {}],
      ["import.logins", { family: "chrome", profile: "Default" }],
      ["import.cookies", { family: "chrome", profile: "Default", domains: ["example.test"] }],
    ] as const) {
      expect(await r.call(channel, r.main, payload)).toMatchObject({ ok: false, error: { code: "POLICY_DENIED" } });
    }
    expect(r.asked).toEqual([]);
    expect(r.keys.asked).toEqual([]);
  });
});

// ------------------------------------------------------------------ saved logins

const { DatabaseSync } = createRequire(import.meta.url)("node:sqlite") as typeof import("node:sqlite");
const IV = Buffer.alloc(16, 0x20);

function encrypt(plain: string, key: Buffer): Buffer {
  const cipher = createCipheriv("aes-128-cbc", key, IV);
  return Buffer.concat([Buffer.from("v10"), cipher.update(plain, "utf8"), cipher.final()]);
}

/** A home with a Chrome profile whose `Login Data` is in Chromium's `logins` shape, encrypted with the mock keychain's key. */
function homeWithLogins(rows: readonly { origin: string; username: string; password: string | Buffer; blocked?: number }[]): string {
  const home = mkdtempSync(join(tmpdir(), "quansio-logins-"));
  const profile = join(supportDirectory(home), "Google", "Chrome", "Default");
  mkdirSync(profile, { recursive: true });
  const database = new DatabaseSync(join(profile, "Login Data"));
  database.exec("CREATE TABLE logins (origin_url VARCHAR NOT NULL, username_value VARCHAR, password_value BLOB, blacklisted_by_user INTEGER NOT NULL)");
  const key = deriveKey(MOCK_KEYCHAIN_PASSWORD);
  const insert = database.prepare("INSERT INTO logins (origin_url, username_value, password_value, blacklisted_by_user) VALUES (?, ?, ?, ?)");
  for (const row of rows) {
    insert.run(row.origin, row.username, typeof row.password === "string" ? encrypt(row.password, key) : row.password, row.blocked ?? 0);
  }
  database.close();
  return home;
}

const homes: string[] = [];
afterAll(() => {
  for (const home of homes.splice(0)) {
    rmSync(home, { recursive: true, force: true });
  }
});

function loginHome(rows: Parameters<typeof homeWithLogins>[0]): string {
  const home = homeWithLogins(rows);
  homes.push(home);
  return home;
}

describe("saved logins", () => {
  const PASSWORD = "imp0rted-p4ssw0rd";

  it("go into the password manager on this device, after their own question, and into nothing else", async () => {
    const home = loginHome([
      { origin: "https://example.com/login", username: "ada", password: PASSWORD },
      { origin: "https://shop.example", username: "grace", password: "an0ther-p4ss" },
      { origin: "https://blocked.example", username: "mallory", password: "never", blocked: 1 },
      { origin: "https://broken.example", username: "eve", password: Buffer.from("not an encrypted frame") },
      { origin: "ftp://files.example", username: "frank", password: "ftp-p4ss" },
    ]);
    const r = rig({ home });
    expect((await r.call("import.scan", r.main)).result?.["profiles"]).toMatchObject([{ family: "chrome", profile: "Default", logins: 4 }]);
    r.answers.push(1);
    const answer = await r.call("import.logins", r.main, { family: "chrome", profile: "Default" });
    expect(answer).toMatchObject({ ok: true, result: { imported: 2, skipped: 2 } });
    expect(r.asked).toHaveLength(1);
    expect(r.asked[0]?.title).toBe("Import saved logins?");
    expect(r.asked[0]?.message).toContain("4 saved logins");
    expect(r.asked[0]?.defaultId).toBe(0);
    expect(r.keys.issued.every(isZeroed)).toBe(true);
    // The vault holds them, as metadata for the page and as passwords only for the fill path.
    const listed = await r.call("passwords.list", r.main);
    expect((listed.result?.["entries"] as { username: string }[]).map((entry) => entry.username).sort()).toEqual(["ada", "grace"]);
    for (const text of [JSON.stringify(answer), JSON.stringify(listed)]) {
      expect(text).not.toContain(PASSWORD);
      expect(text).not.toContain("an0ther-p4ss");
    }
    const ada = r.composed.vault.suggest("https://example.com")[0];
    expect(r.composed.vault.password(ada?.id ?? "").expose()).toBe(PASSWORD);
    // Nothing went to the server: logins stay on the device.
    expect(r.invoked).toEqual([]);
  });

  it("asks the keychain nothing when the person says no, and does nothing where the vault cannot keep them", async () => {
    const home = loginHome([{ origin: "https://example.com", username: "ada", password: PASSWORD }]);
    const declined = rig({ home });
    expect(await declined.call("import.logins", declined.main, { family: "chrome", profile: "Default" })).toMatchObject({ ok: false, error: { details: { reason: "declined" } } });
    expect(declined.keys.asked).toEqual([]);

    const unencryptable = rig({ home, cipher: { isEncryptionAvailable: () => false, encryptString: () => Buffer.alloc(0), decryptString: () => "" } });
    unencryptable.answers.push(1);
    expect(await unencryptable.call("import.logins", unencryptable.main, { family: "chrome", profile: "Default" })).toMatchObject({ ok: false, error: { code: "ADAPTER_UNAVAILABLE" } });
    expect(unencryptable.asked, "no question for something that cannot be kept").toEqual([]);
    expect(unencryptable.keys.asked).toEqual([]);
  });

  it("refuses a vault it cannot read rather than replacing it", async () => {
    const home = loginHome([{ origin: "https://example.com", username: "ada", password: PASSWORD }]);
    const r = rig({ home });
    writeFileSync(join(r.userData, "password-vault.bin"), Buffer.from("not this device's file"));
    // A new composition over the same user data directory finds the file unreadable.
    const again = rig({ home });
    writeFileSync(join(again.userData, "password-vault.bin"), Buffer.from("not this device's file"));
    again.answers.push(1);
    expect(await again.call("import.logins", again.main, { family: "chrome", profile: "Default" })).toMatchObject({ ok: false, error: { code: "CONFLICT_STATE" } });
    expect(readFileSync(join(again.userData, "password-vault.bin")).toString()).toBe("not this device's file");
  });
});

// ------------------------------------------------------------------ the pieces

describe("the Safe Storage key and values", () => {
  it("is read with the system's tool by the person's own prompt, trimmed, derived and zeroed", async () => {
    const calls: { file: string; args: readonly string[] }[] = [];
    const password = Buffer.from(`${MOCK_KEYCHAIN_PASSWORD}\n`);
    const keys = new KeychainKeys((file, args) => {
      calls.push({ file, args });
      return Promise.resolve(password);
    });
    const chrome = familyOf("chrome") ?? FAMILIES[0];
    if (chrome === undefined) {
      throw new Error("no family");
    }
    const key = await keys.key(chrome);
    expect(calls).toEqual([{ file: "/usr/bin/security", args: ["find-generic-password", "-w", "-s", "Chrome Safe Storage", "-a", "Chrome"] }]);
    expect(key.equals(deriveKey(MOCK_KEYCHAIN_PASSWORD))).toBe(true);
    expect(isZeroed(password), "the buffer the tool answered in is zeroed").toBe(true);
    const refused = new KeychainKeys(() => Promise.reject(new Error("User canceled the authorization")));
    await expect(refused.key(chrome)).rejects.toMatchObject({ reason: "keychain_denied" });
    await expect(refused.key(chrome)).rejects.toBeInstanceOf(ImportError);
  });

  it("is the real keychain's only on a machine where someone will answer its prompt", async () => {
    if (process.env["QUANSIO_TEST_MACOS_KEYCHAIN_IMPORT"] !== "1") {
      console.log("BLOCKED_EXTERNAL: QUANSIO_TEST_MACOS_KEYCHAIN_IMPORT=1 is not set; the real keychain prompt (a person's to answer) was not exercised");
      return;
    }
    const family = FAMILIES[0];
    expect(family).toBeDefined();
    const key = await new KeychainKeys().key(family ?? FAMILIES[1] ?? (() => { throw new Error("no family"); })());
    expect(key.length).toBe(16);
  });

  it("decrypts a v10 or v11 frame, strips a matching host digest only, and answers null for anything else", () => {
    const key = deriveKey(MOCK_KEYCHAIN_PASSWORD);
    const host = ".example.test";
    const withHost = (plain: string): Buffer => encrypt(`${createHash("sha256").update(host).digest().toString("latin1")}${plain}`, key);
    expect(decryptValue(encrypt("plain-value", key), key, null)?.toString()).toBe("plain-value");
    const v11 = encrypt("v11-value", key);
    v11.write("v11", 0, "latin1");
    expect(decryptValue(v11, key, null)?.toString()).toBe("v11-value");
    // A cookie newer Chrome wrote: the digest of its host key leads the plaintext.
    const stripped = decryptValue(Buffer.concat([Buffer.from("v10"), encrypt("", key).subarray(3)]), key, host);
    expect(stripped === null || stripped.length === 0).toBe(true);
    const hashed = Buffer.from(createHash("sha256").update(host).digest());
    const cipher = createCipheriv("aes-128-cbc", key, IV);
    const framed = Buffer.concat([Buffer.from("v10"), cipher.update(Buffer.concat([hashed, Buffer.from("cookie-value")])), cipher.final()]);
    expect(decryptValue(framed, key, host)?.toString()).toBe("cookie-value");
    expect(decryptValue(framed, key, ".another.test")?.length, "a digest for another host is not stripped").toBe(44);
    expect(withHost("x").length).toBeGreaterThan(0);
    for (const bad of [Buffer.alloc(0), Buffer.from("v10"), Buffer.from("plaintext-password!!"), Buffer.concat([Buffer.from("v10"), Buffer.alloc(15)]), Buffer.concat([Buffer.from("v10"), Buffer.alloc(16, 7)])]) {
      expect(decryptValue(bad, key, null)).toBeNull();
    }
    expect(decryptValue(encrypt("x", key), deriveKey("not-the-password"), null)).toBeNull();
  });

  it("discovers only profiles under the browsers' own folders", () => {
    const home = mkdtempSync(join(tmpdir(), "quansio-discover-"));
    homes.push(home);
    const support = supportDirectory(home);
    for (const profile of ["Default", "Profile 2", "Profile X", "System Profile", "Crashpad"]) {
      mkdirSync(join(support, "Google", "Chrome", profile, "Network"), { recursive: true });
      writeFileSync(join(support, "Google", "Chrome", profile, "Network", "Cookies"), "");
    }
    mkdirSync(join(support, "BraveSoftware", "Brave-Browser", "Default"), { recursive: true });
    writeFileSync(join(support, "BraveSoftware", "Brave-Browser", "Default", "Login Data"), "");
    writeFileSync(join(support, "Google", "Chrome", "Local State"), JSON.stringify({ profile: { info_cache: { Default: { name: "Work" }, "Profile 2": { name: 5 } } } }));
    const found = discoverProfiles(support);
    expect(found.map((profile) => `${profile.family}/${profile.profile}/${profile.displayName}`)).toEqual(["chrome/Default/Work", "chrome/Profile 2/Profile 2", "brave/Default/Default"]);
    expect(discoverProfiles(join(home, "nothing"))).toEqual([]);
  });
});

describe("a bundle's digest", () => {
  const golden: BundleCookie[] = [
    { name: "session", value: 'v"a\\l/ü€\u{1F600}', domain: ".example.test", path: "/app", secure: true, http_only: true, same_site: "strict", expires: 1_900_000_000 },
    { name: "prefs", value: "dark", domain: "example.test", path: "/", secure: false, http_only: false, same_site: "unspecified", expires: null },
    { name: "a", value: "", domain: "é.test", path: "/", secure: false, http_only: false, same_site: "none", expires: 0 },
  ];

  it("is the digest the runtime computes for the same bundle (crates/machine/src/secrets/imports.rs holds the same value)", () => {
    expect(bundleDigest(golden)).toBe("ceda87a0e712adc728b9002e8bffd7a1f5e040e6cc8d96c6b4a4595b437cb2d5");
  });

  it("does not depend on the order cookies were read in, and changes with any value", () => {
    expect(bundleDigest([...golden].reverse())).toBe(bundleDigest(golden));
    const changed = golden.map((cookie, index) => (index === 1 ? { ...cookie, value: "darker" } : cookie));
    expect(bundleDigest(changed)).not.toBe(bundleDigest(golden));
  });
});
