/**
 * Notification preferences (APP-010). Channels: in-app, desktop, email.
 */

export type Channel = "in_app" | "desktop" | "email";

export interface NotificationPreferences {
  readonly channels: readonly Channel[];
}

export function prefers(prefs: NotificationPreferences, channel: Channel): boolean {
  return prefs.channels.includes(channel);
}
