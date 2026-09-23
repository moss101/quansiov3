/**
 * APP-015: the SDK verifies a webhook exactly as the server signs it. The vector is the one
 * the server's own test asserts (`crates/server/src/notify/webhooks/signature.rs`), so the
 * two sides cannot drift apart.
 */

import { describe, expect, it } from "vitest";

import { WEBHOOK_SIGNATURE, WebhookSignatureError, parseWebhookSignature, verifyWebhookSignature } from "../src/index.js";

const SECRET = "whsec_dGVzdC1vbmx5LXNpZ25pbmctc2VjcmV0LTMyYnl0ZXMh";
const BODY = '{"event_id":"evt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE","type":"run.succeeded"}';
const TIMESTAMP = 1_790_000_000;
const HEADER = "t=1790000000,v1=9852831d03e3f0aa4d104a985bb8b53967170389ff827d1d3a6492fe2d2b4adb";

async function reason(promise: Promise<void>): Promise<string> {
  try {
    await promise;
    return "ok";
  } catch (error) {
    return error instanceof WebhookSignatureError ? error.reason : "other";
  }
}

describe("webhook signatures", () => {
  it("uses the contract generated from DOMAIN.md", () => {
    expect(WEBHOOK_SIGNATURE.header).toBe("quansio-signature");
    expect(WEBHOOK_SIGNATURE.headers).toContain("quansio-event-id");
    expect(WEBHOOK_SIGNATURE.scheme).toBe("v1");
    expect(WEBHOOK_SIGNATURE.toleranceSeconds).toBe(300);
    expect(SECRET.startsWith(WEBHOOK_SIGNATURE.secretPrefix)).toBe(true);
  });

  it("accepts the server's signature over the raw body, as text or bytes", async () => {
    expect(await reason(verifyWebhookSignature({ secret: SECRET, header: HEADER, body: BODY, now: TIMESTAMP }))).toBe("ok");
    const bytes = new TextEncoder().encode(BODY);
    expect(await reason(verifyWebhookSignature({ secret: SECRET, header: HEADER, body: bytes, now: TIMESTAMP + 299 }))).toBe("ok");
  });

  it("refuses another secret, another body, a stale time and a malformed header", async () => {
    expect(await reason(verifyWebhookSignature({ secret: "whsec_other", header: HEADER, body: BODY, now: TIMESTAMP }))).toBe("mismatch");
    expect(await reason(verifyWebhookSignature({ secret: SECRET, header: HEADER, body: `${BODY} `, now: TIMESTAMP }))).toBe("mismatch");
    expect(await reason(verifyWebhookSignature({ secret: SECRET, header: HEADER, body: BODY, now: TIMESTAMP + 301 }))).toBe("stale");
    expect(await reason(verifyWebhookSignature({ secret: SECRET, header: "v1=00", body: BODY, now: TIMESTAMP }))).toBe("malformed");
    expect(await reason(verifyWebhookSignature({ secret: SECRET, header: "t=1790000000,v1=zz", body: BODY, now: TIMESTAMP }))).toBe("mismatch");
  });

  it("accepts any listed signature, so a rotation window verifies", async () => {
    const rotated = `${HEADER},v1=${"0".repeat(64)}`;
    expect(parseWebhookSignature(rotated)?.signatures).toHaveLength(2);
    expect(await reason(verifyWebhookSignature({ secret: SECRET, header: rotated, body: BODY, now: TIMESTAMP }))).toBe("ok");
  });
});
