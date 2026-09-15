/**
 * Externalized UI strings (APP-003). The renderer must not hard-code product copy.
 */

export const STRINGS = {
  appName: "Quansio",
  reconnecting: "Reconnecting to the runtime…",
  runRestored: "Restored the active run.",
  permissionDenied: "The renderer is not allowed to use this device permission.",
} as const;

export type StringKey = keyof typeof STRINGS;
