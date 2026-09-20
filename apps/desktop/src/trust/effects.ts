/**
 * Effects and the run timeline (APP-006, RUN-007): the Effect Ledger as it is.
 *
 * Every effect is shown with its class, tier, resource, target, the digest of what was
 * dispatched, its outcome and evidence, and its reconciliation record. Unsettled effects —
 * `DISPATCHED`, `OUTCOME_UNKNOWN`, `RECONCILING`, `RECONCILIATION_MANUAL` — are never
 * folded into "done": they are their own timeline state until the ledger settles them,
 * and `OUTCOME_UNKNOWN` is shown distinctly from `RECONCILING`.
 */

export type EffectStatus =
  | "PROPOSED"
  | "AUTHORIZED"
  | "RESERVED"
  | "DISPATCHED"
  | "SETTLED_SUCCESS"
  | "SETTLED_FAILED"
  | "OUTCOME_UNKNOWN"
  | "RECONCILING"
  | "RECONCILED_SUCCESS"
  | "RECONCILED_FAILED"
  | "RECONCILIATION_MANUAL"
  | "DENIED";

export const EFFECT_STATUSES: readonly EffectStatus[] = [
  "PROPOSED",
  "AUTHORIZED",
  "RESERVED",
  "DISPATCHED",
  "SETTLED_SUCCESS",
  "SETTLED_FAILED",
  "OUTCOME_UNKNOWN",
  "RECONCILING",
  "RECONCILED_SUCCESS",
  "RECONCILED_FAILED",
  "RECONCILIATION_MANUAL",
  "DENIED",
];

/** The user-facing resolution of an effect: what the timeline says about it. */
export type Resolution = "pending" | "in_flight" | "succeeded" | "failed" | "unknown" | "reconciling" | "needs_human" | "denied";

export interface EffectOutcome {
  readonly kind: string;
  readonly remoteRef: string | null;
  readonly evidenceIds: readonly string[];
}

export interface Reconciliation {
  readonly strategy: string;
  readonly attempts: number;
  readonly lastAt: string;
  readonly result: string | null;
}

export interface EffectView {
  readonly id: string;
  readonly workspaceId: string;
  readonly runId: string | null;
  readonly stepId: string | null;
  readonly toolCallId: string | null;
  readonly effectClass: string;
  readonly tier: number;
  readonly resource: { readonly kind: string; readonly selector: string };
  readonly target: { readonly kind: string; readonly id: string } | null;
  readonly paramsDigest: string;
  readonly approvalReceiptId: string | null;
  readonly status: EffectStatus;
  readonly unsettled: boolean;
  readonly terminal: boolean;
  readonly outcome: EffectOutcome | null;
  readonly reconciliation: Reconciliation | null;
  readonly dispatchedAt: string | null;
  readonly settledAt: string | null;
  readonly createdAt: string;
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

/** Decode `GET /v1/effects`. */
export function decodeEffects(payload: unknown): EffectView[] {
  const views: EffectView[] = [];
  for (const raw of list(record(payload)["effects"])) {
    const row = record(raw);
    const id = text(row["id"]);
    const status = text(row["status"]);
    if (!id || !status || !(EFFECT_STATUSES as readonly string[]).includes(status)) {
      continue;
    }
    const resource = record(row["resource"]);
    const target = record(row["target"]);
    const outcome = record(row["outcome"]);
    const reconciliation = record(row["reconciliation"]);
    const effectStatus = status as EffectStatus;
    views.push({
      id,
      workspaceId: text(row["workspace_id"]) ?? "",
      runId: text(row["run_id"]),
      stepId: text(row["step_id"]),
      toolCallId: text(row["tool_call_id"]),
      effectClass: text(row["effect_class"]) ?? "",
      tier: number(row["tier"]) ?? 0,
      resource: { kind: text(resource["kind"]) ?? "", selector: text(resource["selector"]) ?? "" },
      target: text(target["kind"]) ? { kind: text(target["kind"]) ?? "", id: text(target["id"]) ?? "" } : null,
      paramsDigest: text(row["params_digest"]) ?? "",
      approvalReceiptId: text(row["approval_receipt_id"]),
      status: effectStatus,
      // The server says whether it is unsettled; the page never infers "done" on its own.
      unsettled: row["unsettled"] === true || isUnsettled(effectStatus),
      terminal: row["terminal"] === true,
      outcome: text(outcome["kind"]) ? { kind: text(outcome["kind"]) ?? "", remoteRef: text(outcome["remote_ref"]), evidenceIds: list(outcome["evidence_ids"]).filter((item): item is string => typeof item === "string") } : null,
      reconciliation: text(reconciliation["strategy"])
        ? { strategy: text(reconciliation["strategy"]) ?? "", attempts: number(reconciliation["attempts"]) ?? 0, lastAt: text(reconciliation["last_at"]) ?? "", result: text(reconciliation["result"]) }
        : null,
      dispatchedAt: text(row["dispatched_at"]),
      settledAt: text(row["settled_at"]),
      createdAt: text(row["created_at"]) ?? "",
    });
  }
  return views;
}

/** Whether the status is one the ledger has not settled. */
export function isUnsettled(status: EffectStatus): boolean {
  return status === "DISPATCHED" || status === "OUTCOME_UNKNOWN" || status === "RECONCILING" || status === "RECONCILIATION_MANUAL";
}

/** The timeline's word for an effect; unknown and reconciling are never merged. */
export function resolution(effect: EffectView): Resolution {
  switch (effect.status) {
    case "PROPOSED":
    case "AUTHORIZED":
    case "RESERVED":
      return "pending";
    case "DISPATCHED":
      return "in_flight";
    case "SETTLED_SUCCESS":
    case "RECONCILED_SUCCESS":
      return "succeeded";
    case "SETTLED_FAILED":
    case "RECONCILED_FAILED":
      return "failed";
    case "OUTCOME_UNKNOWN":
      return "unknown";
    case "RECONCILING":
      return "reconciling";
    case "RECONCILIATION_MANUAL":
      return "needs_human";
    case "DENIED":
      return "denied";
    default:
      return "unknown";
  }
}

/** Effects that still need attention, oldest first: unresolved ones never disappear. */
export function unresolved(effects: readonly EffectView[]): readonly EffectView[] {
  return effects.filter((effect) => effect.unsettled).sort((a, b) => a.createdAt.localeCompare(b.createdAt));
}

export interface TimelineEntry {
  readonly at: string;
  readonly kind: "effect" | "approval" | "evidence";
  readonly id: string;
  readonly label: string;
  readonly resolution: Resolution | null;
  readonly runId: string | null;
}

/** One ordered timeline over effects (by creation) — the run's story as the ledger tells it. */
export function timeline(effects: readonly EffectView[]): readonly TimelineEntry[] {
  const entries: TimelineEntry[] = [];
  for (const effect of effects) {
    entries.push({ at: effect.createdAt, kind: "effect", id: effect.id, label: `${effect.effectClass} · ${effect.status}`, resolution: resolution(effect), runId: effect.runId });
    for (const evidenceId of effect.outcome?.evidenceIds ?? []) {
      entries.push({ at: effect.settledAt ?? effect.createdAt, kind: "evidence", id: evidenceId, label: `evidence for ${effect.effectClass}`, resolution: null, runId: effect.runId });
    }
  }
  return entries.sort((a, b) => a.at.localeCompare(b.at) || a.id.localeCompare(b.id));
}
