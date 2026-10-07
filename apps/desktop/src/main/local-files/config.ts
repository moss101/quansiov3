/**
 * The local-file limits (ADP-005, D-018): `config/local-files.yaml`, read by the shell's flat-YAML parser and bounded
 * exactly as `schemas/json/local-files-config.schema.json` bounds it (a test holds the two together). No key accepts a
 * path: the file holds counts, sizes and timings only.
 */

import { readFileSync } from "node:fs";

import { parseFlatYaml } from "../shell-config.js";

/** What the local-file modules run on. */
export interface LocalFilesConfig {
  /** Off: every local-file channel answers `POLICY_DENIED` and no native dialog opens. */
  readonly enabled: boolean;
  readonly grantTtlMs: number;
  readonly grantMaxLive: number;
  readonly grantMaxFiles: number;
  readonly grantMaxFileBytes: number;
  readonly grantMaxTotalBytes: number;
  readonly rootsMax: number;
  readonly entriesMax: number;
  readonly mediaMaxTokens: number;
  readonly editMaxBytes: number;
  readonly editMaxSessions: number;
  readonly selfWriteTtlMs: number;
  readonly watchDebounceMs: number;
  readonly watchMaxDirs: number;
  readonly watchMaxSubscribers: number;
  readonly watchQueueMax: number;
  readonly watchMaxEntries: number;
}

/** The bounds of each numeric key (kept equal to the JSON schema's by a test). */
export const LOCAL_FILES_BOUNDS: Readonly<Record<string, readonly [number, number]>> = {
  "local_files.grant_ttl_ms": [5000, 600000],
  "local_files.grant_max_live": [1, 128],
  "local_files.grant_max_files": [1, 100],
  "local_files.grant_max_file_bytes": [1024, 268435456],
  "local_files.grant_max_total_bytes": [1024, 1073741824],
  "local_files.roots_max": [1, 32],
  "local_files.entries_max": [64, 65536],
  "local_files.media_max_tokens": [1, 4096],
  "local_files.edit_max_bytes": [1024, 16777216],
  "local_files.edit_max_sessions": [1, 64],
  "local_files.self_write_ttl_ms": [100, 10000],
  "local_files.watch_debounce_ms": [50, 5000],
  "local_files.watch_max_dirs": [1, 256],
  "local_files.watch_max_subscribers": [1, 64],
  "local_files.watch_queue_max": [16, 4096],
  "local_files.watch_max_entries": [100, 100000],
};

const KNOWN_KEYS = new Set<string>(["version", "local_files.enabled", ...Object.keys(LOCAL_FILES_BOUNDS)]);

type Scalar = string | number | boolean | null;

function numberOf(map: Record<string, Scalar>, key: string): number {
  const bounds = LOCAL_FILES_BOUNDS[key];
  const value = map[key];
  if (bounds === undefined || typeof value !== "number") {
    throw new Error(`local-files.yaml: ${key} must be an integer`);
  }
  if (value < bounds[0] || value > bounds[1]) {
    throw new Error(`local-files.yaml: ${key} must be between ${String(bounds[0])} and ${String(bounds[1])}`);
  }
  return value;
}

/** Validate a parsed map; throws on a missing, unknown or out-of-bounds key. */
export function localFilesConfigFrom(map: Record<string, Scalar>): LocalFilesConfig {
  for (const key of Object.keys(map)) {
    if (!KNOWN_KEYS.has(key)) {
      throw new Error(`local-files.yaml: ${key} is not a known key`);
    }
  }
  if (map["version"] !== 1) {
    throw new Error("local-files.yaml: version must be 1");
  }
  const enabled = map["local_files.enabled"];
  if (typeof enabled !== "boolean") {
    throw new Error("local-files.yaml: local_files.enabled must be true or false");
  }
  return {
    enabled,
    grantTtlMs: numberOf(map, "local_files.grant_ttl_ms"),
    grantMaxLive: numberOf(map, "local_files.grant_max_live"),
    grantMaxFiles: numberOf(map, "local_files.grant_max_files"),
    grantMaxFileBytes: numberOf(map, "local_files.grant_max_file_bytes"),
    grantMaxTotalBytes: numberOf(map, "local_files.grant_max_total_bytes"),
    rootsMax: numberOf(map, "local_files.roots_max"),
    entriesMax: numberOf(map, "local_files.entries_max"),
    mediaMaxTokens: numberOf(map, "local_files.media_max_tokens"),
    editMaxBytes: numberOf(map, "local_files.edit_max_bytes"),
    editMaxSessions: numberOf(map, "local_files.edit_max_sessions"),
    selfWriteTtlMs: numberOf(map, "local_files.self_write_ttl_ms"),
    watchDebounceMs: numberOf(map, "local_files.watch_debounce_ms"),
    watchMaxDirs: numberOf(map, "local_files.watch_max_dirs"),
    watchMaxSubscribers: numberOf(map, "local_files.watch_max_subscribers"),
    watchQueueMax: numberOf(map, "local_files.watch_queue_max"),
    watchMaxEntries: numberOf(map, "local_files.watch_max_entries"),
  };
}

/** Parse `config/local-files.yaml`'s text. */
export function parseLocalFilesConfig(text: string): LocalFilesConfig {
  return localFilesConfigFrom(parseFlatYaml(text));
}

/** Read the shipped configuration; a missing or invalid file is a packaging error and stops the feature starting. */
export function loadLocalFilesConfig(path: string): LocalFilesConfig {
  return parseLocalFilesConfig(readFileSync(path, "utf8"));
}
