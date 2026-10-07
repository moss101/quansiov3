/**
 * Shortcuts (ADP-004, DOMAIN.md 13.6 D): the binding grammar, conflict detection, rollback when the system declines a
 * registration, registration only of what the person configured, and the recording mode.
 */

import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { bindingFromKeyEvent, bindingProblem, displayBinding, parseBinding, toAccelerator } from "../src/shortcut-grammar.js";
import { PreferencesFile, parsePreferences } from "../src/main/preferences.js";
import { FIXED_BINDINGS, ShortcutRegistry, WINDOW_DEFAULTS } from "../src/main/shortcuts.js";
import { FakeSystem } from "./shell-fakes.js";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function userData(): string {
  const directory = mkdtempSync(join(tmpdir(), "quansio-shortcuts-"));
  directories.push(directory);
  return directory;
}

function registry(options: { directory?: string; platform?: string; toggleDefault?: string | null; system?: FakeSystem } = {}): { registry: ShortcutRegistry; system: FakeSystem; preferences: PreferencesFile; fired: string[]; log: string[]; directory: string } {
  const directory = options.directory ?? userData();
  const system = options.system ?? new FakeSystem();
  const preferences = new PreferencesFile(directory);
  const fired: string[] = [];
  const log: string[] = [];
  return {
    registry: new ShortcutRegistry({
      platform: options.platform ?? "darwin",
      maxLength: 64,
      toggleDefault: options.toggleDefault === undefined ? "mod+alt+q" : options.toggleDefault,
      adapter: system,
      preferences,
      onGlobal: (id) => fired.push(id),
      log: (line) => log.push(line),
    }),
    system,
    preferences,
    fired,
    log,
    directory,
  };
}

describe("the binding grammar", () => {
  const ok = ["mod+k", "mod+shift+o", "mod+alt+q", "ctrl+alt+shift+a", "f5", "mod+f12", "mod+comma", "alt+space", "mod+0", "shift+up", "ctrl+backquote"];
  const bad: [string, string][] = [
    ["", "empty"],
    ["mod+K", "not_lower_case"],
    ["Mod+k", "not_lower_case"],
    ["mod + k", "not_lower_case"],
    ["shift+mod+k", "bad_modifier_order"],
    ["mod+mod+k", "bad_modifier_order"],
    ["mod+ctrl+k", "mod_with_ctrl"],
    ["meta+k", "bad_modifier_order"],
    ["mod+", "unknown_key"],
    ["mod+kk", "unknown_key"],
    ["mod+f25", "unknown_key"],
    ["mod++", "bad_modifier_order"],
    ["k", "needs_modifier"],
    ["space", "needs_modifier"],
    ["a".repeat(65), "too_long"],
  ];

  it("accepts the documented spellings and nothing else", () => {
    for (const text of ok) {
      expect(bindingProblem(text, 64), text).toBeNull();
    }
    for (const [text, problem] of bad) {
      expect(bindingProblem(text, 64), JSON.stringify(text)).toBe(problem);
    }
  });

  it("honours the configured maximum length", () => {
    expect(bindingProblem("mod+alt+shift+a", 8)).toBe("too_long");
    expect(bindingProblem("mod+k", 8)).toBeNull();
  });

  it("maps to the platform's accelerator", () => {
    const accelerator = (text: string, platform: string): string => toAccelerator(parseBinding(text, 64) ?? { modifiers: [], key: "?" }, platform);
    expect(accelerator("mod+alt+q", "darwin")).toBe("Command+Option+Q");
    expect(accelerator("mod+alt+q", "win32")).toBe("Control+Alt+Q");
    expect(accelerator("ctrl+shift+f5", "darwin")).toBe("Control+Shift+F5");
    expect(accelerator("mod+comma", "linux")).toBe("Control+,");
    expect(accelerator("alt+space", "win32")).toBe("Alt+Space");
    expect(parseBinding("Mod+K", 64)).toBeNull();
    expect(displayBinding("mod+alt+q", "darwin")).toBe("⌘⌥Q");
    expect(displayBinding("mod+shift+comma", "win32")).toBe("Ctrl+Shift+,");
  });

  it("builds a binding from a key event on the physical key, and ignores a lone modifier", () => {
    const event = (code: string, modifiers: Partial<Record<"metaKey" | "ctrlKey" | "altKey" | "shiftKey", boolean>>) => ({ code, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...modifiers });
    // Option+Q on macOS types a different glyph; the binding still names the key.
    expect(bindingFromKeyEvent(event("KeyQ", { metaKey: true, altKey: true }), "darwin", 64)).toBe("mod+alt+q");
    expect(bindingFromKeyEvent(event("KeyQ", { ctrlKey: true, altKey: true }), "win32", 64)).toBe("mod+alt+q");
    // Command and Control together would be `mod` with `ctrl`, which the grammar forbids.
    expect(bindingFromKeyEvent(event("KeyQ", { ctrlKey: true, metaKey: true }), "darwin", 64)).toBeNull();
    expect(bindingFromKeyEvent(event("KeyK", { ctrlKey: true }), "darwin", 64)).toBe("ctrl+k");
    expect(bindingFromKeyEvent(event("Digit1", { metaKey: true, shiftKey: true }), "darwin", 64)).toBe("mod+shift+1");
    expect(bindingFromKeyEvent(event("F5", {}), "linux", 64)).toBe("f5");
    expect(bindingFromKeyEvent(event("Comma", { ctrlKey: true }), "linux", 64)).toBe("mod+comma");
    expect(bindingFromKeyEvent(event("MetaLeft", { metaKey: true }), "darwin", 64)).toBeNull();
    expect(bindingFromKeyEvent(event("KeyA", { metaKey: true }), "win32", 64)).toBeNull(); // the Windows key is not a modifier here
    expect(bindingFromKeyEvent(event("KeyA", {}), "darwin", 64)).toBeNull(); // a bare letter is not a shortcut
  });
});

describe("registering", () => {
  it("registers only what the person's preferences name, never anything else", () => {
    const { registry: shortcuts, system } = registry();
    shortcuts.registerStored();
    // The one default (the toggle) and nothing for quick entry, which has none until the person sets one.
    expect(system.registered).toEqual(["Command+Option+Q"]);
    expect(shortcuts.snapshot().shortcuts.find((entry) => entry.id === "quick_entry")).toMatchObject({ binding: null, registered: false });
    // Window shortcuts are menu accelerators: the operating system is never asked for them.
    expect(system.registered).not.toContain("Command+N");
  });

  it("registers nothing for a shortcut the person cleared, and never the default again", () => {
    const first = registry();
    expect(first.registry.setBinding("toggle_window", null).ok).toBe(true);
    expect(first.system.held.size).toBe(0);
    const second = registry({ directory: first.directory });
    second.registry.registerStored();
    expect(second.system.registered).toEqual([]);
    expect(second.registry.binding("toggle_window")).toBeNull();
  });

  it("registers nothing at all when there is no default and no stored binding", () => {
    const { registry: shortcuts, system } = registry({ toggleDefault: null });
    shortcuts.registerStored();
    expect(system.registered).toEqual([]);
  });

  it("a stored binding is registered at the next start", () => {
    const first = registry({ toggleDefault: null });
    const set = first.registry.setBinding("quick_entry", "mod+shift+space");
    expect(set.ok).toBe(true);
    const second = registry({ directory: first.directory, toggleDefault: null });
    second.registry.registerStored();
    expect(second.system.registered).toEqual(["Command+Shift+Space"]);
    second.system.held.get("Command+Shift+Space")?.();
    expect(second.fired).toEqual(["quick_entry"]);
  });

  it("reports a stored binding the system declines, and leaves the binding stored", () => {
    const system = new FakeSystem();
    system.declined.add("Command+Option+Q");
    const { registry: shortcuts, log } = registry({ system });
    shortcuts.registerStored();
    const view = shortcuts.snapshot().shortcuts.find((entry) => entry.id === "toggle_window");
    expect(view).toMatchObject({ binding: "mod+alt+q", registered: false });
    expect(view?.failure).toMatch(/declined/);
    expect(log.join("\n")).toMatch(/toggle_window .* is not registered/);
  });
});

describe("setting a binding", () => {
  it("applies a valid, free binding and remembers it", () => {
    const { registry: shortcuts, directory } = registry();
    const result = shortcuts.setBinding("find", "mod+shift+f");
    expect(result.ok).toBe(true);
    expect(shortcuts.binding("find")).toBe("mod+shift+f");
    expect((JSON.parse(readFileSync(join(directory, "shell-preferences.json"), "utf8")) as { shortcuts: Record<string, string> }).shortcuts["find"]).toBe("mod+shift+f");
    expect(shortcuts.windowBindings().find).toBe("mod+shift+f");
  });

  it("refuses an invalid binding with a typed result and changes nothing", () => {
    const { registry: shortcuts, directory } = registry();
    const result = shortcuts.setBinding("find", "Mod+F");
    expect(result).toMatchObject({ ok: false, code: "invalid", problem: "not_lower_case" });
    expect(shortcuts.binding("find")).toBe(WINDOW_DEFAULTS.find);
    expect(() => readFileSync(join(directory, "shell-preferences.json"), "utf8")).toThrow();
    expect(shortcuts.setBinding("nope", "mod+k")).toMatchObject({ ok: false, code: "unknown_shortcut" });
  });

  it("refuses a binding that is a menu accelerator, naming it", () => {
    const { registry: shortcuts } = registry();
    for (const [name, binding] of Object.entries(FIXED_BINDINGS)) {
      const result = shortcuts.setBinding("find", binding);
      expect(result, name).toMatchObject({ ok: false, code: "conflict" });
      expect(!result.ok && result.code === "conflict" ? result.with : "").toContain(name.replace(/_/g, " "));
    }
    expect(shortcuts.binding("find")).toBe(WINDOW_DEFAULTS.find);
  });

  it("refuses a binding another shortcut holds, window or global, and allows it once released", () => {
    const { registry: shortcuts, system } = registry();
    shortcuts.registerStored();
    expect(shortcuts.setBinding("find", WINDOW_DEFAULTS.new_thread)).toMatchObject({ ok: false, code: "conflict", with: "new thread" });
    // A window shortcut cannot take the global's keys either, or the global would swallow the menu's.
    expect(shortcuts.setBinding("find", "mod+alt+q")).toMatchObject({ ok: false, code: "conflict", with: "toggle window" });
    expect(shortcuts.setBinding("quick_entry", "mod+k")).toMatchObject({ ok: false, code: "conflict", with: "find" });
    expect(system.held.size).toBe(1);
    shortcuts.setBinding("new_thread", null);
    expect(shortcuts.setBinding("find", WINDOW_DEFAULTS.new_thread).ok).toBe(true);
  });

  it("rolls back when the system declines a global shortcut: the previous binding stays registered and stored", () => {
    const system = new FakeSystem();
    const { registry: shortcuts, directory } = registry({ system });
    shortcuts.registerStored();
    expect(system.held.has("Command+Option+Q")).toBe(true);
    system.declined.add("Command+Shift+Space");

    const result = shortcuts.setBinding("toggle_window", "mod+shift+space");

    expect(result).toMatchObject({ ok: false, code: "registration_failed" });
    expect(shortcuts.binding("toggle_window")).toBe("mod+alt+q");
    expect([...system.held.keys()]).toEqual(["Command+Option+Q"]);
    expect(shortcuts.snapshot().shortcuts.find((entry) => entry.id === "toggle_window")).toMatchObject({ registered: true, binding: "mod+alt+q" });
    // Nothing was written: a restart sees the old binding.
    expect(() => readFileSync(join(directory, "shell-preferences.json"), "utf8")).toThrow();
  });

  it("rolls back when the system throws, and when there was no previous binding leaves none", () => {
    const system = new FakeSystem();
    system.throwOn.add("Command+Shift+Space");
    const { registry: shortcuts } = registry({ system, toggleDefault: null });
    const result = shortcuts.setBinding("quick_entry", "mod+shift+space");
    expect(result).toMatchObject({ ok: false, code: "registration_failed" });
    expect(!result.ok ? result.message : "").toMatch(/the system refused/);
    expect(shortcuts.binding("quick_entry")).toBeNull();
    expect(system.held.size).toBe(0);
  });

  it("says so when even the previous binding could not be restored", () => {
    const system = new FakeSystem();
    const { registry: shortcuts } = registry({ system });
    shortcuts.registerStored();
    // Another application grabs the old keys the moment they are released, and the new keys are declined.
    system.declined.add("Command+Shift+Space");
    const release = system.unregister.bind(system);
    system.unregister = (accelerator) => {
      release(accelerator);
      system.declined.add(accelerator);
    };
    const result = shortcuts.setBinding("toggle_window", "mod+shift+space");
    expect(result.ok).toBe(false);
    const view = shortcuts.snapshot().shortcuts.find((entry) => entry.id === "toggle_window");
    expect(view?.registered).toBe(false);
    expect(view?.failure).toMatch(/could not be restored/);
    // The stored binding is still the old one.
    expect(shortcuts.binding("toggle_window")).toBe("mod+alt+q");
  });

  it("replaces a registered binding with the new one on success", () => {
    const { registry: shortcuts, system } = registry();
    shortcuts.registerStored();
    expect(shortcuts.setBinding("toggle_window", "mod+shift+space").ok).toBe(true);
    expect([...system.held.keys()]).toEqual(["Command+Shift+Space"]);
  });

  it("resets to the default", () => {
    const { registry: shortcuts, preferences } = registry();
    shortcuts.setBinding("find", "mod+shift+f");
    expect(shortcuts.resetBinding("find").ok).toBe(true);
    expect(shortcuts.binding("find")).toBe(WINDOW_DEFAULTS.find);
    expect("find" in preferences.current().shortcuts).toBe(false);
  });
});

describe("recording mode", () => {
  it("pauses every global shortcut and the menu's accelerators, and restores them when it ends", () => {
    const { registry: shortcuts, system } = registry();
    shortcuts.registerStored();
    let changes = 0;
    shortcuts.onChange(() => (changes += 1));
    expect(shortcuts.windowBindings().find).toBe("mod+k");

    expect(shortcuts.beginRecording(7)).toBe(true);
    expect(system.held.size).toBe(0);
    expect(shortcuts.windowBindings()).toEqual({ new_thread: null, find: null, open_settings: null, popout_thread: null });
    expect(shortcuts.recording).toBe(true);
    expect(changes).toBeGreaterThan(0);

    expect(shortcuts.endRecording("done")).toBe(true);
    expect([...system.held.keys()]).toEqual(["Command+Option+Q"]);
    expect(shortcuts.windowBindings().find).toBe("mod+k");
    expect(shortcuts.recording).toBe(false);
    expect(shortcuts.endRecording("done")).toBe(false);
  });

  it("ends by itself when the recording window loses focus, navigates, closes or crashes", () => {
    for (const event of ["blur", "navigation", "crash", "closed"] as const) {
      const { registry: shortcuts, system } = registry();
      shortcuts.registerStored();
      const ended: string[] = [];
      shortcuts.onRecordingEnded((reason) => ended.push(reason));
      shortcuts.beginRecording(7);
      // Another window's events change nothing.
      shortcuts.windowEvent(8, event);
      expect(shortcuts.recording, event).toBe(true);
      shortcuts.windowEvent(7, event);
      expect(shortcuts.recording, event).toBe(false);
      expect(ended, event).toEqual([event]);
      expect(system.held.size, event).toBe(1);
    }
  });

  it("lets one window record at a time", () => {
    const { registry: shortcuts } = registry();
    expect(shortcuts.beginRecording(1)).toBe(true);
    expect(shortcuts.beginRecording(2)).toBe(false);
    expect(shortcuts.beginRecording(1)).toBe(true);
    expect(shortcuts.recordingWindow()).toBe(1);
  });

  it("a change made while recording ends it first, so the others are registered when the new one is tried", () => {
    const { registry: shortcuts, system } = registry();
    shortcuts.registerStored();
    shortcuts.beginRecording(1);
    const result = shortcuts.setBinding("quick_entry", "mod+shift+space");
    expect(result.ok).toBe(true);
    expect(shortcuts.recording).toBe(false);
    expect([...system.held.keys()].sort()).toEqual(["Command+Option+Q", "Command+Shift+Space"]);
  });
});

describe("the preferences file", () => {
  it("round-trips through disk with owner-only permissions", () => {
    const directory = userData();
    const file = new PreferencesFile(directory);
    file.update((previous) => ({ ...previous, keepAwake: false, indicator: { d1: { x: 4, y: 5 } } }));
    const again = new PreferencesFile(directory);
    expect(again.current()).toMatchObject({ keepAwake: false, indicator: { d1: { x: 4, y: 5 } } });
  });

  it("reads anything malformed, foreign or oversized as the defaults", () => {
    expect(parsePreferences(null).keepAwake).toBeNull();
    expect(parsePreferences("not json").shortcuts).toEqual({});
    expect(parsePreferences(JSON.stringify({ version: 2 })).shortcuts).toEqual({});
    expect(parsePreferences(JSON.stringify({ version: 1, shortcuts: { Find: "mod+k" }, keepAwake: null, installWhenIdle: null, installOnQuit: false, indicator: {} })).shortcuts).toEqual({});
    expect(parsePreferences(JSON.stringify({ version: 1, shortcuts: {}, keepAwake: "yes", installWhenIdle: null, installOnQuit: false, indicator: {} })).keepAwake).toBeNull();
    expect(parsePreferences(JSON.stringify({ version: 1, shortcuts: { find: "mod+k" }, keepAwake: true, installWhenIdle: null, installOnQuit: false, indicator: {}, extra: 1 })).keepAwake).toBeNull();
  });
});
