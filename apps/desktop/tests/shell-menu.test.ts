/**
 * The application menu, the tray and what closing the window does (ADP-004, DOMAIN.md 13.6): the menu is data built from
 * the bindings in force and the focused window, the tray exists on Windows and Linux only, and a window that cannot be
 * brought back is never hidden.
 */

import { inflateSync } from "node:zlib";

import { describe, expect, it } from "vitest";

import { acceleratorsOf, buildMenu, type MenuItem, type MenuState } from "../src/main/menu.js";
import { FIXED_BINDINGS, WINDOW_DEFAULTS, type WindowShortcutId } from "../src/main/shortcuts.js";
import { closeBehaviour, trayIconPng, trayMenu, wantsTray } from "../src/main/tray.js";
import { parseBinding, toAccelerator } from "../src/shortcut-grammar.js";

const BINDINGS: Record<WindowShortcutId, string | null> = { ...WINDOW_DEFAULTS };

function state(overrides: Partial<MenuState> = {}): MenuState {
  return { platform: "darwin", bindings: BINDINGS, focused: "main", maxBindingLength: 64, ...overrides };
}

function find(items: readonly MenuItem[], label: string): MenuItem | undefined {
  for (const item of items) {
    if (item.label === label) {
      return item;
    }
    const inner = item.submenu ? find(item.submenu, label) : undefined;
    if (inner) {
      return inner;
    }
  }
  return undefined;
}

describe("the application menu", () => {
  it("on macOS has the app menu with Settings and Check for Updates, then File, Edit, View, Window, Help", () => {
    const menu = buildMenu(state());
    expect(menu.map((item) => item.label)).toEqual(["Quansio", "File", "Edit", "View", "Window", "Help"]);
    const app = menu[0]?.submenu ?? [];
    expect(app.map((item) => item.label).filter(Boolean)).toEqual(expect.arrayContaining(["Settings…", "Check for Updates…", "Quit Quansio"]));
    expect(find(menu, "Settings…")?.action).toEqual({ kind: "command", command: { kind: "open-settings" } });
    expect(find(menu, "Check for Updates…")?.action).toEqual({ kind: "check-for-updates" });
    expect(find(menu, "Quit Quansio")?.action).toEqual({ kind: "quit" });
    expect(find(menu, "New Thread")?.action).toEqual({ kind: "command", command: { kind: "new-thread" } });
    expect(find(menu, "Find…")?.action).toEqual({ kind: "command", command: { kind: "find" } });
    expect(find(menu, "Zoom In")?.role).toBe("zoomIn");
  });

  it("on Windows and Linux has no app menu: Settings, updates and quit live under File", () => {
    for (const platform of ["win32", "linux"]) {
      const menu = buildMenu(state({ platform }));
      expect(menu.map((item) => item.label)).toEqual(["File", "Edit", "View", "Window", "Help"]);
      const file = menu[0]?.submenu ?? [];
      expect(file.map((item) => item.label)).toEqual(expect.arrayContaining(["New Thread", "Settings…", "Check for Updates…", "Quit Quansio"]));
    }
  });

  it("carries the bindings in force as accelerators, in the platform's spelling, and none for a cleared one", () => {
    const menu = buildMenu(state({ bindings: { ...BINDINGS, find: "mod+shift+f", new_thread: null } }));
    expect(find(menu, "Find…")?.accelerator).toBe("Command+Shift+F");
    expect(find(menu, "New Thread")?.accelerator).toBeUndefined();
    expect(find(menu, "Settings…")?.accelerator).toBe("Command+,");
    expect(find(buildMenu(state({ platform: "win32", bindings: { ...BINDINGS, find: "mod+shift+f" } })), "Find…")?.accelerator).toBe("Control+Shift+F");
  });

  it("is rebuilt per focused window: document commands act in the main window and pop-outs only", () => {
    const enabled = (focused: MenuState["focused"], label: string): boolean | undefined => find(buildMenu(state({ focused })), label)?.enabled;
    expect(enabled("main", "Open Thread in New Window")).toBe(true);
    expect(enabled("main", "Find…")).toBe(true);
    expect(enabled("popout", "New Thread")).toBe(true);
    expect(enabled("popout", "Find…")).toBe(false);
    expect(enabled("popout", "Open Thread in New Window")).toBe(false);
    expect(enabled(null, "New Thread")).toBe(true);
    for (const focused of ["quick_entry", "indicator"] as const) {
      expect(enabled(focused, "New Thread")).toBe(false);
      expect(enabled(focused, "Settings…")).toBe(false);
      expect(enabled(focused, "Find…")).toBe(false);
    }
  });

  it("drops every accelerator while a binding is recorded, so the key reaches the page", () => {
    const menu = buildMenu(state({ paused: true }));
    expect(acceleratorsOf(menu)).toEqual([]);
    // No role either: a role such as copy carries a default accelerator the platform would still honour.
    expect(JSON.stringify(menu)).not.toContain("role");
    for (const platform of ["win32", "linux"]) {
      expect(JSON.stringify(buildMenu(state({ platform, paused: true })))).not.toMatch(/role|accelerator/);
    }
    expect(acceleratorsOf(buildMenu(state())).length).toBeGreaterThan(10);
  });

  it("uses the fixed table for the menu's own accelerators, and no accelerator twice", () => {
    for (const platform of ["darwin", "win32", "linux"]) {
      const accelerators = acceleratorsOf(buildMenu(state({ platform })));
      expect(new Set(accelerators).size, platform).toBe(accelerators.length);
      const fixed = Object.values(FIXED_BINDINGS).map((binding) => toAccelerator(parseBinding(binding, 64) ?? { modifiers: [], key: "?" }, platform));
      for (const accelerator of fixed) {
        // `hide` exists only in the macOS app menu.
        if (accelerator !== toAccelerator({ modifiers: ["mod"], key: "h" }, platform) || platform === "darwin") {
          expect(accelerators, `${platform} ${accelerator}`).toContain(accelerator);
        }
      }
    }
  });

  it("no default window binding is a fixed menu binding, and the defaults are all valid", () => {
    for (const binding of Object.values(WINDOW_DEFAULTS)) {
      expect(Object.values(FIXED_BINDINGS)).not.toContain(binding);
      expect(parseBinding(binding, 64)).not.toBeNull();
    }
    for (const binding of Object.values(FIXED_BINDINGS)) {
      expect(parseBinding(binding, 64), binding).not.toBeNull();
    }
  });
});

describe("the tray", () => {
  it("exists on Windows and Linux only, with Open and Quit", () => {
    expect(wantsTray("win32")).toBe(true);
    expect(wantsTray("linux")).toBe(true);
    expect(wantsTray("darwin")).toBe(false);
    expect(trayMenu()).toEqual([
      { label: "Open Quansio", action: "open" },
      { label: "Quit Quansio", action: "quit" },
    ]);
  });

  it("hides the window to the tray when there is one, and minimizes it when there is not", () => {
    expect(closeBehaviour({ platform: "win32", trayAvailable: true, quitting: false })).toBe("hide");
    expect(closeBehaviour({ platform: "linux", trayAvailable: true, quitting: false })).toBe("hide");
    // No tray: a hidden window could not be brought back, so it is minimized instead.
    expect(closeBehaviour({ platform: "win32", trayAvailable: false, quitting: false })).toBe("minimize");
    expect(closeBehaviour({ platform: "linux", trayAvailable: false, quitting: false })).toBe("minimize");
    // macOS keeps the app in the Dock: the window hides, and the Dock icon shows it again.
    expect(closeBehaviour({ platform: "darwin", trayAvailable: false, quitting: false })).toBe("hide");
    // Quitting is quitting.
    for (const platform of ["win32", "linux", "darwin"]) {
      for (const trayAvailable of [true, false]) {
        expect(closeBehaviour({ platform, trayAvailable, quitting: true })).toBe("close");
      }
    }
  });

  it("draws its own icon: a valid RGBA PNG with a disc and a transparent corner", () => {
    const png = trayIconPng(32);
    expect([...png.subarray(0, 8)]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    expect(png.subarray(12, 16).toString("ascii")).toBe("IHDR");
    expect(png.readUInt32BE(16)).toBe(32);
    expect(png.readUInt32BE(20)).toBe(32);
    expect(png[24]).toBe(8);
    expect(png[25]).toBe(6);
    const length = png.readUInt32BE(33);
    expect(png.subarray(37, 41).toString("ascii")).toBe("IDAT");
    const pixels = inflateSync(png.subarray(41, 41 + length));
    expect(pixels.length).toBe(32 * (1 + 32 * 4));
    const alphaAt = (x: number, y: number): number => pixels[y * (1 + 32 * 4) + 1 + x * 4 + 3] ?? -1;
    expect(alphaAt(0, 0)).toBe(0);
    expect(alphaAt(16, 16)).toBe(255);
    expect(png.subarray(png.length - 8, png.length - 4).toString("ascii")).toBe("IEND");
  });
});
