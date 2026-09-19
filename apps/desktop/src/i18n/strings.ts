/**
 * Externalized UI strings (APP-003). The renderer must not hard-code product copy; every
 * visible string is a key here so localization is a table swap, not a code change.
 */

export const STRINGS = {
  appName: "Quansio",
  connecting: "Connecting to the runtime…",
  connected: "Connected.",
  reconnecting: "Reconnecting to the runtime…",
  disconnected: "Disconnected from the runtime.",
  runRestored: "Restored the active run.",
  noActiveRun: "No active run.",
  activeRun: "Active run",
  workspace: "Workspace",
  permissionDenied: "The renderer is not allowed to use this device permission.",
  composerPlaceholder: "Message your teammates…",
  send: "Send",
  streamTitle: "Stream",
  streamEmpty: "Durable events and live frames appear here as they arrive.",
  surfaces: "Surfaces",
  updateUnavailable: "Updates are delivered with signed builds.",
  updateIdle: "Up to date.",
  updateChecking: "Checking for updates…",
  updateDownloaded: "An update is ready to install.",
  updateError: "Update check failed.",
  deepLinkOpened: "Opened from a link.",
  commandFailed: "The command was refused.",
  signIn: "Sign in",
  emailPlaceholder: "you@example.com",
  displayNamePlaceholder: "Your name (optional)",
  sendMagicLink: "Email me a sign-in link",
  continueWith: "Continue with",
  checkInbox: "Check your inbox",
  pasteToken: "Paste the sign-in token",
  continue: "Continue",
  chooseExecutionTarget: "Where should your work run?",
  securityDefaults: "Security defaults",
  confirm: "Confirm",
  signOut: "Sign out",
} as const;

export type StringKey = keyof typeof STRINGS;

/** Look up a string by key; unknown keys are a programming error, not a fallback. */
export function t(key: StringKey): string {
  return STRINGS[key];
}
