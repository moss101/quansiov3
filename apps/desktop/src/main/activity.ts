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

export type ActivityKind =
  | "needs_approval"
  | "needs_answer"
  | "running"
  | "blocked"
  | "failed"
  | "completed"
  // Remote control of this computer (ADP-009, DOMAIN.md 8.6.2): asked for, and standing.
  | "remote_ask_view"
  | "remote_ask_control"
  | "remote_watching"
  | "remote_controlling";
export type ActivityGroup = "waiting" | "running" | "error" | "completed";

export interface ActivityRef {
  readonly runId?: string;
  readonly approvalId?: string;
  readonly questionId?: string;
  readonly turnId?: string;
  /** A remote session: the computer and the viewer (ids only). */
  readonly targetId?: string;
  readonly viewerId?: string;
  /** When a remote session stops standing by itself, in milliseconds since the epoch. */
  readonly untilMs?: number;
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
  remote_ask_view: "waiting",
  remote_ask_control: "waiting",
  remote_watching: "waiting",
  remote_controlling: "waiting",
};

const GROUP_ORDER: readonly ActivityGroup[] = ["waiting", "running", "error", "completed"];

const TITLE_OF: Readonly<Record<ActivityKind, StringKey>> = {
  needs_approval: "activityNeedsApproval",
  needs_answer: "activityNeedsAnswer",
  running: "activityRunning",
  blocked: "activityBlocked",
  failed: "activityFailed",
  completed: "activityCompleted",
  remote_ask_view: "activityRemoteAskView",
  remote_ask_control: "activityRemoteAskControl",
  remote_watching: "activityRemoteWatching",
  remote_controlling: "activityRemoteControlling",
};

/** The kinds that are a remote session: they never leave the list by a dismissal, only when the session ends. */
export function isRemoteKind(kind: string): boolean {
  return kind.startsWith("remote_");
}

export interface ActivityOptions {
  readonly maxItems: number;
  readonly completedTtlMs: number;
  readonly now: () => number;
  /**
   * The signed-in person. A remote session is shown to the computer's owner; when this is known and is not the owner the event is
   * the viewer's own and shows nothing here. Unknown shows it: an indicator errs toward being visible.
   */
  readonly selfUserId?: () => string | null;
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
      case "target.remote_changed":
        changed = this.#remote(payload);
        break;
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

  /**
   * A remote session on this computer (DOMAIN.md 8.6.2): asked for, standing, or over. Nothing but ids, a scope and a state is read from
   * the payload; a state or scope it does not know shows nothing. A session whose time already passed (an event replayed after the
   * server forgot it) shows nothing either.
   */
  #remote(payload: Record<string, unknown>): boolean {
    const target = text(payload["target_id"]);
    const viewer = text(payload["viewer_user_id"]);
    const host = text(payload["host_user_id"]);
    if (target === null || viewer === null || host === null) {
      return false;
    }
    const key = `remote:${target}:${viewer}`;
    const self = this.#options.selfUserId?.() ?? null;
    if (self !== null && self !== host) {
      return false;
    }
    const state = payload["state"];
    const now = this.#options.now();
    const until = (field: string): number | null => {
      const value = payload[field];
      const at = typeof value === "string" ? Date.parse(value) : Number.NaN;
      return Number.isFinite(at) ? at : null;
    };
    if (state === "requested" || state === "authorized") {
      const bound = until(state === "requested" ? "consent_expires_at" : "ends_at");
      if (bound === null || bound <= now) {
        return this.#items.delete(key);
      }
      const scope = state === "requested" ? payload["requested_scope"] : payload["granted_scope"];
      if (scope !== "view" && scope !== "control") {
        return this.#items.delete(key);
      }
      const kind: ActivityKind = state === "requested" ? (scope === "control" ? "remote_ask_control" : "remote_ask_view") : scope === "control" ? "remote_controlling" : "remote_watching";
      return this.#set(key, kind, { targetId: target, viewerId: viewer, untilMs: bound });
    }
    return state === "stopped" ? this.#items.delete(key) : false;
  }

  /** Put an item that is not dismissible (a remote session): a change of kind replaces it. */
  #set(key: string, kind: ActivityKind, ref: ActivityRef): boolean {
    const existing = this.#items.get(key);
    if (existing?.kind === kind && existing.ref.untilMs === ref.untilMs) {
      return false;
    }
    this.#items.set(key, { key, kind, group: GROUP_OF[kind], title: t(TITLE_OF[kind]), ref, since: existing?.since ?? this.#options.now() });
    return true;
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
      // A remote session that ran out by itself (the server tells the owner when it does; this covers a missed event).
      if (isRemoteKind(item.kind) && item.ref.untilMs !== undefined && item.ref.untilMs <= this.#options.now()) {
        changed = this.#items.delete(item.key) || changed;
        continue;
      }
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
    // The indicator that this computer is being watched or controlled is not the person's to hide: only the session ending removes it.
    if (this.#items.get(key) !== undefined && isRemoteKind(this.#items.get(key)?.kind ?? "")) {
      return false;
    }
    this.#dismissed.add(key);
    const removed = this.#items.delete(key);
    if (removed) {
      this.#emit();
    }
    return removed;
  }

  get(key: string): ActivityItem | null {
    this.#pruneRemote();
    return this.#items.get(key) ?? null;
  }

  /** The items in display order. */
  items(): readonly ActivityItem[] {
    this.#pruneRemote();
    return [...this.#items.values()].sort(compareActivity);
  }

  /** A remote session past its time is not listed or answered, whether or not its end event arrived. */
  #pruneRemote(): void {
    const now = this.#options.now();
    for (const item of [...this.#items.values()]) {
      if (isRemoteKind(item.kind) && item.ref.untilMs !== undefined && item.ref.untilMs <= now) {
        this.#items.delete(item.key);
      }
    }
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
