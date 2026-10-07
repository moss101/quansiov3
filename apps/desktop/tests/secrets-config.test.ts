/**
 * `config/local-secrets.yaml` (ADP-009, D-018): the values the password manager, the browser-data import and the payment form
 * run on, bounded exactly as `schemas/json/local-secrets-config.schema.json` bounds them. No key names a path or holds a secret.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { LOCAL_SECRETS_BOUNDS, loadLocalSecretsConfig, localSecretsConfigFrom, parseLocalSecretsConfig } from "../src/main/secrets/config.js";
import { parseFlatYaml } from "../src/main/shell-config.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const CONFIG_PATH = join(ROOT, "config", "local-secrets.yaml");

describe("config/local-secrets.yaml", () => {
  it("is the file the shell loads, and says what the contract says", () => {
    const config = loadLocalSecretsConfig(CONFIG_PATH);
    expect(config.enabled).toBe(true);
    // DOMAIN.md 13.9 C: a supplied value waits five minutes by default and never more than the server's own ceiling.
    expect(config.paymentTtlMs).toBe(300_000);
    expect(config.paymentMaxFields).toBe(12);
    expect(config.paymentValueMaxChars).toBe(128);
    // 13.9 B: an import needs an approval, and the approval does not outlive its time.
    expect(config.importApprovalTtlMs).toBe(300_000);
  });

  it("is bounded exactly as the JSON schema bounds it", () => {
    const schema = JSON.parse(readFileSync(join(ROOT, "schemas", "json", "local-secrets-config.schema.json"), "utf8")) as {
      properties: Record<string, { minimum?: number; maximum?: number }>;
      required: string[];
    };
    const text = readFileSync(CONFIG_PATH, "utf8");
    for (const [key, [low, high]] of Object.entries(LOCAL_SECRETS_BOUNDS)) {
      expect(schema.properties[key]?.minimum, key).toBe(low);
      expect(schema.properties[key]?.maximum, key).toBe(high);
      const set = (value: number): string => text.replace(new RegExp(`^${key.replace(/\./g, "\\.")}: .*$`, "m"), `${key}: ${String(value)}`);
      expect(() => parseLocalSecretsConfig(set(low)), `${key} at its minimum`).not.toThrow();
      expect(() => parseLocalSecretsConfig(set(high)), `${key} at its maximum`).not.toThrow();
      expect(() => parseLocalSecretsConfig(set(low - 1)), `${key} below its minimum`).toThrow(/must be between/);
      expect(() => parseLocalSecretsConfig(set(high + 1)), `${key} above its maximum`).toThrow(/must be between/);
    }
    // Every key the schema requires is one the shell reads, and the shell reads no other.
    expect(Object.keys(parseFlatYaml(text)).sort()).toEqual([...schema.required].sort());
  });

  it("refuses a key it does not know, a missing key and a switch that is not a switch", () => {
    const text = readFileSync(CONFIG_PATH, "utf8");
    expect(() => parseLocalSecretsConfig(`${text}local_secrets.vault_path: x\n`)).toThrow(/not a known key/);
    expect(() => parseLocalSecretsConfig(text.replace(/^local_secrets\.passwords_max: .*$/m, ""))).toThrow(/passwords_max/);
    expect(() => parseLocalSecretsConfig(text.replace("version: 1", "version: 2"))).toThrow(/version must be 1/);
    expect(() => localSecretsConfigFrom({ ...parseFlatYaml(text), "local_secrets.enabled": "yes" })).toThrow(/true or false/);
    expect(parseLocalSecretsConfig(text.replace("local_secrets.enabled: true", "local_secrets.enabled: false")).enabled).toBe(false);
  });

  it("keeps the payment ceiling at or under the runtime's", () => {
    const runtime = readFileSync(join(ROOT, "config", "payment-inputs.yaml"), "utf8");
    const ceiling = Number(/^max_ttl_seconds: (\d+)/m.exec(runtime)?.[1]);
    const fields = Number(/^max_fields: (\d+)/m.exec(runtime)?.[1]);
    const chars = Number(/^max_value_chars: (\d+)/m.exec(runtime)?.[1]);
    const config = loadLocalSecretsConfig(CONFIG_PATH);
    expect(config.paymentTtlMs / 1000).toBeLessThanOrEqual(ceiling);
    expect(config.paymentMaxFields).toBeLessThanOrEqual(fields);
    expect(config.paymentValueMaxChars).toBeLessThanOrEqual(chars);
  });
});
