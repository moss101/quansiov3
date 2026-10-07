/**
 * The local-secrets limits (ADP-009, D-018): `config/local-secrets.yaml`, read by the shell's flat-YAML parser and bounded
 * exactly as `schemas/json/local-secrets-config.schema.json` bounds it (a test holds the two together). No key names a
 * path or holds a secret: the file holds counts, sizes and timings only.
 */

import { readFileSync } from "node:fs";

import { parseFlatYaml } from "../shell-config.js";

/** What the password manager, the import and the payment-input modules run on. */
export interface LocalSecretsConfig {
  /** Off: every passwords, import and payment channel answers `POLICY_DENIED` and nothing is read or stored. */
  readonly enabled: boolean;
  readonly passwordsMax: number;
  readonly fieldMaxChars: number;
  readonly suggestionsMax: number;
  readonly confirmTtlMs: number;
  readonly fillMaxAgeMs: number;
  readonly importEnabled: boolean;
  readonly importMaxCookies: number;
  readonly importMaxLogins: number;
  readonly importMaxDbBytes: number;
  readonly importApprovalTtlMs: number;
  readonly paymentTtlMs: number;
  readonly paymentMaxFields: number;
  readonly paymentValueMaxChars: number;
}

/** The bounds of each numeric key (kept equal to the JSON schema's by a test). */
export const LOCAL_SECRETS_BOUNDS: Readonly<Record<string, readonly [number, number]>> = {
  "local_secrets.passwords_max": [1, 10000],
  "local_secrets.field_max_chars": [16, 4096],
  "local_secrets.suggestions_max": [1, 32],
  "local_secrets.confirm_ttl_ms": [5000, 300000],
  "local_secrets.fill_max_age_ms": [1000, 60000],
  "local_secrets.import_max_cookies": [1, 100000],
  "local_secrets.import_max_logins": [1, 20000],
  "local_secrets.import_max_db_bytes": [1048576, 1073741824],
  "local_secrets.import_approval_ttl_ms": [10000, 900000],
  "local_secrets.payment_ttl_ms": [10000, 900000],
  "local_secrets.payment_max_fields": [1, 16],
  "local_secrets.payment_value_max_chars": [4, 256],
};

const SWITCHES = ["local_secrets.enabled", "local_secrets.import_enabled"] as const;
const KNOWN_KEYS = new Set<string>(["version", ...SWITCHES, ...Object.keys(LOCAL_SECRETS_BOUNDS)]);

type Scalar = string | number | boolean | null;

function numberOf(map: Record<string, Scalar>, key: string): number {
  const bounds = LOCAL_SECRETS_BOUNDS[key];
  const value = map[key];
  if (bounds === undefined || typeof value !== "number") {
    throw new Error(`local-secrets.yaml: ${key} must be an integer`);
  }
  if (value < bounds[0] || value > bounds[1]) {
    throw new Error(`local-secrets.yaml: ${key} must be between ${String(bounds[0])} and ${String(bounds[1])}`);
  }
  return value;
}

function switchOf(map: Record<string, Scalar>, key: (typeof SWITCHES)[number]): boolean {
  const value = map[key];
  if (typeof value !== "boolean") {
    throw new Error(`local-secrets.yaml: ${key} must be true or false`);
  }
  return value;
}

/** Validate a parsed map; throws on a missing, unknown or out-of-bounds key. */
export function localSecretsConfigFrom(map: Record<string, Scalar>): LocalSecretsConfig {
  for (const key of Object.keys(map)) {
    if (!KNOWN_KEYS.has(key)) {
      throw new Error(`local-secrets.yaml: ${key} is not a known key`);
    }
  }
  if (map["version"] !== 1) {
    throw new Error("local-secrets.yaml: version must be 1");
  }
  return {
    enabled: switchOf(map, "local_secrets.enabled"),
    passwordsMax: numberOf(map, "local_secrets.passwords_max"),
    fieldMaxChars: numberOf(map, "local_secrets.field_max_chars"),
    suggestionsMax: numberOf(map, "local_secrets.suggestions_max"),
    confirmTtlMs: numberOf(map, "local_secrets.confirm_ttl_ms"),
    fillMaxAgeMs: numberOf(map, "local_secrets.fill_max_age_ms"),
    importEnabled: switchOf(map, "local_secrets.import_enabled"),
    importMaxCookies: numberOf(map, "local_secrets.import_max_cookies"),
    importMaxLogins: numberOf(map, "local_secrets.import_max_logins"),
    importMaxDbBytes: numberOf(map, "local_secrets.import_max_db_bytes"),
    importApprovalTtlMs: numberOf(map, "local_secrets.import_approval_ttl_ms"),
    paymentTtlMs: numberOf(map, "local_secrets.payment_ttl_ms"),
    paymentMaxFields: numberOf(map, "local_secrets.payment_max_fields"),
    paymentValueMaxChars: numberOf(map, "local_secrets.payment_value_max_chars"),
  };
}

/** Parse `config/local-secrets.yaml`'s text. */
export function parseLocalSecretsConfig(text: string): LocalSecretsConfig {
  return localSecretsConfigFrom(parseFlatYaml(text));
}

/** Read the shipped configuration; a missing or invalid file is a packaging error and stops the feature starting. */
export function loadLocalSecretsConfig(path: string): LocalSecretsConfig {
  return parseLocalSecretsConfig(readFileSync(path, "utf8"));
}
