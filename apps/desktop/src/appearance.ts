/**
 * The person's theme preference (APP-003): `UpdateUserSettings {theme}` stores system, light or
 * dark for the person in a workspace, and `/v1/me` returns it in the workspace's effective
 * settings. The page follows it through `data-theme` on the root element (the stylesheet keys
 * its dark tokens on it); "system" removes the attribute and the operating system decides.
 */

export type Theme = "system" | "light" | "dark";

const THEMES: readonly Theme[] = ["system", "light", "dark"];

/** The effective theme for a workspace in a `/v1/me` payload; anything unknown is "system". */
export function themeFromMe(me: unknown, workspaceId: string | null): Theme {
  const record = me && typeof me === "object" ? (me as Record<string, unknown>) : {};
  const workspaces = Array.isArray(record["workspaces"]) ? (record["workspaces"] as unknown[]) : [];
  const workspace = workspaces.find((entry) => entry && typeof entry === "object" && (entry as Record<string, unknown>)["id"] === workspaceId) as Record<string, unknown> | undefined;
  const settings = workspace?.["effective_settings"];
  const theme = settings && typeof settings === "object" ? (settings as Record<string, unknown>)["theme"] : undefined;
  return typeof theme === "string" && (THEMES as readonly string[]).includes(theme) ? (theme as Theme) : "system";
}

/** Apply a theme to the document root. */
export function applyTheme(theme: Theme, root: HTMLElement): void {
  if (theme === "system") {
    delete root.dataset["theme"];
    root.style.colorScheme = "";
  } else {
    root.dataset["theme"] = theme;
    root.style.colorScheme = theme;
  }
}
