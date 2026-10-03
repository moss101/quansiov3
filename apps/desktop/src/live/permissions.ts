/**
 * The permission guide the native-computer surface shows (ADP-002, DOMAIN.md §8.6.1 E).
 *
 * When a run needs an OS permission it does not have, the runtime parks it (`SUSPENDED`,
 * `COMPUTER_PERMISSION_REQUIRED`, a `blocked` notification). The page does three things about that and
 * nothing more: it shows what is missing, it lets the person open the right Privacy pane (a click that
 * names a permission, never a URL), and it enables a Continue control — their own `ResumeRun` — only
 * when what the run needs reads `granted`. A lapsed foreground-control grant is a different answer: it
 * asks the person to stop using the keyboard and pointer for a moment and never offers Settings.
 *
 * Everything the page decodes here arrives from main or the stream and is treated as data.
 */

export type PermissionName = "accessibility" | "screen_recording" | "input_monitoring" | "microphone";
export type PermissionState = "granted" | "denied" | "not_determined" | "restricted";
export type GuideAction = "none" | "open_settings" | "contact_administrator";

const NAMES: readonly PermissionName[] = ["accessibility", "screen_recording", "input_monitoring", "microphone"];
const STATES: readonly PermissionState[] = ["granted", "denied", "not_determined", "restricted"];
const ACTIONS: readonly GuideAction[] = ["none", "open_settings", "contact_administrator"];

export interface PermissionRow {
  readonly permission: PermissionName;
  readonly state: PermissionState;
  readonly needed: boolean;
  readonly probed: boolean;
  readonly action: GuideAction;
}

export interface PermissionSnapshot {
  readonly applicable: boolean;
  readonly rows: readonly PermissionRow[];
  readonly idleSeconds: number | null;
  readonly pauseInput: boolean;
  readonly ready: boolean;
  readonly pollIntervalMs: number;
}

function record(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${what} must be an object`);
  }
  return value as Record<string, unknown>;
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[], what: string): T {
  if (typeof value !== "string" || !(allowed as readonly string[]).includes(value)) {
    throw new Error(`${what} is not one of ${allowed.join(", ")}`);
  }
  return value as T;
}

function flag(source: Record<string, unknown>, key: string): boolean {
  const value = source[key];
  if (typeof value !== "boolean") {
    throw new Error(`${key} must be a boolean`);
  }
  return value;
}

/** Decode main's snapshot strictly; a malformed one is an error, never a default of "granted". */
export function decodePermissionSnapshot(raw: unknown): PermissionSnapshot {
  const source = record(raw, "permission snapshot");
  const rows = source["rows"];
  if (!Array.isArray(rows)) {
    throw new Error("rows must be a list");
  }
  const idle = source["idleSeconds"];
  if (idle !== null && (typeof idle !== "number" || !Number.isFinite(idle) || idle < 0)) {
    throw new Error("idleSeconds must be null or a non-negative number");
  }
  const interval = source["pollIntervalMs"];
  if (typeof interval !== "number" || !Number.isInteger(interval) || interval < 500) {
    throw new Error("pollIntervalMs must be an integer of at least 500");
  }
  return {
    applicable: flag(source, "applicable"),
    rows: rows.map((entry) => {
      const row = record(entry, "row");
      return {
        permission: oneOf(row["permission"], NAMES, "permission"),
        state: oneOf(row["state"], STATES, "state"),
        needed: flag(row, "needed"),
        probed: flag(row, "probed"),
        action: oneOf(row["action"], ACTIONS, "action"),
      };
    }),
    idleSeconds: idle,
    pauseInput: flag(source, "pauseInput"),
    ready: flag(source, "ready"),
    pollIntervalMs: interval,
  };
}

// ------------------------------------------------------------------ the attention item

export type AttentionCode = "COMPUTER_PERMISSION_REQUIRED" | "COMPUTER_FOREGROUND_CONTROL_LAPSED";

/** What the runtime parked the run on (`AttentionRequest::to_json` in `quansio-machine`). */
export interface ComputerAttention {
  readonly code: AttentionCode;
  readonly permission: PermissionName | null;
  readonly status: PermissionState | null;
  readonly guide: { readonly action: "open_settings" | "contact_administrator" | "pause_input"; readonly pane: string | null };
}

const GUIDE_ACTIONS = ["open_settings", "contact_administrator", "pause_input"] as const;

/**
 * Parse the runtime's attention item strictly. Anything with another notification kind, another
 * resume command or an unknown guide is not shown: this page offers `ResumeRun` and nothing else.
 */
export function parseComputerAttention(raw: unknown): ComputerAttention {
  const source = record(raw, "attention");
  const allowed = ["code", "notification_kind", "resume_command", "permission", "status", "guide"];
  for (const key of Object.keys(source)) {
    if (!allowed.includes(key)) {
      throw new Error(`attention has an unknown field ${JSON.stringify(key)}`);
    }
  }
  if (source["notification_kind"] !== "blocked") {
    throw new Error("attention must be delivered as a blocked notification");
  }
  if (source["resume_command"] !== "ResumeRun") {
    throw new Error("attention may only be resumed with ResumeRun");
  }
  const code = oneOf(source["code"], ["COMPUTER_PERMISSION_REQUIRED", "COMPUTER_FOREGROUND_CONTROL_LAPSED"] as const, "code");
  const guide = record(source["guide"], "guide");
  const action = oneOf(guide["action"], GUIDE_ACTIONS, "guide.action");
  const pane = guide["pane"];
  if (action === "open_settings") {
    if (typeof pane !== "string" || !pane.startsWith("Privacy_")) {
      throw new Error("an open_settings guide names a Privacy pane");
    }
  } else if (pane !== undefined) {
    throw new Error(`a ${action} guide has no pane`);
  }
  const permission = source["permission"] === null ? null : oneOf(source["permission"], NAMES, "permission");
  const status = source["status"] === null ? null : oneOf(source["status"], STATES, "status");
  if (action === "pause_input" && (permission !== null || status !== null)) {
    throw new Error("a pause_input guide names no permission");
  }
  return { code, permission, status, guide: { action, pane: typeof pane === "string" ? pane : null } };
}

/**
 * The attention item an event carries, when it is a run parking event with a valid one. Events from
 * other runs and payloads that do not parse yield null: they are not this page's to act on.
 */
export function attentionFromEvent(event: Record<string, unknown>, runId: string | null): ComputerAttention | null {
  const type = typeof event["type"] === "string" ? event["type"] : "";
  if (runId === null || (type !== "run.suspended" && type !== "run.waiting")) {
    return null;
  }
  const payload = event["payload"];
  if (!payload || typeof payload !== "object") {
    return null;
  }
  const body = payload as Record<string, unknown>;
  if (body["run_id"] !== runId || body["attention"] === undefined) {
    return null;
  }
  try {
    return parseComputerAttention(body["attention"]);
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ what the page shows

export type GuideKind = "not_applicable" | "ready" | "open_settings" | "contact_administrator" | "pause_input";

export interface GuideView {
  readonly kind: GuideKind;
  /** The rows to list: what the run needs first, then the rest. */
  readonly rows: readonly PermissionRow[];
  /** Whether the person may resume the run now. */
  readonly canContinue: boolean;
  /** The permission the first Settings button opens, when the guide is `open_settings`. */
  readonly first: PermissionName | null;
}

/**
 * The guide for a snapshot and, when the runtime said why the run is parked, its attention item.
 * `canContinue` is the snapshot's own `ready`: the page never decides a permission is granted.
 */
export function guideView(snapshot: PermissionSnapshot, attention: ComputerAttention | null): GuideView {
  if (!snapshot.applicable) {
    return { kind: "not_applicable", rows: [], canContinue: true, first: null };
  }
  const rows = [...snapshot.rows].sort((a, b) => Number(b.needed) - Number(a.needed));
  if (attention?.guide.action === "pause_input") {
    return { kind: "pause_input", rows: [], canContinue: snapshot.ready, first: null };
  }
  const missing = rows.filter((row) => row.needed && row.state !== "granted");
  const settings = missing.find((row) => row.action === "open_settings");
  if (settings) {
    return { kind: "open_settings", rows, canContinue: false, first: settings.permission };
  }
  if (missing.some((row) => row.action === "contact_administrator")) {
    return { kind: "contact_administrator", rows, canContinue: false, first: null };
  }
  return { kind: "ready", rows, canContinue: snapshot.ready, first: null };
}

/** The `needs` and `pauseInput` a snapshot request carries for an attention item. */
export function requestFor(attention: ComputerAttention | null): { readonly needs: readonly PermissionName[]; readonly pauseInput: boolean } {
  if (attention === null) {
    return { needs: ["accessibility"], pauseInput: false };
  }
  if (attention.guide.action === "pause_input") {
    return { needs: [], pauseInput: true };
  }
  return { needs: attention.permission ? [attention.permission] : ["accessibility"], pauseInput: false };
}

/** The one command that leaves the parked state, and only the person sends it. */
export function resumeCommand(run: { readonly id: string; readonly generation: number }): { readonly name: "ResumeRun"; readonly params: { readonly run_id: string; readonly generation: number } } {
  if (!run.id.startsWith("run_")) {
    throw new Error("ResumeRun names a run_ run");
  }
  return { name: "ResumeRun", params: { run_id: run.id, generation: run.generation } };
}

// ------------------------------------------------------------------ polling

export interface Timers {
  readonly set: (callback: () => void, ms: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

const REAL_TIMERS: Timers = {
  set: (callback, ms) => setTimeout(callback, ms),
  clear: (handle) => {
    clearTimeout(handle as ReturnType<typeof setTimeout>);
  },
};

/**
 * Reads the snapshot, tells the page, and re-arms a timeout once the read has finished — never an
 * interval, so a slow read cannot overlap the next. A read that throws is reported and tried again.
 */
export class PermissionPoller {
  readonly #read: () => Promise<unknown>;
  readonly #onSnapshot: (snapshot: PermissionSnapshot) => void;
  readonly #onError: (error: unknown) => void;
  readonly #timers: Timers;
  #handle: unknown = null;
  #running = false;

  constructor(options: {
    readonly read: () => Promise<unknown>;
    readonly onSnapshot: (snapshot: PermissionSnapshot) => void;
    readonly onError?: (error: unknown) => void;
    readonly timers?: Timers;
  }) {
    this.#read = options.read;
    this.#onSnapshot = options.onSnapshot;
    this.#onError = options.onError ?? (() => undefined);
    this.#timers = options.timers ?? REAL_TIMERS;
  }

  get running(): boolean {
    return this.#running;
  }

  start(): void {
    if (!this.#running) {
      this.#running = true;
      void this.#tick(2000);
    }
  }

  stop(): void {
    this.#running = false;
    if (this.#handle !== null) {
      this.#timers.clear(this.#handle);
      this.#handle = null;
    }
  }

  async #tick(fallbackMs: number): Promise<void> {
    let next = fallbackMs;
    try {
      const snapshot = decodePermissionSnapshot(await this.#read());
      next = snapshot.pollIntervalMs;
      if (this.#running) {
        this.#onSnapshot(snapshot);
      }
    } catch (error) {
      if (this.#running) {
        this.#onError(error);
      }
    }
    if (this.#running) {
      this.#handle = this.#timers.set(() => {
        this.#handle = null;
        void this.#tick(next);
      }, next);
    }
  }
}
