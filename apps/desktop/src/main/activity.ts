/**
 * Activity items (ADP-004, DOMAIN.md §13.6 C): what the person should look at, derived from durable events.
 *
 * The tracker is a projection of the stream and holds no authority: every item is rebuilt from RuntimeEvents
 * (and, at start-up, the approvals the server lists), so losing the tracker loses a list, not state. Items are
 * ordered waiting-for-user, running, error, completed. "Running" is Turn state, from `turn.started` to
 * `turn.completed`/`turn.aborted`, never Run status: a Run stays `RUNNING` between Turns, so reading it would show
 * "working" forever after the last reply.
 *
 * Titles are fixed product strings chosen from the item's kind. Nothing from an event's payload (a prompt, a
 * consequence preview) is copied into an item, because the indicator is a floating always-on-top surface and
 * payload text can be untrusted.
 */

import { t, type StringKey } from "../i18n/strings.js";

export type ActivityKind = "needs_approval" | "needs_answer" | "running" | "blocked" | "failed" | "completed";
export type ActivityGroup = "waiting" | "running" | "error" | "completed";

export interface ActivityRef {
  readonly runId?: string;
  readonly approvalId?: string;
  readonly questionId?: string;
  readonly turnId?: string;
}

export interface ActivityItem {
  readonly key: string;
  readonly kind: ActivityKind;
  readonly group: ActivityGroup;
  readonly title: string;
  readonly ref: ActivityRef;
  /** When the shell first saw it, in milliseconds since the epoch. */
  readonly since: number;
}

const GROUP_OF: Readonly<Record<ActivityKind, ActivityGroup>> = {
  needs_approval: "waiting",
  needs_answer: "waiting",
  running: "running",
  blocked: "error",
  failed: "error",
  completed: "completed",
};

const GROUP_ORDER: readonly ActivityGroup[] = ["waiting", "running", "error", "completed"];

const TITLE_OF: Readonly<Record<ActivityKind, StringKey>> = {
  needs_approval: "activityNeedsApproval",
  needs_answer: "activityNeedsAnswer",
  running: "activityRunning",
  blocked: "activityBlocked",
  failed: "activityFailed",
  completed: "activityCompleted",
};

export interface ActivityOptions {
  readonly maxItems: number;
  readonly completedTtlMs: number;
  readonly now: () => number;
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 && value.length <= 128 ? value : null;
}

/** The order the indicator lists items in. */
export function compareActivity(left: ActivityItem, right: ActivityItem): number {
  const byGroup = GROUP_ORDER.indexOf(left.group) - GROUP_ORDER.indexOf(right.group);
  return byGroup !== 0 ? byGroup : left.since - right.since || left.key.localeCompare(right.key);
}

export class ActivityTracker {
  readonly #options: ActivityOptions;
  readonly #items = new Map<string, ActivityItem>();
  readonly #dismissed = new Set<string>();
  readonly #listeners = new Set<() => void>();

  constructor(options: ActivityOptions) {
    this.#options = options;
  }

  onChange(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #emit(): void {
    for (const listener of this.#listeners) {
      listener();
    }
  }

  #add(key: string, kind: ActivityKind, ref: ActivityRef): boolean {
    if (this.#dismissed.has(key)) {
      return false;
    }
    const existing = this.#items.get(key);
    if (existing?.kind === kind) {
      return false;
    }
    this.#items.set(key, { key, kind, group: GROUP_OF[kind], title: t(TITLE_OF[kind]), ref, since: existing?.since ?? this.#options.now() });
    this.#evict();
    return true;
  }

  #remove(key: string): boolean {
    return this.#items.delete(key);
  }

  /** Remove every item of a run for which `predicate` holds. */
  #removeRun(runId: string, predicate: (item: ActivityItem) => boolean): boolean {
    let changed = false;
    for (const item of [...this.#items.values()]) {
      if (item.ref.runId === runId && predicate(item)) {
        changed = this.#items.delete(item.key) || changed;
      }
    }
    return changed;
  }

  /** Keep the list within its bound: completed go first, then errors, then running; waiting items never. */
  #evict(): void {
    while (this.#items.size > this.#options.maxItems) {
      const victim = [...this.#items.values()]
        .filter((item) => item.group !== "waiting")
        .sort((left, right) => GROUP_ORDER.indexOf(right.group) - GROUP_ORDER.indexOf(left.group) || left.since - right.since)[0];
      if (!victim) {
        return;
      }
      this.#items.delete(victim.key);
    }
  }

  /** Apply one durable event; true when the list changed. */
  apply(event: Record<string, unknown>): boolean {
    const type = typeof event["type"] === "string" ? event["type"] : "";
    const payload = event["payload"] !== null && typeof event["payload"] === "object" ? (event["payload"] as Record<string, unknown>) : {};
    const aggregate = text(event["aggregate_id"]);
    let changed = false;
    switch (type) {
      case "approval.requested": {
        const id = text(payload["approval_id"]) ?? aggregate;
        if (id) {
          const runId = text(payload["run_id"]);
          changed = this.#add(`approval:${id}`, "needs_approval", { approvalId: id, ...(runId ? { runId } : {}) });
        }
        break;
      }
      case "approval.granted":
      case "approval.denied":
      case "approval.expired":
      case "approval.superseded": {
        const id = text(payload["approval_id"]) ?? aggregate;
        changed = id ? this.#remove(`approval:${id}`) : false;
        break;
      }
      case "question.asked": {
        const id = text(payload["question_id"]) ?? aggregate;
        if (id) {
          const runId = text(payload["run_id"]);
          changed = this.#add(`question:${id}`, "needs_answer", { questionId: id, ...(runId ? { runId } : {}) });
        }
        break;
      }
      case "turn.started": {
        const turnId = text(payload["turn_id"]) ?? aggregate;
        if (turnId) {
          const runId = text(payload["run_id"]);
          changed = this.#add(`turn:${turnId}`, "running", { turnId, ...(runId ? { runId } : {}) });
        }
        break;
      }
      case "turn.completed":
      case "turn.aborted": {
        const turnId = text(payload["turn_id"]) ?? aggregate;
        changed = turnId ? this.#remove(`turn:${turnId}`) : false;
        break;
      }
      case "run.succeeded":
      case "run.failed":
      case "run.blocked":
      case "run.cancelled": {
        const runId = aggregate ?? text(payload["run_id"]);
        if (runId) {
          // A finished run has no Turn in flight and nothing left to answer or approve.
          changed = this.#removeRun(runId, (item) => item.group === "running" || item.group === "waiting");
          if (type === "run.failed") {
            changed = this.#add(`run:${runId}`, "failed", { runId }) || changed;
          } else if (type === "run.blocked") {
            changed = this.#add(`run:${runId}`, "blocked", { runId }) || changed;
          } else if (type === "run.succeeded") {
            changed = this.#add(`run:${runId}`, "completed", { runId }) || changed;
          }
        }
        break;
      }
      default:
        if (type.startsWith("question.")) {
          const id = text(payload["question_id"]) ?? aggregate;
          changed = id ? this.#remove(`question:${id}`) : false;
        }
    }
    if (changed) {
      this.#emit();
    }
    return changed;
  }

  /** Add the approvals the server lists as pending (at start-up, before any event has been replayed). */
  hydrateApprovals(approvals: readonly { readonly id: string; readonly runId: string | null }[]): void {
    let changed = false;
    for (const approval of approvals) {
      changed = this.#add(`approval:${approval.id}`, "needs_approval", { approvalId: approval.id, ...(approval.runId ? { runId: approval.runId } : {}) }) || changed;
    }
    if (changed) {
      this.#emit();
    }
  }

  /** Drop finished items older than their lifetime; true when the list changed. */
  expire(): boolean {
    const cutoff = this.#options.now() - this.#options.completedTtlMs;
    let changed = false;
    for (const item of [...this.#items.values()]) {
      if (item.group === "completed" && item.since < cutoff) {
        changed = this.#items.delete(item.key) || changed;
      }
    }
    if (changed) {
      this.#emit();
    }
    return changed;
  }

  /** The person dismissed an item: it is gone and a replay of the same event does not bring it back. */
  dismiss(key: string): boolean {
    this.#dismissed.add(key);
    const removed = this.#items.delete(key);
    if (removed) {
      this.#emit();
    }
    return removed;
  }

  get(key: string): ActivityItem | null {
    return this.#items.get(key) ?? null;
  }

  /** The items in display order. */
  items(): readonly ActivityItem[] {
    return [...this.#items.values()].sort(compareActivity);
  }

  /** How many Turns are executing (the keep-awake input and a quit-confirmation input). */
  runningCount(): number {
    return [...this.#items.values()].filter((item) => item.kind === "running").length;
  }

  /** How many items wait for the person. */
  waitingCount(): number {
    return [...this.#items.values()].filter((item) => item.group === "waiting").length;
  }
}
