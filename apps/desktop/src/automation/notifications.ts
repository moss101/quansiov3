/**
 * Notifications (APP-010, DOMAIN.md §13.3): the client half.
 *
 * The server makes notifications and decides their channels from the person's preference
 * for the workspace × kind; this module decodes the projection, builds the two commands,
 * keeps the in-app list current from `notification.*` events (which only the person they
 * name receives), and decides whether an event asks for a native desktop notification.
 */

export const NOTIFICATION_KINDS = ["needs_approval", "needs_answer", "blocked", "completed", "failed", "mention", "attention", "system"] as const;
export const NOTIFICATION_CHANNELS = ["in_app", "desktop", "email"] as const;

export type NotificationKind = (typeof NOTIFICATION_KINDS)[number];
export type Channel = (typeof NOTIFICATION_CHANNELS)[number];

export interface NotificationView {
  readonly id: string;
  readonly kind: string;
  readonly title: string;
  readonly ref: Record<string, unknown>;
  readonly channels: readonly string[];
  readonly delivered: readonly string[];
  readonly readAt: string | null;
  readonly createdAt: string | null;
}

export interface Preference {
  readonly kind: string;
  readonly channels: readonly string[];
  readonly chosen: boolean;
}

export interface NotificationCenter {
  readonly items: readonly NotificationView[];
  readonly unread: number;
  readonly preferences: readonly Preference[];
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function notificationOf(value: unknown): NotificationView {
  const source = record(value);
  return {
    id: typeof source["id"] === "string" ? source["id"] : "",
    kind: typeof source["kind"] === "string" ? source["kind"] : "system",
    title: typeof source["title"] === "string" ? source["title"] : "",
    ref: record(source["ref"]),
    channels: strings(source["channels"]),
    delivered: strings(source["channels_delivered"]),
    readAt: typeof source["read_at"] === "string" ? source["read_at"] : null,
    createdAt: typeof source["created_at"] === "string" ? source["created_at"] : null,
  };
}

/** Decode `GET /v1/notifications`. */
export function decodeNotifications(raw: unknown): NotificationCenter {
  const source = record(raw);
  const items = (Array.isArray(source["items"]) ? source["items"] : []).map(notificationOf);
  const preferences = (Array.isArray(source["preferences"]) ? source["preferences"] : []).map((entry) => {
    const pref = record(entry);
    return { kind: typeof pref["kind"] === "string" ? pref["kind"] : "", channels: strings(pref["channels"]), chosen: pref["chosen"] === true };
  });
  return { items, unread: items.filter((item) => item.readAt === null).length, preferences };
}

/** The in-app list shows what the person chose to see in the app. */
export function inAppItems(center: NotificationCenter): readonly NotificationView[] {
  return center.items.filter((item) => item.channels.includes("in_app"));
}

/** `UpdateNotificationPreferences` params, refusing kinds and channels the server does not know. */
export function preferenceParams(workspaceId: string, kind: string, channels: readonly string[]): Record<string, unknown> {
  if (!(NOTIFICATION_KINDS as readonly string[]).includes(kind)) {
    throw new Error(`${kind} is not a notification kind`);
  }
  for (const channel of channels) {
    if (!(NOTIFICATION_CHANNELS as readonly string[]).includes(channel)) {
      throw new Error(`${channel} is not a channel`);
    }
  }
  return { workspace_id: workspaceId, kind, channels: [...new Set(channels)] };
}

/** The channels after toggling one: the page's checkbox. Order follows NOTIFICATION_CHANNELS. */
export function toggleChannel(current: readonly string[], channel: Channel, on: boolean): string[] {
  const chosen = new Set(current.filter((item) => item !== channel));
  if (on) {
    chosen.add(channel);
  }
  return NOTIFICATION_CHANNELS.filter((item) => chosen.has(item));
}

/** Apply a `notification.created` / `notification.read` event naming `me` to the center. */
export function applyNotificationEvent(center: NotificationCenter, event: Record<string, unknown>, me: string | null): NotificationCenter {
  const type = typeof event["type"] === "string" ? event["type"] : "";
  const payload = record(event["payload"]);
  if (!type.startsWith("notification.") || me === null || payload["user_id"] !== me) {
    return center;
  }
  if (type === "notification.created") {
    const id = typeof payload["notification_id"] === "string" ? payload["notification_id"] : "";
    if (!id || center.items.some((item) => item.id === id)) {
      return center;
    }
    const channels = strings(payload["channels"]);
    const item: NotificationView = {
      id,
      kind: typeof payload["kind"] === "string" ? payload["kind"] : "system",
      title: typeof payload["title"] === "string" ? payload["title"] : "",
      ref: record(payload["ref"]),
      channels,
      delivered: channels.filter((channel) => channel !== "email"),
      readAt: null,
      createdAt: typeof event["occurred_at"] === "string" ? event["occurred_at"] : null,
    };
    const items = [item, ...center.items];
    return { ...center, items, unread: items.filter((entry) => entry.readAt === null).length };
  }
  if (type === "notification.read") {
    const items = center.items.map((item) => (item.id === payload["notification_id"] && item.readAt === null ? { ...item, readAt: new Date().toISOString() } : item));
    return { ...center, items, unread: items.filter((entry) => entry.readAt === null).length };
  }
  return center;
}

export interface DesktopNotice {
  readonly notificationId: string;
  readonly title: string;
  readonly body: string;
  /** What the notification points at (`thread_id`, `run_id`, `approval_id`…): where a click on it goes (ADP-004). */
  readonly ref: Readonly<Record<string, unknown>>;
}

/** The native notification a durable event asks for, or `null`: only `notification.created` with `desktop: true`. */
export function desktopNotice(event: Record<string, unknown>): DesktopNotice | null {
  if (event["type"] !== "notification.created") {
    return null;
  }
  const payload = record(event["payload"]);
  const id = payload["notification_id"];
  if (payload["desktop"] !== true || typeof id !== "string") {
    return null;
  }
  const kind = typeof payload["kind"] === "string" ? payload["kind"] : "system";
  const title = typeof payload["title"] === "string" && payload["title"] ? payload["title"] : "Quansio";
  return { notificationId: id, title, body: kind.replace(/_/g, " "), ref: record(payload["ref"]) };
}
