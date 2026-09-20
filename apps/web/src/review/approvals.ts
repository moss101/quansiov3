/**
 * Approvals on the web (APP-012, the same contract APP-006 gives the desktop): a decision
 * is bound to the previewed `params_digest`; a request whose digest or status changed is
 * refused by the page before the command is sent and by the server again.
 */

import { list, number, record, strings, text } from "./wire.js";

export type ApprovalStatus = "requested" | "granted" | "denied" | "expired" | "superseded";

export interface ApprovalView {
  readonly id: string;
  readonly workspaceId: string;
  readonly runId: string;
  readonly effectId: string;
  readonly summary: string;
  readonly digest: string;
  readonly status: ApprovalStatus;
  readonly expiresAt: string;
  readonly createdAt: string;
  readonly effect: { readonly effectClass: string; readonly tier: number | null; readonly toolName: string | null; readonly resource: string | null; readonly args: unknown } | null;
  readonly consequences: { readonly recipients: readonly string[]; readonly targets: readonly string[]; readonly dataClasses: readonly string[]; readonly untrustedOrigin: string | null; readonly amounts: readonly string[] };
  readonly receipts: readonly { readonly id: string; readonly approverUserId: string; readonly consumed: boolean }[];
}

const STATUSES: readonly ApprovalStatus[] = ["requested", "granted", "denied", "expired", "superseded"];

/** Decode `GET /v1/approvals`. */
export function decodeApprovals(payload: unknown): ApprovalView[] {
  const views: ApprovalView[] = [];
  for (const raw of list(record(payload)["approvals"])) {
    const row = record(raw);
    const id = text(row["id"]);
    const digest = text(row["params_digest"]);
    const runId = text(row["run_id"]);
    const effectId = text(row["effect_id"]);
    const status = text(row["status"]);
    if (!id || !digest || !runId || !effectId || !status || !(STATUSES as readonly string[]).includes(status)) {
      continue;
    }
    const effect = record(row["effect"]);
    const resource = record(effect["resource"]);
    const preview = record(row["consequence_preview"]);
    const origin = record(preview["untrusted_origin"]);
    views.push({
      id,
      workspaceId: text(row["workspace_id"]) ?? "",
      runId,
      effectId,
      summary: text(row["summary"]) ?? "",
      digest,
      status: status as ApprovalStatus,
      expiresAt: text(row["expires_at"]) ?? "",
      createdAt: text(row["created_at"]) ?? "",
      effect: text(effect["class"])
        ? {
            effectClass: text(effect["class"]) ?? "",
            tier: number(effect["tier"]),
            toolName: text(effect["tool_name"]),
            resource: text(resource["kind"]) ? `${text(resource["kind"]) ?? ""} ${text(resource["selector"]) ?? ""}`.trim() : null,
            args: effect["args"] ?? null,
          }
        : null,
      consequences: {
        recipients: strings(preview["recipients"]),
        targets: strings(preview["targets"]),
        dataClasses: strings(preview["data_classes"]),
        untrustedOrigin: text(origin["trust"]),
        amounts: list(preview["amounts"]).map(record).flatMap((amount) => {
          const currency = text(amount["currency"]);
          const minor = number(amount["minor_units"]);
          return currency && minor !== null ? [`${String(minor / 100)} ${currency}`] : [];
        }),
      },
      receipts: list(row["receipts"]).map(record).flatMap((receipt) => {
        const receiptId = text(receipt["id"]);
        return receiptId ? [{ id: receiptId, approverUserId: text(receipt["approver_user_id"]) ?? "", consumed: receipt["consumed"] === true }] : [];
      }),
    });
  }
  return views;
}

/** Warnings shown above a decision, in order. */
export function warnings(view: ApprovalView): readonly string[] {
  const found: string[] = [];
  if (view.consequences.untrustedOrigin) {
    found.push(`untrusted origin: ${view.consequences.untrustedOrigin}`);
  }
  if (view.effect?.tier !== null && view.effect?.tier !== undefined && view.effect.tier >= 3) {
    found.push(`tier ${String(view.effect.tier)} effect: a fresh approval is required every time`);
  }
  if (view.consequences.amounts.length > 0) {
    found.push("moves money");
  }
  if (view.consequences.dataClasses.length > 0) {
    found.push(`involves ${view.consequences.dataClasses.join(", ")} data`);
  }
  return found;
}

export type Decision =
  | { readonly ok: true; readonly command: "ApproveEffect" | "DenyEffect"; readonly params: Record<string, string> }
  | { readonly ok: false; readonly code: "APPROVAL_SUPERSEDED" | "APPROVAL_EXPIRED" | "CONFLICT_STATE"; readonly reason: string };

/**
 * Bind a decision to what was previewed: `previewed` is what the user saw, `current` the
 * request as last loaded. A changed digest, a status other than `requested` or a passed
 * expiry refuses the decision here; the server enforces the same independently.
 */
export function decide(kind: "approve" | "deny", previewed: ApprovalView, current: ApprovalView, now: number = Date.now()): Decision {
  if (previewed.id !== current.id) {
    return { ok: false, code: "CONFLICT_STATE", reason: "the decision is for a different request" };
  }
  if (current.status === "superseded" || previewed.digest !== current.digest) {
    return { ok: false, code: "APPROVAL_SUPERSEDED", reason: "the payload changed after the preview; review the new request" };
  }
  if (current.status === "expired" || new Date(current.expiresAt).getTime() <= now) {
    return { ok: false, code: "APPROVAL_EXPIRED", reason: "the request expired before a decision" };
  }
  if (current.status !== "requested") {
    return { ok: false, code: "CONFLICT_STATE", reason: `the request is already ${current.status}` };
  }
  return kind === "approve"
    ? { ok: true, command: "ApproveEffect", params: { approval_request_id: previewed.id, params_digest: previewed.digest } }
    : { ok: true, command: "DenyEffect", params: { approval_request_id: previewed.id } };
}
