/**
 * `config/local-files.yaml` (ADP-005, D-018): the values the local-file modules run on, bounded exactly as
 * `schemas/json/local-files-config.schema.json` bounds them. No key accepts a path.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { LOCAL_FILES_BOUNDS, loadLocalFilesConfig, localFilesConfigFrom, parseLocalFilesConfig } from "../src/main/local-files/config.js";
import { parseFlatYaml } from "../src/main/shell-config.js";

import { CONFIG, CONFIG_PATH } from "./local-files-fixtures.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

describe("config/local-files.yaml", () => {
  it("is the file the shell loads, and says what the contract says", () => {
    const config = loadLocalFilesConfig(CONFIG_PATH);
    // DOMAIN.md 13.7 A: 60 s, at most 32 live grants, 20 files, 50 MiB a file, 100 MiB in all.
    expect(config.enabled).toBe(true);
    expect(config.grantTtlMs).toBe(60_000);
    expect(config.grantMaxLive).toBe(32);
    expect(config.grantMaxFiles).toBe(20);
    expect(config.grantMaxFileBytes).toBe(50 * 1024 * 1024);
    expect(config.grantMaxTotalBytes).toBe(100 * 1024 * 1024);
    // 13.7 E: a 300 ms debounce.
    expect(config.watchDebounceMs).toBe(300);
    expect(CONFIG).toEqual(config);
  });

  it("is bounded exactly as the JSON schema bounds it", () => {
    const schema = JSON.parse(readFileSync(join(ROOT, "schemas", "json", "local-files-config.schema.json"), "utf8")) as {
      properties: Record<string, { minimum?: number; maximum?: number }>;
      required: string[];
    };
    const text = readFileSync(CONFIG_PATH, "utf8");
    for (const [key, [low, high]] of Object.entries(LOCAL_FILES_BOUNDS)) {
      expect(schema.properties[key]?.minimum, key).toBe(low);
      expect(schema.properties[key]?.maximum, key).toBe(high);
      const set = (value: number): string => text.replace(new RegExp(`^${key.replace(/\./g, "\\.")}: .*$`, "m"), `${key}: ${String(value)}`);
      expect(() => parseLocalFilesConfig(set(low)), `${key} at its minimum`).not.toThrow();
      expect(() => parseLocalFilesConfig(set(high)), `${key} at its maximum`).not.toThrow();
      expect(() => parseLocalFilesConfig(set(low - 1)), `${key} below its minimum`).toThrow(/must be between/);
      expect(() => parseLocalFilesConfig(set(high + 1)), `${key} above its maximum`).toThrow(/must be between/);
    }
    // Every key the schema requires is one the shell reads, and the shell reads no other.
    expect(Object.keys(parseFlatYaml(text)).sort()).toEqual([...schema.required].sort());
  });

  it("refuses a key it does not know, a missing key and a switch that is not a switch", () => {
    const text = readFileSync(CONFIG_PATH, "utf8");
    expect(() => parseLocalFilesConfig(`${text}local_files.allowed_paths: x\n`)).toThrow(/not a known key/);
    expect(() => parseLocalFilesConfig(text.replace(/^local_files\.grant_ttl_ms: .*$/m, ""))).toThrow(/grant_ttl_ms/);
    expect(() => parseLocalFilesConfig(text.replace("version: 1", "version: 2"))).toThrow(/version must be 1/);
    expect(() => localFilesConfigFrom({ ...parseFlatYaml(text), "local_files.enabled": "yes" })).toThrow(/true or false/);
    expect(parseLocalFilesConfig(text.replace("local_files.enabled: true", "local_files.enabled: false")).enabled).toBe(false);
  });
});
