/**
 * The Settings form against its asynchronous reads: an unsaved edit survives a read of the
 * effective settings; a saved one gives way to what the server says is in effect.
 */

import { describe, expect, it } from "vitest";

import { afterSave, fieldsFromWorkspace, mergeLoaded, type SettingsFields } from "../src/settings-form.js";

const SERVER: SettingsFields = { name: "Acme", target: "cloud", retention: "365", theme: "system", autonomy: "standard", security: { telemetry: false, share_analytics: false } };

describe("settings form", () => {
  it("reads the fields from a workspace view", () => {
    expect(fieldsFromWorkspace({ name: "Acme", effective_settings: { execution_target: "local_capsule", retention_days: 30, theme: "dark", telemetry: true, autonomy_mode: "ask_every_action" } }, ["telemetry", "share_analytics"])).toEqual({
      name: "Acme",
      target: "local_capsule",
      retention: "30",
      theme: "dark",
      autonomy: "ask_every_action",
      security: { telemetry: true, share_analytics: false },
    });
    expect(fieldsFromWorkspace({ name: "B", effective_settings: {} }, [])).toEqual({ name: "B", target: "cloud", retention: "365", theme: "system", autonomy: "standard", security: {} });
  });

  it("keeps a field the person changed before the first read arrived", () => {
    const typed: SettingsFields = { name: "", target: "cloud", retention: "365", theme: "dark", autonomy: "standard", security: {} };
    expect(mergeLoaded(typed, SERVER, new Set(["theme"]))).toEqual({ ...SERVER, theme: "dark" });
    expect(mergeLoaded(typed, SERVER, new Set())).toEqual(SERVER);
  });

  it("keeps an edited security control and takes the rest from the server", () => {
    const current: SettingsFields = { ...SERVER, security: { telemetry: true, share_analytics: true } };
    expect(mergeLoaded(current, SERVER, new Set(["security.telemetry"])).security).toEqual({ telemetry: true, share_analytics: false });
  });

  it("keeps an autonomy choice the person has not yet saved, whichever form it is in", () => {
    const typed: SettingsFields = { ...SERVER, autonomy: "read_only" };
    expect(mergeLoaded(typed, SERVER, new Set(["autonomy"])).autonomy).toBe("read_only");
    expect(mergeLoaded(typed, SERVER, new Set(["myAutonomy"])).autonomy).toBe("read_only");
    expect(mergeLoaded(typed, SERVER, new Set()).autonomy).toBe("standard");
  });

  it("clears an edit once saved, unless it changed again while the save was in flight", () => {
    const sent: SettingsFields = { ...SERVER, name: "Acme 2", retention: "30" };
    const now: SettingsFields = { ...sent, retention: "31" };
    const left = afterSave(new Set(["name", "retention", "theme"]), ["name", "retention", "target"], sent, now);
    expect([...left].sort()).toEqual(["retention", "theme"]);
    expect([...afterSave(new Set(["security.telemetry"]), ["security.telemetry"], { ...SERVER, security: { telemetry: true } }, { ...SERVER, security: { telemetry: true } })]).toEqual([]);
  });
});
