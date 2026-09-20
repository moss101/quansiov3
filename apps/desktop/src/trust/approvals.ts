/**
 * Approvals (APP-006, RUN-006): show exactly what Quansio wants to do and why, and bind
 * the decision to what was shown.
 *
 * The page decodes `GET /v1/approvals` into a preview — the effect's class and tier, the
 * resource and target, the tool and its arguments, the typed consequences (recipients,
 * amounts, targets, data classes), the untrusted origin and escalation when policy raised
 * the tier because the proposal's chain reached untrusted content, and the expiry. The
 * `params_digest` of that preview is what an `ApproveEffect` carries: a decision is
 * refused by the page before it is sent when the request it would approve is no longer
 * the one previewed (a different digest, or a status other than `requested`), and the
 * server refuses it again with `APPROVAL_SUPERSEDED`/`APPROVAL_EXPIRED`/`CONFLICT_STATE`.
 */

export type ApprovalStatus = "requested" | "granted" | "denied" | "expired" | "superseded";

export interface Amount {
  readonly currency: string;
  readonly minorUnits: number;
}

export interface UntrustedOrigin {
  readonly trust: string;
  readonly sourceRefs: readonly string[];
}

export interface ConsequencePreview {
  readonly recipients: readonly string[];
  readonly amounts: readonly Amount[];
  readonly targets: readonly string[];
  readonly dataClasses: readonly string[];
  readonly untrustedOrigin: UntrustedOrigin | null;
}

export interface ApprovalEffect {
  readonly effectClass: string;
  readonly tier: number | null;
  readonly resource: { readonly kind: string; readonly selector: string } | null;
  readonly target: { readonly kind: string; readonly id: string | null } | null;
  readonly status: string | null;
  readonly toolCallId: string | null;
  readonly stepId: string | null;
  readonly toolName: string | null;
  readonly args: unknown;
}

export interface Receipt {
  readonly id: string;
  readonly approverUserId: string;
  readonly grantedAt: string;
  readonly expiresAt: string;
  readonly consumed: boolean;
}

export interface ApprovalPreview {
  readonly id: string;
  readonly workspaceId: string;
  readonly runId: string;
  readonly effectId: string;
  readonly summary: string;
  readonly consequences: ConsequencePreview;
  /** The digest the decision binds to. */
  readonly digest: string;
  readonly status: ApprovalStatus;
  readonly expiresAt: string;
  readonly createdAt: string;
  readonly effect: ApprovalEffect | null;
  readonly receipts: readonly Receipt[];
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function number(value: unknown): number | null {
  return typeof value === "number" ? value : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function strings(value: unknown): string[] {
  return list(value).filter((item): item is string => typeof item === "string");
}

const STATUSES: readonly ApprovalStatus[] = ["requested", "granted", "denied", "expired", "superseded"];

/** Decode `GET /v1/approvals`. */
export function decodeApprovals(payload: unknown): ApprovalPreview[] {
  const previews: ApprovalPreview[] = [];
  for (const raw of list(record(payload)["approvals"])) {
    const row = record(raw);
    const id = text(row["id"]);
    const digest = text(row["params_digest"]);
    const runId = text(row["run_id"]);
    const effectId = text(row["effect_id"]);
    if (!id || !digest || !runId || !effectId) {
      continue;
    }
    const status = text(row["status"]);
    const preview = record(row["consequence_preview"]);
    const origin = record(preview["untrusted_origin"]);
    const effect = record(row["effect"]);
    const resource = record(effect["resource"]);
    const target = record(effect["target"]);
    previews.push({
      id,
      workspaceId: text(row["workspace_id"]) ?? "",
      runId,
      effectId,
      summary: text(row["summary"]) ?? "",
      consequences: {
        recipients: strings(preview["recipients"]),
        amounts: list(preview["amounts"]).map(record).flatMap((amount) => {
          const currency = text(amount["currency"]);
          const minorUnits = number(amount["minor_units"]);
          return currency && minorUnits !== null ? [{ currency, minorUnits }] : [];
        }),
        targets: strings(preview["targets"]),
        dataClasses: strings(preview["data_classes"]),
        untrustedOrigin: text(origin["trust"]) ? { trust: text(origin["trust"]) ?? "", sourceRefs: strings(origin["source_refs"]) } : null,
      },
      digest,
      status: status && (STATUSES as readonly string[]).includes(status) ? (status as ApprovalStatus) : "requested",
      expiresAt: text(row["expires_at"]) ?? "",
      createdAt: text(row["created_at"]) ?? "",
      effect: text(effect["class"])
        ? {
            effectClass: text(effect["class"]) ?? "",
            tier: number(effect["tier"]),
            resource: text(resource["kind"]) ? { kind: text(resource["kind"]) ?? "", selector: text(resource["selector"]) ?? "" } : null,
            target: text(target["kind"]) ? { kind: text(target["kind"]) ?? "", id: text(target["id"]) } : null,
            status: text(effect["status"]),
            toolCallId: text(effect["tool_call_id"]),
            stepId: text(effect["step_id"]),
            toolName: text(effect["tool_name"]),
            args: effect["args"] ?? null,
          }
        : null,
      receipts: list(row["receipts"]).map(record).flatMap((receipt) => {
        const receiptId = text(receipt["id"]);
        return receiptId
          ? [{ id: receiptId, approverUserId: text(receipt["approver_user_id"]) ?? "", grantedAt: text(receipt["granted_at"]) ?? "", expiresAt: text(receipt["expires_at"]) ?? "", consumed: receipt["consumed"] === true }]
          : [];
      }),
    });
  }
  return previews;
}

/** Why an approval preview deserves a warning, in the order the page shows them. */
export function warnings(preview: ApprovalPreview): readonly string[] {
  const found: string[] = [];
  if (preview.consequences.untrustedOrigin) {
    found.push(`untrusted origin: ${preview.consequences.untrustedOrigin.trust} (${preview.consequences.untrustedOrigin.sourceRefs.join(", ")})`);
  }
  if (preview.effect?.tier !== null && preview.effect?.tier !== undefined && preview.effect.tier >= 3) {
    found.push(`tier ${String(preview.effect.tier)} effect: a fresh approval is required every time`);
  }
  if (preview.consequences.amounts.length > 0) {
    found.push("moves money");
  }
  if (preview.consequences.dataClasses.length > 0) {
    found.push(`involves ${preview.consequences.dataClasses.join(", ")} data`);
  }
  return found;
}

/** Whether the preview can still be decided: requested and not past its expiry. */
export function isDecidable(preview: ApprovalPreview, now: number = Date.now()): boolean {
  return preview.status === "requested" && new Date(preview.expiresAt).getTime() > now;
}

export type Binding =
  | { readonly ok: true; readonly params: { readonly approval_request_id: string; readonly params_digest: string } }
  | { readonly ok: false; readonly code: "APPROVAL_SUPERSEDED" | "APPROVAL_EXPIRED" | "CONFLICT_STATE"; readonly reason: string };

/**
 * Bind a decision to what was previewed. `previewed` is the preview the user saw;
 * `current` is the same request as the page last loaded it. A changed digest, a status
 * other than `requested`, or a passed expiry refuses the decision here, before any
 * command is sent — the server enforces the same rule independently.
 */
export function bindApproval(previewed: ApprovalPreview, current: ApprovalPreview, now: number = Date.now()): Binding {
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
  return { ok: true, params: { approval_request_id: previewed.id, params_digest: previewed.digest } };
}
