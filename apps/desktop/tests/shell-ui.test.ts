/**
 * What the shell's windows decode from main (ADP-004): every answer and push is data, and a malformed one is nothing.
 */

import { describe, expect, it } from "vitest";

import { connectivityOf, decodeActivity, decodePending, decodeShortcuts, decodeWindowContext, groupActivity, shellRefusal, shellResult, shortcutFailureKey } from "../src/shell-ui.js";

describe("shell answers", () => {
  it("reads the result of a success and the refusal of a failure", () => {
    expect(shellResult({ ok: true, result: { a: 1 } })).toEqual({ a: 1 });
    expect(shellResult({ ok: false, error: {} })).toBeNull();
    expect(shellResult("nope")).toBeNull();
    expect(shellResult(undefined)).toBeNull();
    expect(shellRefusal({ ok: true, result: {} })).toBeNull();
    expect(shellRefusal({ ok: false, error: { code: "conflict", message: "taken", details: { with: "find" } } })).toEqual({ code: "conflict", message: "taken", details: { with: "find" } });
    // An answer that is neither (an unexpected shape) is a refusal, not a success.
    expect(shellRefusal(undefined)).toMatchObject({ code: "INTERNAL" });
    expect(shellRefusal({ ok: false })).toMatchObject({ code: "INTERNAL", message: "" });
  });

  it("decodes the window context from main, and refuses a type main does not have", () => {
    expect(decodeWindowContext({ ok: true, result: { type: "main", platform: "darwin", launch: { type: "main" } } })).toEqual({ type: "main", platform: "darwin", thread: null });
    expect(decodeWindowContext({ ok: true, result: { type: "popout", platform: "win32", launch: { type: "popout", threadId: "thr_1", tab: "inspector", sidebar: true } } })).toEqual({ type: "popout", platform: "win32", thread: { threadId: "thr_1", tab: "inspector", sidebar: true } });
    expect(decodeWindowContext({ ok: true, result: { type: "admin", platform: "darwin", launch: {} } })).toBeNull();
    expect(decodeWindowContext({ ok: false, error: { code: "SCOPE_FORBIDDEN" } })).toBeNull();
    expect(decodeWindowContext(undefined)).toBeNull();
  });

  it("decodes the events that waited, dropping anything that is not one", () => {
    expect(decodePending({ ok: true, result: { events: [{ channel: "shell.command", payload: { kind: "find" } }, { payload: 1 }, "x"] } })).toEqual([{ channel: "shell.command", payload: { kind: "find" } }]);
    expect(decodePending({ ok: false })).toEqual([]);
  });

  it("reads a connectivity state and nothing else", () => {
    expect(connectivityOf({ state: "offline" })).toBe("offline");
    expect(connectivityOf({ state: "offline_pending" })).toBe("offline_pending");
    expect(connectivityOf({ state: "weird" })).toBeNull();
    expect(connectivityOf(null)).toBeNull();
  });
});

describe("activity rows", () => {
  it("decodes a push and an answer, keeps main's order, and drops what has no key or a group it does not know", () => {
    const items = [
      { key: "a", kind: "needs_approval", group: "waiting", title: "Needs your approval" },
      { key: "b", kind: "running", group: "running", title: "Working" },
      { kind: "running", group: "running" },
      { key: "c", group: "elsewhere" },
    ];
    expect(decodeActivity({ items }).map((row) => row.key)).toEqual(["a", "b"]);
    expect(decodeActivity({ ok: true, result: { items } }).map((row) => row.key)).toEqual(["a", "b"]);
    expect(decodeActivity(null)).toEqual([]);
    expect(groupActivity(decodeActivity({ items })).map((entry) => entry.group)).toEqual(["waiting", "running"]);
  });
});

describe("shortcut rows", () => {
  it("decodes the snapshot bare, in a result and in a change's result", () => {
    const snapshot = { shortcuts: [{ id: "find", scope: "window", binding: "mod+k", display: "⌘K", registered: false, failure: null }, { id: "quick_entry", scope: "global", binding: null, display: null, registered: false, failure: "x" }, { id: 1 }], recording: true };
    for (const payload of [snapshot, { ...snapshot, platform: "darwin" }, { snapshot }]) {
      const view = decodeShortcuts(payload, "darwin");
      expect(view?.shortcuts.map((row) => row.id)).toEqual(["find", "quick_entry"]);
      expect(view?.recording).toBe(true);
      expect(view?.platform).toBe("darwin");
    }
    expect(decodeShortcuts({ nothing: true })).toBeNull();
  });

  it("names the sentence for each refusal", () => {
    expect(shortcutFailureKey("conflict")).toBe("shortcutConflict");
    expect(shortcutFailureKey("registration_failed")).toBe("shortcutRegistrationFailed");
    expect(shortcutFailureKey("invalid")).toBe("shortcutInvalid");
  });
});
