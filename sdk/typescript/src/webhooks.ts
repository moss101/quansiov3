/**
 * Outbound webhook signature verification (APP-015, DOMAIN.md §13.5).
 *
 * A delivery carries `quansio-signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret,
 * "<t>.<body>")>`. Verify it over the raw request body — before parsing it — with the
 * subscription's `whsec_…` secret, and reject a timestamp outside the tolerance so a captured
 * delivery cannot be replayed later. Deliveries are at-least-once: dedupe on
 * `quansio-event-id`. The header names, scheme and tolerance come from the generated
 * {@link WEBHOOK_SIGNATURE}; this module is only the arithmetic (Web Crypto, no dependencies).
 */

import { WEBHOOK_SIGNATURE } from "./generated/operations.js";

export { WEBHOOK_SIGNATURE };

/** Why a signature was refused. */
export type WebhookSignatureFailure = "malformed" | "stale" | "mismatch";

export class WebhookSignatureError extends Error {
  readonly reason: WebhookSignatureFailure;

  constructor(reason: WebhookSignatureFailure) {
    super(`webhook signature ${reason}`);
    this.name = "WebhookSignatureError";
    this.reason = reason;
  }
}

export interface VerifyWebhookOptions {
  /** The subscription's signing secret (`whsec_…`). */
  readonly secret: string;
  /** The `quansio-signature` header value. */
  readonly header: string;
  /** The raw request body, exactly as received. */
  readonly body: string | Uint8Array;
  /** Unix seconds; defaults to the local clock. */
  readonly now?: number;
  /** Accepted clock skew in seconds; defaults to the contract's tolerance. */
  readonly toleranceSeconds?: number;
}

const encoder = new TextEncoder();

function bytesOf(value: string | Uint8Array): Uint8Array {
  return typeof value === "string" ? encoder.encode(value) : value;
}

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let index = 0; index < out.length; index += 1) {
    out[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  }
  return out;
}

/** The `t=` timestamp and every signature for the contract's scheme in a header. */
export function parseWebhookSignature(header: string): { timestamp: number; signatures: string[] } | null {
  let timestamp: number | null = null;
  const signatures: string[] = [];
  for (const part of header.split(",")) {
    const at = part.indexOf("=");
    if (at < 0) continue;
    const key = part.slice(0, at).trim();
    const value = part.slice(at + 1).trim();
    if (key === "t" && /^-?\d+$/.test(value)) timestamp = Number(value);
    else if (key === WEBHOOK_SIGNATURE.scheme) signatures.push(value);
  }
  if (timestamp === null || signatures.length === 0) return null;
  return { timestamp, signatures };
}

/**
 * Verify a delivery. Resolves when the signature is valid; rejects with a
 * {@link WebhookSignatureError} naming why it is not.
 */
export async function verifyWebhookSignature(options: VerifyWebhookOptions): Promise<void> {
  const parsed = parseWebhookSignature(options.header);
  if (!parsed) throw new WebhookSignatureError("malformed");
  const now = options.now ?? Math.floor(Date.now() / 1000);
  const tolerance = options.toleranceSeconds ?? WEBHOOK_SIGNATURE.toleranceSeconds;
  if (Math.abs(now - parsed.timestamp) > tolerance) throw new WebhookSignatureError("stale");
  const key = await globalThis.crypto.subtle.importKey(
    "raw",
    encoder.encode(options.secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["verify"],
  );
  const body = bytesOf(options.body);
  const prefix = encoder.encode(`${String(parsed.timestamp)}.`);
  const signed = new Uint8Array(prefix.length + body.length);
  signed.set(prefix, 0);
  signed.set(body, prefix.length);
  for (const candidate of parsed.signatures) {
    const signature = hexToBytes(candidate);
    // `verify` compares in constant time.
    if (signature && (await globalThis.crypto.subtle.verify("HMAC", key, signature, signed))) return;
  }
  throw new WebhookSignatureError("mismatch");
}
