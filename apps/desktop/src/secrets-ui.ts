/**
 * The page's decoders for the password manager and the import (ADP-009, DOMAIN.md §13.9).
 *
 * Every answer is data from main: metadata and counts. These functions turn an answer into the view the settings page renders and the
 * refusal into words; none of them can produce a password or a cookie value, because none of the answers carries one.
 */

import { shellRefusal, shellResult } from "./shell-ui.js";

export interface SavedLogin {
  readonly id: string;
  readonly origin: string;
  readonly username: string;
}

export interface VaultStatus {
  readonly available: boolean;
  readonly locked: boolean;
  readonly count: number;
  readonly max: number;
}

export interface ScanDomain {
  readonly domain: string;
  readonly count: number;
}

export interface ScannedProfile {
  readonly family: string;
  readonly familyLabel: string;
  readonly profile: string;
  readonly displayName: string;
  readonly cookies: { readonly total: number; readonly domains: readonly ScanDomain[] } | null;
  readonly logins: number | null;
}

const record = (value: unknown): Record<string, unknown> => (value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {});

export function decodeVaultStatus(answer: unknown): VaultStatus | null {
  const result = shellResult(answer);
  if (result === null) {
    return null;
  }
  return {
    available: result["available"] === true,
    locked: result["locked"] === true,
    count: typeof result["count"] === "number" ? result["count"] : 0,
    max: typeof result["max"] === "number" ? result["max"] : 0,
  };
}

export function decodeLogins(answer: unknown): readonly SavedLogin[] {
  const result = shellResult(answer);
  const entries = result !== null && Array.isArray(result["entries"]) ? (result["entries"] as unknown[]) : [];
  return entries.flatMap((raw) => {
    const entry = record(raw);
    return typeof entry["id"] === "string" && typeof entry["origin"] === "string" && typeof entry["username"] === "string"
      ? [{ id: entry["id"], origin: entry["origin"], username: entry["username"] }]
      : [];
  });
}

export function decodeScan(answer: unknown): readonly ScannedProfile[] {
  const result = shellResult(answer);
  const profiles = result !== null && Array.isArray(result["profiles"]) ? (result["profiles"] as unknown[]) : [];
  return profiles.flatMap((raw) => {
    const profile = record(raw);
    if (typeof profile["family"] !== "string" || typeof profile["profile"] !== "string") {
      return [];
    }
    const cookies = profile["cookies"] === null || profile["cookies"] === undefined ? null : record(profile["cookies"]);
    const domains = cookies !== null && Array.isArray(cookies["domains"]) ? (cookies["domains"] as unknown[]) : [];
    return [
      {
        family: profile["family"],
        familyLabel: typeof profile["familyLabel"] === "string" ? profile["familyLabel"] : profile["family"],
        profile: profile["profile"],
        displayName: typeof profile["displayName"] === "string" ? profile["displayName"] : profile["profile"],
        cookies:
          cookies === null
            ? null
            : {
                total: typeof cookies["total"] === "number" ? cookies["total"] : 0,
                domains: domains.flatMap((entry) => {
                  const row = record(entry);
                  return typeof row["domain"] === "string" && typeof row["count"] === "number" ? [{ domain: row["domain"], count: row["count"] }] : [];
                }),
              },
        logins: typeof profile["logins"] === "number" ? profile["logins"] : null,
      },
    ];
  });
}

/** A refusal as one line, including the reason when the answer names one. */
export function refusalText(answer: unknown): string | null {
  const refusal = shellRefusal(answer);
  if (refusal === null) {
    return null;
  }
  const reason = typeof refusal.details["reason"] === "string" ? ` (${refusal.details["reason"]})` : "";
  return `${refusal.code}${reason}: ${refusal.message}`;
}

/** The counts an import answered with. */
export function importCounts(answer: unknown): { readonly imported: number; readonly skipped: number } | null {
  const result = shellResult(answer);
  if (result === null || typeof result["imported"] !== "number") {
    return null;
  }
  return { imported: result["imported"], skipped: typeof result["skipped"] === "number" ? result["skipped"] : 0 };
}

/** What a staged cookie bundle answered with: counts, domains and a time. */
export function stagedSummary(answer: unknown): { readonly cookieCount: number; readonly domains: readonly string[]; readonly moreDomains: number; readonly expiresAt: string } | null {
  const result = shellResult(answer);
  if (result === null || typeof result["cookieCount"] !== "number") {
    return null;
  }
  return {
    cookieCount: result["cookieCount"],
    domains: Array.isArray(result["domains"]) ? (result["domains"] as unknown[]).filter((domain): domain is string => typeof domain === "string") : [],
    moreDomains: typeof result["moreDomains"] === "number" ? result["moreDomains"] : 0,
    expiresAt: typeof result["expiresAt"] === "string" ? result["expiresAt"] : "",
  };
}
