/**
 * Approvals, effects and evidence timeline (APP-006).
 *
 * An approval is bound to the previewed payload digest. A changed payload cannot
 * be approved. OUTCOME_UNKNOWN effects stay unresolved until reconciliation.
 */

import { createHash } from "node:crypto";

export interface ApprovalPreview {
  readonly requestId: string;
  readonly payload: unknown;
  readonly digest: string;
  readonly untrustedOrigin: boolean;
  readonly escalationReason: string | null;
}

export interface EffectRow {
  readonly id: string;
  readonly status: string;
  readonly digest?: string;
}

export function digestPayload(payload: unknown): string {
  return createHash("sha256").update(JSON.stringify(payload)).digest("hex");
}

export function preview(requestId: string, payload: unknown, untrusted = false): ApprovalPreview {
  return {
    requestId,
    payload,
    digest: digestPayload(payload),
    untrustedOrigin: untrusted,
    escalationReason: untrusted ? "UNTRUSTED_EXTERNAL" : null,
  };
}

export function approve(previewed: ApprovalPreview, presented: unknown): { ok: true } | { ok: false; code: "APPROVAL_SUPERSEDED" } {
  if (digestPayload(presented) !== previewed.digest) {
    return { ok: false, code: "APPROVAL_SUPERSEDED" };
  }
  return { ok: true };
}

export function isUnresolved(effect: EffectRow): boolean {
  return effect.status === "OUTCOME_UNKNOWN" || effect.status === "RECONCILING";
}
