/**
 * What the shell's windows read from main and how they show it (ADP-004): pure decoders and presentation
 * helpers shared by the renderer's four window kinds, kept out of the React files so they can be tested alone.
 *
 * Every payload main pushes or answers is decoded here as untrusted data: a malformed one becomes "nothing",
 * never an exception in the page.
 */

import type { StringKey } from "./i18n/strings.js";

export type WindowKind = "main" | "popout" | "quick_entry" | "indicator";

/** What a window learns about itself from main (never from its URL). */
export interface WindowContext {
  readonly type: WindowKind;
  readonly platform: string;
  readonly thread: { readonly threadId: string; readonly tab: "conversation" | "inspector"; readonly sidebar: boolean } | null;
}

function record(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

/** The `result` of a shell channel's `{ok: true, result}` answer; `null` for a refusal or anything else. */
export function shellResult(answer: unknown): Record<string, unknown> | null {
  const body = record(answer);
  return body["ok"] === true ? record(body["result"]) : null;
}

/** The refusal in a shell channel's answer, as text, or `null` when it succeeded. */
export function shellRefusal(answer: unknown): { readonly code: string; readonly message: string; readonly details: Record<string, unknown> } | null {
  const body = record(answer);
  if (body["ok"] === true) {
    return null;
  }
  const error = record(body["error"]);
  return {
    code: typeof error["code"] === "string" ? error["code"] : "INTERNAL",
    message: typeof error["message"] === "string" ? error["message"] : "",
    details: record(error["details"]),
  };
}

/** Decode `shell.window.context`. */
export function decodeWindowContext(answer: unknown): WindowContext | null {
  const result = shellResult(answer);
  if (result === null) {
    return null;
  }
  const type = result["type"];
  if (type !== "main" && type !== "popout" && type !== "quick_entry" && type !== "indicator") {
    return null;
  }
  const launch = record(result["launch"]);
  const thread =
    type === "popout" && typeof launch["threadId"] === "string"
      ? { threadId: launch["threadId"], tab: launch["tab"] === "inspector" ? ("inspector" as const) : ("conversation" as const), sidebar: launch["sidebar"] === true }
      : null;
  return { type, platform: typeof result["platform"] === "string" ? result["platform"] : "", thread };
}

// ---- the activity list ----

export type ActivityGroup = "waiting" | "running" | "error" | "completed";

export interface ActivityRow {
  readonly key: string;
  readonly kind: string;
  readonly group: ActivityGroup;
  readonly title: string;
}

const GROUPS: readonly ActivityGroup[] = ["waiting", "running", "error", "completed"];

/** Decode a `shell.activity` push or a `shell.activity.list` answer into rows in the order main sent them. */
export function decodeActivity(payload: unknown): ActivityRow[] {
  const body = record(payload);
  const source = body["ok"] === true ? record(body["result"]) : body;
  const items = Array.isArray(source["items"]) ? (source["items"] as unknown[]) : [];
  const rows: ActivityRow[] = [];
  for (const item of items) {
    const entry = record(item);
    const key = entry["key"];
    const group = entry["group"];
    if (typeof key !== "string" || typeof group !== "string" || !(GROUPS as readonly string[]).includes(group)) {
      continue;
    }
    rows.push({
      key,
      kind: typeof entry["kind"] === "string" ? entry["kind"] : "",
      group: group as ActivityGroup,
      title: typeof entry["title"] === "string" ? entry["title"] : "",
    });
  }
  return rows;
}

export type RemoteAnswer = "view" | "control" | "deny" | "stop";

/** The answers the computer's owner is offered on a remote-control row (ADP-009): none on any other row. */
export function remoteAnswers(kind: string): readonly RemoteAnswer[] {
  switch (kind) {
    case "remote_ask_view":
      return ["view", "deny"];
    case "remote_ask_control":
      return ["view", "control", "deny"];
    case "remote_watching":
    case "remote_controlling":
      return ["stop"];
    default:
      return [];
  }
}

export const REMOTE_ANSWER_LABELS: Readonly<Record<RemoteAnswer, StringKey>> = {
  view: "remoteAllowView",
  control: "remoteAllowControl",
  deny: "remoteDeny",
  stop: "remoteStop",
};

/** Whether a row is a remote session: it is not dismissible, only answered or ended. */
export function isRemoteRow(kind: string): boolean {
  return kind.startsWith("remote_");
}

export const GROUP_HEADINGS: Readonly<Record<ActivityGroup, StringKey>> = {
  waiting: "activityGroupWaiting",
  running: "activityGroupRunning",
  error: "activityGroupError",
  completed: "activityGroupCompleted",
};

/** The rows grouped in display order (waiting for you, running, error, completed); empty groups are dropped. */
export function groupActivity(rows: readonly ActivityRow[]): { readonly group: ActivityGroup; readonly rows: readonly ActivityRow[] }[] {
  return GROUPS.map((group) => ({ group, rows: rows.filter((row) => row.group === group) })).filter((entry) => entry.rows.length > 0);
}

// ---- shortcuts ----

export interface ShortcutRow {
  readonly id: string;
  readonly scope: "window" | "global";
  readonly binding: string | null;
  readonly display: string | null;
  readonly registered: boolean;
  readonly failure: string | null;
}

export interface ShortcutsView {
  readonly shortcuts: readonly ShortcutRow[];
  readonly recording: boolean;
  readonly platform: string;
}

/** Decode `shell.shortcuts.get` or a `shell.shortcuts` push (the snapshot, bare or in a `result`). */
export function decodeShortcuts(payload: unknown, fallbackPlatform = ""): ShortcutsView | null {
  const body = record(payload);
  const source = body["ok"] === true ? record(body["result"]) : body;
  const snapshot = "snapshot" in source ? record(source["snapshot"]) : source;
  if (!Array.isArray(snapshot["shortcuts"])) {
    return null;
  }
  const shortcuts: ShortcutRow[] = [];
  for (const item of snapshot["shortcuts"] as unknown[]) {
    const entry = record(item);
    if (typeof entry["id"] !== "string" || (entry["scope"] !== "window" && entry["scope"] !== "global")) {
      continue;
    }
    shortcuts.push({
      id: entry["id"],
      scope: entry["scope"],
      binding: typeof entry["binding"] === "string" ? entry["binding"] : null,
      display: typeof entry["display"] === "string" ? entry["display"] : null,
      registered: entry["registered"] === true,
      failure: typeof entry["failure"] === "string" ? entry["failure"] : null,
    });
  }
  return { shortcuts, recording: snapshot["recording"] === true, platform: typeof source["platform"] === "string" ? source["platform"] : fallbackPlatform };
}

export const SHORTCUT_LABELS: Readonly<Record<string, StringKey>> = {
  new_thread: "shortcutNewThread",
  find: "shortcutFind",
  open_settings: "shortcutOpenSettings",
  popout_thread: "shortcutPopOut",
  toggle_window: "shortcutToggleWindow",
  quick_entry: "shortcutQuickEntry",
};

/** The sentence for a refused shortcut change (the previous binding is in force after each). */
export function shortcutFailureKey(code: string): StringKey {
  return code === "conflict" ? "shortcutConflict" : code === "registration_failed" ? "shortcutRegistrationFailed" : "shortcutInvalid";
}

// ---- connectivity ----

/** The `state` of a `shell.connectivity` push. */
export function connectivityOf(payload: unknown): "online" | "offline_pending" | "offline" | null {
  const state = record(payload)["state"];
  return state === "online" || state === "offline_pending" || state === "offline" ? state : null;
}

// ---- pending events ----

export interface PendingEvent {
  readonly channel: string;
  readonly payload: unknown;
}

/** Decode `shell.pending.consume`. */
export function decodePending(answer: unknown): PendingEvent[] {
  const result = shellResult(answer);
  const events = result && Array.isArray(result["events"]) ? (result["events"] as unknown[]) : [];
  return events.flatMap((event) => {
    const entry = record(event);
    return typeof entry["channel"] === "string" ? [{ channel: entry["channel"], payload: entry["payload"] }] : [];
  });
}
