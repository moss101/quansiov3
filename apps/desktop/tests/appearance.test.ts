/**
 * The theme preference: read from `/v1/me`'s effective settings for the workspace in view, and
 * applied to the document root (`data-theme`, `color-scheme`); "system" leaves it to the OS.
 */

import { describe, expect, it } from "vitest";

import { applyTheme, themeFromMe } from "../src/appearance.js";

const ME = {
  workspaces: [
    { id: "ws_a", effective_settings: { theme: "dark" } },
    { id: "ws_b", effective_settings: { theme: "light" } },
    { id: "ws_c", effective_settings: { theme: "neon" } },
    { id: "ws_d", effective_settings: {} },
  ],
};

describe("theme preference", () => {
  it("reads the workspace's effective theme and treats anything else as system", () => {
    expect(themeFromMe(ME, "ws_a")).toBe("dark");
    expect(themeFromMe(ME, "ws_b")).toBe("light");
    expect(themeFromMe(ME, "ws_c")).toBe("system");
    expect(themeFromMe(ME, "ws_d")).toBe("system");
    expect(themeFromMe(ME, "ws_missing")).toBe("system");
    expect(themeFromMe(ME, null)).toBe("system");
    expect(themeFromMe(null, "ws_a")).toBe("system");
  });

  it("sets data-theme and color-scheme for a choice and clears them for system", () => {
    const root = { dataset: {} as Record<string, string | undefined>, style: { colorScheme: "" } };
    applyTheme("dark", root as unknown as HTMLElement);
    expect(root.dataset["theme"]).toBe("dark");
    expect(root.style.colorScheme).toBe("dark");
    applyTheme("light", root as unknown as HTMLElement);
    expect(root.dataset["theme"]).toBe("light");
    applyTheme("system", root as unknown as HTMLElement);
    expect("theme" in root.dataset).toBe(false);
    expect(root.style.colorScheme).toBe("");
  });
});
