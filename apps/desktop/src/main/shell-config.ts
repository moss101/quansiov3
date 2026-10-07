/**
 * The desktop shell's configuration (ADP-004, D-018).
 *
 * Timings, queue bounds, the default global shortcut and the secondary-window switches are
 * `config/desktop-shell.yaml`, never source constants. The build copies that file beside `main.cjs`; the shell has
 * no YAML dependency, so the file is a flat map of dotted keys to scalars and this module reads exactly that
 * subset. The same bounds are in `schemas/json/desktop-shell-config.schema.json` (checked by the contract suite),
 * and a test here holds the two in agreement, so a bound cannot drift from either side.
 */

import { readFileSync } from "node:fs";

/** What the shell reads from `config/desktop-shell.yaml`. */
export interface DesktopShellConfig {
  readonly offlineGraceMs: number;
  readonly reconnectMinGapMs: number;
  readonly pendingQueueCap: number;
  readonly shortcutMaxLength: number;
  /** The global toggle shortcut registered before the person has chosen anything; `null` for none. */
  readonly toggleDefault: string | null;
  readonly keepAwakeDefault: boolean;
  readonly activityMaxItems: number;
  readonly activityCompletedTtlMs: number;
  readonly popoutAlwaysOnTopAllowed: boolean;
  readonly quickEntryEnabled: boolean;
  readonly indicatorEnabled: boolean;
  readonly installWhenIdleDefault: boolean;
  readonly updateIdlePollMs: number;
}

type Scalar = string | number | boolean | null;

/** Parse the flat `key: scalar` subset the shell's config uses; anything else is refused with its line number. */
export function parseFlatYaml(text: string): Record<string, Scalar> {
  const out: Record<string, Scalar> = {};
  const lines = text.split(/\r?\n/);
  for (const [index, raw] of lines.entries()) {
    const line = raw.replace(/\s+#.*$/, "").trimEnd();
    if (line.trim() === "" || line.trimStart().startsWith("#")) {
      continue;
    }
    const found = /^([a-z][a-z0-9_.]*):\s+(.+)$/.exec(line);
    if (!found?.[1] || found[2] === undefined) {
      throw new Error(`desktop-shell.yaml line ${String(index + 1)}: expected "key: value"`);
    }
    const key = found[1];
    if (key in out) {
      throw new Error(`desktop-shell.yaml line ${String(index + 1)}: ${key} is set twice`);
    }
    const value = found[2].trim();
    if (value === "true" || value === "false") {
      out[key] = value === "true";
    } else if (value === "null" || value === "~") {
      out[key] = null;
    } else if (/^-?\d+$/.test(value)) {
      out[key] = Number(value);
    } else if (/^"[^"]*"$/.test(value) || /^'[^']*'$/.test(value)) {
      out[key] = value.slice(1, -1);
    } else if (/^[A-Za-z0-9+_.-]+$/.test(value)) {
      out[key] = value;
    } else {
      throw new Error(`desktop-shell.yaml line ${String(index + 1)}: ${key} is not a plain scalar`);
    }
  }
  return out;
}

/** The bounds of each numeric key (kept equal to the JSON schema's by a test). */
export const NUMERIC_BOUNDS: Readonly<Record<string, readonly [number, number]>> = {
  "connectivity.offline_grace_ms": [1000, 60000],
  "connectivity.reconnect_min_gap_ms": [250, 30000],
  "pending.queue_cap": [1, 256],
  "shortcut.max_length": [8, 64],
  "activity.max_items": [4, 100],
  "activity.completed_ttl_ms": [10000, 86400000],
  "update.idle_poll_ms": [5000, 600000],
};

const BOOLEAN_KEYS = [
  "keep_awake.default_enabled",
  "popout.always_on_top_allowed",
  "quick_entry.enabled",
  "indicator.enabled",
  "update.install_when_idle_default",
] as const;

const KNOWN_KEYS = new Set<string>([
  "version",
  "global_shortcut.toggle_default",
  ...Object.keys(NUMERIC_BOUNDS),
  ...BOOLEAN_KEYS,
]);

function numberOf(map: Record<string, Scalar>, key: string): number {
  const bounds = NUMERIC_BOUNDS[key];
  const value = map[key];
  if (bounds === undefined || typeof value !== "number") {
    throw new Error(`desktop-shell.yaml: ${key} must be an integer`);
  }
  if (value < bounds[0] || value > bounds[1]) {
    throw new Error(`desktop-shell.yaml: ${key} must be between ${String(bounds[0])} and ${String(bounds[1])}`);
  }
  return value;
}

function flagOf(map: Record<string, Scalar>, key: (typeof BOOLEAN_KEYS)[number]): boolean {
  const value = map[key];
  if (typeof value !== "boolean") {
    throw new Error(`desktop-shell.yaml: ${key} must be true or false`);
  }
  return value;
}

/** Validate a parsed map into the typed configuration; throws on a missing, unknown or out-of-bounds key. */
export function shellConfigFrom(map: Record<string, Scalar>): DesktopShellConfig {
  for (const key of Object.keys(map)) {
    if (!KNOWN_KEYS.has(key)) {
      throw new Error(`desktop-shell.yaml: ${key} is not a known key`);
    }
  }
  if (map["version"] !== 1) {
    throw new Error("desktop-shell.yaml: version must be 1");
  }
  const toggle = map["global_shortcut.toggle_default"];
  if (toggle !== null && (typeof toggle !== "string" || toggle.length === 0)) {
    throw new Error("desktop-shell.yaml: global_shortcut.toggle_default must be a binding or null");
  }
  return {
    offlineGraceMs: numberOf(map, "connectivity.offline_grace_ms"),
    reconnectMinGapMs: numberOf(map, "connectivity.reconnect_min_gap_ms"),
    pendingQueueCap: numberOf(map, "pending.queue_cap"),
    shortcutMaxLength: numberOf(map, "shortcut.max_length"),
    toggleDefault: toggle ?? null,
    keepAwakeDefault: flagOf(map, "keep_awake.default_enabled"),
    activityMaxItems: numberOf(map, "activity.max_items"),
    activityCompletedTtlMs: numberOf(map, "activity.completed_ttl_ms"),
    popoutAlwaysOnTopAllowed: flagOf(map, "popout.always_on_top_allowed"),
    quickEntryEnabled: flagOf(map, "quick_entry.enabled"),
    indicatorEnabled: flagOf(map, "indicator.enabled"),
    installWhenIdleDefault: flagOf(map, "update.install_when_idle_default"),
    updateIdlePollMs: numberOf(map, "update.idle_poll_ms"),
  };
}

/** Parse `config/desktop-shell.yaml`'s text. */
export function parseShellConfig(text: string): DesktopShellConfig {
  return shellConfigFrom(parseFlatYaml(text));
}

/** Read the shipped configuration; a missing or invalid file is a packaging error and stops the shell starting. */
export function loadShellConfig(path: string): DesktopShellConfig {
  return parseShellConfig(readFileSync(path, "utf8"));
}
