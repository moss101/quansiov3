/**
 * Evidence (APP-006, CORE-007): bytes are only ever shown next to the digest they were
 * recorded under, and the page checks that digest itself.
 *
 * The server refuses to serve bytes that do not hash to the recorded digest (typed
 * `digest_mismatch` details on the error). The page verifies again on its side: a
 * mismatch between what it received and the digest it was told is an error state the
 * user sees, not a silent display.
 */

export type EvidenceState =
  | { readonly kind: "loading"; readonly evidenceId: string }
  | { readonly kind: "verified"; readonly evidenceId: string; readonly digest: string; readonly bytes: Uint8Array; readonly text: string | null }
  | { readonly kind: "digest_error"; readonly evidenceId: string; readonly expected: string; readonly actual: string; readonly source: "server" | "page" }
  | { readonly kind: "unavailable"; readonly evidenceId: string; readonly reason: string };

/** sha256 of the bytes, as the server spells it (bare hex); Web Crypto, so the renderer can do it too. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  // A copy into a plain ArrayBuffer: Web Crypto refuses a view over a shared buffer.
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  const digest = await globalThis.crypto.subtle.digest("SHA-256", copy.buffer);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Decode the error body of a refused content read; a digest mismatch is typed. */
export function errorState(evidenceId: string, error: unknown): EvidenceState {
  const details = error && typeof error === "object" && "details" in error ? (error as { details?: unknown }).details : null;
  const record = details && typeof details === "object" ? (details as Record<string, unknown>) : null;
  if (record && record["kind"] === "digest_mismatch") {
    return {
      kind: "digest_error",
      evidenceId,
      expected: typeof record["expected"] === "string" ? record["expected"] : "",
      actual: typeof record["actual"] === "string" ? record["actual"] : "",
      source: "server",
    };
  }
  return { kind: "unavailable", evidenceId, reason: error instanceof Error ? error.message : String(error) };
}

/** Verify received bytes against the digest the server labelled them with. */
export async function verifiedState(evidenceId: string, bytes: Uint8Array, labelledDigest: string, textPreview: (bytes: Uint8Array) => string | null): Promise<EvidenceState> {
  const actual = await sha256Hex(bytes);
  if (actual !== labelledDigest) {
    return { kind: "digest_error", evidenceId, expected: labelledDigest, actual, source: "page" };
  }
  return { kind: "verified", evidenceId, digest: actual, bytes, text: textPreview(bytes) };
}

/** A bounded UTF-8 rendering for text-like evidence, or null for bytes that are not text. */
export function evidenceText(bytes: Uint8Array, maxChars = 20_000): string | null {
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
  return text.length > maxChars ? `${text.slice(0, maxChars)}\n…[truncated]` : text;
}
