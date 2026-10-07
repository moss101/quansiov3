/**
 * Shell configuration and payload schemas (ADP-004, D-018): the values the shell runs on are
 * `config/desktop-shell.yaml`, bounded the same way as `schemas/json/desktop-shell-config.schema.json`, and every IPC
 * payload is validated against a schema that names its fields.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import * as s from "../src/main/schema.js";
import { NUMERIC_BOUNDS, loadShellConfig, parseFlatYaml, parseShellConfig, shellConfigFrom } from "../src/main/shell-config.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const CONFIG = join(ROOT, "config", "desktop-shell.yaml");

describe("config/desktop-shell.yaml", () => {
  it("is the file the shell loads, and says what the contract says", () => {
    const config = loadShellConfig(CONFIG);
    // The contract's numbers (DOMAIN.md 13.6): a 10 s grace, a 64-event queue, a 64-character binding.
    expect(config.offlineGraceMs).toBe(10_000);
    expect(config.pendingQueueCap).toBe(64);
    expect(config.shortcutMaxLength).toBe(64);
    expect(config.toggleDefault).toBe("mod+alt+q");
    expect(config.keepAwakeDefault).toBe(true);
    expect(config.indicatorEnabled).toBe(true);
  });

  it("is bounded exactly as the JSON schema bounds it", () => {
    const schema = JSON.parse(readFileSync(join(ROOT, "schemas", "json", "desktop-shell-config.schema.json"), "utf8")) as {
      properties: Record<string, { minimum?: number; maximum?: number }>;
      required: string[];
    };
    const text = readFileSync(CONFIG, "utf8");
    for (const [key, [low, high]] of Object.entries(NUMERIC_BOUNDS)) {
      expect(schema.properties[key]?.minimum, key).toBe(low);
      expect(schema.properties[key]?.maximum, key).toBe(high);
      const set = (value: number): string => text.replace(new RegExp(`^${key.replace(/\./g, "\\.")}: .*$`, "m"), `${key}: ${String(value)}`);
      expect(() => parseShellConfig(set(low)), `${key} at its minimum`).not.toThrow();
      expect(() => parseShellConfig(set(high)), `${key} at its maximum`).not.toThrow();
      expect(() => parseShellConfig(set(low - 1)), `${key} below its minimum`).toThrow(/must be between/);
      expect(() => parseShellConfig(set(high + 1)), `${key} above its maximum`).toThrow(/must be between/);
    }
    // Every key the schema requires is one the shell reads, and the shell reads no other.
    const parsed = parseFlatYaml(text);
    expect(Object.keys(parsed).sort()).toEqual([...schema.required].sort());
  });

  it("refuses a key it does not know, a missing key and a malformed line", () => {
    const text = readFileSync(CONFIG, "utf8");
    expect(() => parseShellConfig(`${text}quit.confirm_when_busy: false\n`)).toThrow(/not a known key/);
    expect(() => parseShellConfig(text.replace(/^pending\.queue_cap: .*$/m, ""))).toThrow(/pending.queue_cap/);
    expect(() => parseShellConfig(`${text}this is not a pair\n`)).toThrow(/line \d+/);
    expect(() => parseShellConfig(`${text}pending.queue_cap: 3\n`)).toThrow(/set twice/);
    expect(() => parseShellConfig(text.replace("version: 1", "version: 2"))).toThrow(/version must be 1/);
    expect(() => shellConfigFrom({ ...parseFlatYaml(text), "indicator.enabled": "yes" })).toThrow(/true or false/);
  });

  it("reads a null default shortcut as none", () => {
    const text = readFileSync(CONFIG, "utf8").replace(/^global_shortcut\.toggle_default: .*$/m, "global_shortcut.toggle_default: null");
    expect(parseShellConfig(text).toggleDefault).toBeNull();
  });
});

describe("payload schemas", () => {
  const point = s.object<{ x: number; y: number; label?: string }>({ x: s.integer({ min: -10, max: 10 }), y: s.integer({ min: -10, max: 10 }), label: s.optional(s.string({ max: 4 })) });

  it("accepts exactly the named fields", () => {
    expect(s.parse(point, { x: 1, y: -2 })).toEqual({ x: 1, y: -2 });
    expect(s.parse(point, { x: 1, y: 2, label: "ok" })).toEqual({ x: 1, y: 2, label: "ok" });
  });

  it("refuses an unknown field instead of passing it through", () => {
    const result = s.safeParse(point, { x: 1, y: 2, path: "/etc/passwd" });
    expect(result.ok).toBe(false);
    expect(!result.ok && result.error.path).toBe("payload.path");
  });

  it("refuses the wrong type, a missing field and an out-of-bounds value, naming the field", () => {
    expect(() => s.parse(point, { x: "1", y: 2 })).toThrow(/payload.x: must be an integer/);
    expect(() => s.parse(point, { x: 1.5, y: 2 })).toThrow(/integer/);
    expect(() => s.parse(point, { x: 1 })).toThrow(/payload.y/);
    expect(() => s.parse(point, { x: 11, y: 0 })).toThrow(/between -10 and 10/);
    expect(() => s.parse(point, { x: 1, y: 1, label: "toolong" })).toThrow(/at most 4/);
    expect(() => s.parse(point, null)).toThrow(/must be an object/);
    expect(() => s.parse(point, [1, 2])).toThrow(/must be an object/);
  });

  it("covers the other combinators", () => {
    expect(s.parse(s.oneOf(["a", "b"] as const), "a")).toBe("a");
    expect(() => s.parse(s.oneOf(["a", "b"] as const), "c")).toThrow(/one of a, b/);
    expect(s.parse(s.nullable(s.boolean()), null)).toBeNull();
    expect(() => s.parse(s.boolean(), "true")).toThrow();
    expect(s.parse(s.array(s.integer({ min: 0, max: 9 }), 2), [1, 2])).toEqual([1, 2]);
    expect(() => s.parse(s.array(s.integer({ min: 0, max: 9 }), 2), [1, 2, 3])).toThrow(/at most 2/);
    expect(s.parse(s.literal(1), 1)).toBe(1);
    expect(() => s.parse(s.literal(1), 2)).toThrow();
    expect(s.parse(s.empty(), undefined)).toEqual({});
    expect(s.parse(s.empty(), {})).toEqual({});
    expect(() => s.parse(s.empty(), { a: 1 })).toThrow(/payload.a: is not a known field/);
    expect(() => s.parse(s.empty(), 5)).toThrow(/takes no fields/);
    expect(s.parse(s.record(s.integer({ min: 0, max: 1 }), /^[a-z]+$/, 2), { a: 1 })).toEqual({ a: 1 });
    expect(() => s.parse(s.record(s.integer({ min: 0, max: 1 }), /^[a-z]+$/, 2), { "A!": 1 })).toThrow(/accepted key/);
    expect(() => s.parse(s.record(s.integer({ min: 0, max: 1 }), /^[a-z]+$/, 1), { a: 1, b: 0 })).toThrow(/at most 1/);
  });

  it("leaves an absent optional key absent", () => {
    expect("label" in s.parse(point, { x: 0, y: 0 })).toBe(false);
  });
});
