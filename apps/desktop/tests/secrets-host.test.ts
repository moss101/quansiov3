/**
 * The password manager's channels, and the structure that keeps it a person's tool (ADP-009, DOMAIN.md §13.9 A).
 *
 * Through the real guarded IPC: validation, the main window's monopoly, the switch, and what an answer may carry. Then source scans, because
 * the properties are about what the code contains: one place decrypts a saved password, no agent tool or server command names the vault,
 * and no page file other than the settings page saves one.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { SECRET_CHANNELS } from "../src/preload/bridge.js";

import { makeRig, type Rig } from "./secrets-rig.js";

const ROOT = join(__dirname, "..", "..", "..");
const PASSWORD = "c0rrect-h0rse-battery-staple";
const rigs: Rig[] = [];

afterEach(() => {
  for (const rig of rigs.splice(0)) {
    rig.cleanup();
  }
});

function rig(options: Parameters<typeof makeRig>[0] = {}): Rig {
  const made = makeRig(options);
  rigs.push(made);
  return made;
}

describe("the password manager's channels", () => {
  it("keep logins, and no answer carries a password", async () => {
    const r = rig();
    const saved = await r.call("passwords.save", r.main, { origin: "https://example.com/login", username: "ada", password: PASSWORD });
    expect(saved).toMatchObject({ ok: true, result: { entry: { origin: "https://example.com", username: "ada" } } });
    const id = (saved.result?.["entry"] as { id: string }).id;
    const answers = [
      saved,
      await r.call("passwords.status", r.main),
      await r.call("passwords.list", r.main),
      await r.call("passwords.suggest", r.main, { pageUrl: "https://example.com/checkout" }),
      await r.call("passwords.suggest", r.main, { pageUrl: "https://evil.test/" }),
    ];
    expect(JSON.stringify(answers)).not.toContain(PASSWORD);
    expect(answers[1]).toMatchObject({ ok: true, result: { available: true, locked: false, count: 1 } });
    expect((answers[3]?.result?.["entries"] as unknown[]).length).toBe(1);
    expect((answers[4]?.result?.["entries"] as unknown[]).length).toBe(0);
    expect(await r.call("passwords.remove", r.main, { id })).toMatchObject({ ok: true, result: { removed: true } });
    expect((await r.call("passwords.list", r.main)).result?.["entries"]).toEqual([]);
  });

  it("validate every payload and refuse a field they do not know", async () => {
    const r = rig();
    const good = { origin: "https://example.com", username: "ada", password: PASSWORD };
    for (const bad of [{}, { ...good, extra: 1 }, { ...good, password: "" }, { ...good, username: 5 }, { ...good, origin: "" }, { ...good, password: "x".repeat(5000) }, null, "text"]) {
      const answer = await r.call("passwords.save", r.main, bad);
      expect(answer, JSON.stringify(bad).slice(0, 60)).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
      expect(JSON.stringify(answer)).not.toContain(PASSWORD);
    }
    expect(await r.call("passwords.remove", r.main, { id: "not-an-id" })).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    expect(await r.call("passwords.suggest", r.main, {})).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    expect(await r.call("passwords.status", r.main, { extra: true })).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    expect((await r.call("passwords.list", r.main)).result?.["entries"]).toEqual([]);
  });

  it("are refused to every window but the main one, and the refusal carries nothing", async () => {
    const r = rig();
    await r.call("passwords.save", r.main, { origin: "https://example.com", username: "ada", password: PASSWORD });
    const payloads: Record<string, unknown> = {
      "passwords.status": {},
      "passwords.list": {},
      "passwords.suggest": { pageUrl: "https://example.com" },
      "passwords.save": { origin: "https://example.com", username: "grace", password: PASSWORD },
      "passwords.remove": { id: "pw_".padEnd(35, "0") },
      "passwords.fill": { id: "pw_".padEnd(35, "0"), sessionId: "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" },
      "import.scan": {},
      "import.logins": { family: "chrome", profile: "Default" },
      "import.cookies": { family: "chrome", profile: "Default", domains: ["example.test"] },
    };
    expect(Object.keys(payloads).sort()).toEqual([...SECRET_CHANNELS].sort());
    for (const channel of SECRET_CHANNELS) {
      for (const window of [r.popout, r.quick, r.indicator]) {
        const answer = await r.call(channel, window, payloads[channel]);
        expect(answer, `${window.type} → ${channel}`).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
        expect(JSON.stringify(answer)).not.toContain("ada");
      }
    }
    expect(r.composed.vault.count(), "nothing a refused window sent was saved").toBe(1);
  });

  it("answer POLICY_DENIED for everything when the switch is off, and touch nothing", async () => {
    const r = rig({ config: { enabled: false } });
    for (const channel of SECRET_CHANNELS) {
      const answer = await r.call(channel, r.main, channel === "passwords.save" ? { origin: "https://example.com", username: "ada", password: PASSWORD } : {});
      expect(answer, channel).toMatchObject({ ok: false, error: { code: "POLICY_DENIED" } });
    }
    expect(r.composed.vault.count()).toBe(0);
  });

  it("report a vault the platform cannot keep, and one this device cannot read, without losing what is there", async () => {
    const r = rig({ cipher: { isEncryptionAvailable: () => false, encryptString: () => Buffer.alloc(0), decryptString: () => "" } });
    expect(await r.call("passwords.status", r.main)).toMatchObject({ ok: true, result: { available: false } });
    expect(await r.call("passwords.save", r.main, { origin: "https://example.com", username: "ada", password: PASSWORD })).toMatchObject({ ok: false, error: { code: "ADAPTER_UNAVAILABLE", details: { reason: "unavailable" } } });
  });
});

// ------------------------------------------------------------------ structure

function sources(directory: string, extensions: RegExp): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(directory)) {
    if (entry === "node_modules" || entry === "target") {
      continue;
    }
    const path = join(directory, entry);
    if (statSync(path).isDirectory()) {
      found.push(...sources(path, extensions));
    } else if (extensions.test(entry)) {
      found.push(path);
    }
  }
  return found;
}

const code = (path: string): string => readFileSync(path, "utf8").replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");

describe("the structure that keeps the vault a person's", () => {
  const desktop = sources(join(ROOT, "apps", "desktop", "src"), /\.tsx?$/);

  it("decrypts a saved password in one place, the filler, which wipes it", () => {
    const callers = desktop.filter((path) => /\.password\(/.test(code(path)) && !path.endsWith("vault.ts")).map((path) => relative(ROOT, path));
    expect(callers).toEqual(["apps/desktop/src/main/secrets/fill.ts"]);
    const filler = code(join(ROOT, "apps/desktop/src/main/secrets/fill.ts"));
    expect(filler).toMatch(/finally\s*\{[^}]*password\.wipe\(\)/);
    // Nothing logs or serializes a password: the vault's list and the host's answers are built from metadata.
    const host = code(join(ROOT, "apps/desktop/src/main/secrets/host.ts"));
    expect(host).not.toMatch(/\.password\(|expose\(\)/);
  });

  it("gives no agent tool, server command or worker call to the vault or the importer", () => {
    const tools = readFileSync(join(ROOT, "config", "tools.yaml"), "utf8");
    expect(tools).not.toMatch(/name:\s*(passwords|vault|import)\./);
    const catalog = readFileSync(join(ROOT, "schemas", "catalog", "tools.yaml"), "utf8");
    expect(catalog).not.toMatch(/passwords\.|vault\./);
    const server = sources(join(ROOT, "crates"), /\.rs$/).filter((path) => !path.includes("/tests/") && /password.vault|PasswordVault|password-vault/i.test(readFileSync(path, "utf8")));
    expect(server.map((path) => relative(ROOT, path))).toEqual([]);
  });

  it("is reached from the page only through the bridge, and only the settings page saves a password", () => {
    const renderer = desktop.filter((path) => path.includes("/renderer/"));
    const saving = renderer.filter((path) => /savePassword\(/.test(code(path))).map((path) => relative(ROOT, path));
    expect(saving).toEqual(["apps/desktop/src/renderer/SecretsSettings.tsx"]);
    const filling = renderer.filter((path) => /fillPassword\(/.test(code(path))).map((path) => relative(ROOT, path));
    expect(filling).toEqual(["apps/desktop/src/renderer/LoginSuggestions.tsx"]);
    for (const path of renderer.filter((candidate) => /Secrets|Login|Payment/.test(candidate))) {
      expect(code(path), path).not.toMatch(/localStorage|sessionStorage|indexedDB|console\./);
    }
    // The password field is cleared before the save is awaited.
    const settings = code(join(ROOT, "apps/desktop/src/renderer/SecretsSettings.tsx"));
    expect(settings.indexOf("form.current.reset()")).toBeGreaterThan(0);
    expect(settings.indexOf("form.current.reset()")).toBeLessThan(settings.indexOf("await window.quansio.savePassword"));
  });

  it("keeps imported logins on the device: the importer sends the server cookies and nothing else", () => {
    const importer = code(join(ROOT, "apps/desktop/src/main/secrets/import.ts"));
    expect(importer).not.toMatch(/invoke\(|fetch\(/);
    const compose = code(join(ROOT, "apps/desktop/src/main/secrets/compose.ts"));
    const invocations = [...compose.matchAll(/invoke\(\s*"([A-Za-z]+)"/g)].map((match) => match[1]);
    expect(invocations).toEqual(["ImportBrowserSession"]);
  });
});
