/**
 * ADP-002 permission onboarding (DOMAIN.md §8.6.1 E): the typed status, the guide the page shows, the
 * one click that opens a Privacy pane, and the rule that the agent never drives System Settings.
 *
 * The attention fixtures under `crates/machine/tests/fixtures/computer/` are the same files the Rust
 * suite compares against what the runtime writes, so the page parses exactly what the runtime emits.
 */

import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  DesktopBridge,
  PAUSE_IDLE_SECONDS,
  PERMISSION_NAMES,
  PERMISSION_POLL_MS,
  PRELOAD_ALLOWLIST,
  PermissionPoller,
  SETTINGS_PANES,
  SessionFile,
  UNPROBED_PERMISSIONS,
  UpdateHooks,
  attentionFromEvent,
  buildSnapshot,
  decodePermissionSnapshot,
  guideAction,
  guideView,
  mapMediaAccess,
  parseComputerAttention,
  parseOpenSettings,
  parsePermissionRequest,
  parsePermissionStatus,
  requestFor,
  resumeCommand,
  settingsUrl,
  type PermissionSource,
  type PermissionStatus,
} from "../src/index.js";

const ROOT = join(import.meta.dirname, "..", "..", "..");
const FIXTURES = join(ROOT, "crates", "machine", "tests", "fixtures", "computer");

function fixture(name: string): unknown {
  return JSON.parse(readFileSync(join(FIXTURES, name), "utf8")) as unknown;
}

const GRANTED: PermissionStatus = { accessibility: "granted", screen_recording: "granted", input_monitoring: "granted", microphone: "granted" };

const temporaryDirs: string[] = [];
afterEach(() => {
  for (const dir of temporaryDirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

// ------------------------------------------------------------------ the typed status

describe("permission status", () => {
  it("is exactly four permissions in four states, parsed strictly", () => {
    expect(parsePermissionStatus(fixture("permission_status.json"))).toEqual({
      accessibility: "denied",
      screen_recording: "granted",
      input_monitoring: "not_determined",
      microphone: "restricted",
    });
    for (const name of PERMISSION_NAMES) {
      const missing = Object.fromEntries(Object.entries(GRANTED).filter(([key]) => key !== name));
      expect(() => parsePermissionStatus(missing)).toThrow(/missing or not a known state/);
    }
    expect(() => parsePermissionStatus({ ...GRANTED, camera: "granted" })).toThrow(/unknown permission/);
    expect(() => parsePermissionStatus({ ...GRANTED, accessibility: "allowed" })).toThrow(/not a known state/);
    expect(() => parsePermissionStatus({ ...GRANTED, accessibility: true })).toThrow();
    expect(() => parsePermissionStatus(null)).toThrow();
    expect(() => parsePermissionStatus([])).toThrow();
  });

  it("never reads an answer it does not understand as granted", () => {
    expect(mapMediaAccess("granted")).toBe("granted");
    expect(mapMediaAccess("denied")).toBe("denied");
    expect(mapMediaAccess("restricted")).toBe("restricted");
    expect(mapMediaAccess("not-determined")).toBe("not_determined");
    expect(mapMediaAccess("unknown")).toBe("not_determined");
    expect(mapMediaAccess("")).toBe("not_determined");
    expect(mapMediaAccess("GRANTED")).toBe("not_determined");
  });

  it("offers Settings for a permission the person can grant and an administrator for one they cannot", () => {
    expect(guideAction("granted")).toBe("none");
    expect(guideAction("denied")).toBe("open_settings");
    expect(guideAction("not_determined")).toBe("open_settings");
    expect(guideAction("restricted")).toBe("contact_administrator");
  });
});

// ------------------------------------------------------------------ configuration parity

describe("configuration the desktop mirrors", () => {
  const config = readFileSync(join(ROOT, "config", "computer-use.yaml"), "utf8");

  it("opens the Privacy panes configuration names, and polls at the configured rate", () => {
    for (const name of PERMISSION_NAMES) {
      const found = new RegExp(`^\\s+${name}: (Privacy_\\w+)\\s*$`, "m").exec(config);
      expect(found?.[1], name).toBe(SETTINGS_PANES[name]);
    }
    expect(/poll_interval_ms: (\d+)/.exec(config)?.[1]).toBe(String(PERMISSION_POLL_MS));
    // The page waits at least as long as the bridge's own pause window (1.5 s) before it enables Continue.
    expect(PAUSE_IDLE_SECONDS).toBeGreaterThanOrEqual(1.5);
  });

  it("builds a Settings URL from a fixed table, one per permission, and nothing else", () => {
    for (const name of PERMISSION_NAMES) {
      expect(settingsUrl(name)).toBe(`x-apple.systempreferences:com.apple.preference.security?${SETTINGS_PANES[name]}`);
    }
    expect(new Set(PERMISSION_NAMES.map(settingsUrl)).size).toBe(4);
  });
});

// ------------------------------------------------------------------ the snapshot and the guide

describe("snapshot", () => {
  it("is ready only when every permission the run needs is granted", () => {
    const denied = buildSnapshot({ status: { ...GRANTED, accessibility: "denied" }, needs: ["accessibility"], pauseInput: false, idleSeconds: null });
    expect(denied.ready).toBe(false);
    expect(denied.rows.find((row) => row.permission === "accessibility")).toMatchObject({ needed: true, action: "open_settings" });
    // A permission the run does not need does not hold it back.
    const unrelated = buildSnapshot({ status: { ...GRANTED, microphone: "denied" }, needs: ["accessibility"], pauseInput: false, idleSeconds: null });
    expect(unrelated.ready).toBe(true);
    // Once granted, it is ready.
    expect(buildSnapshot({ status: GRANTED, needs: ["accessibility", "screen_recording"], pauseInput: false, idleSeconds: null }).ready).toBe(true);
    expect(buildSnapshot({ status: { ...GRANTED, screen_recording: "not_determined" }, needs: ["accessibility", "screen_recording"], pauseInput: false, idleSeconds: null }).ready).toBe(false);
  });

  it("reports a permission it cannot probe as not checked and never sends the person to Settings for it", () => {
    expect(UNPROBED_PERMISSIONS).toEqual(["input_monitoring"]);
    const snapshot = buildSnapshot({
      status: { ...GRANTED, input_monitoring: "not_determined" },
      unprobed: UNPROBED_PERMISSIONS,
      needs: ["accessibility"],
      pauseInput: false,
      idleSeconds: null,
    });
    expect(snapshot.rows.find((row) => row.permission === "input_monitoring")).toMatchObject({ probed: false, action: "none" });
    expect(snapshot.ready).toBe(true);
  });

  it("asks the person to pause input and is ready only once they have been away long enough", () => {
    const base = { status: GRANTED, needs: [] as const, pauseInput: true };
    expect(buildSnapshot({ ...base, idleSeconds: 0.2 }).ready).toBe(false);
    expect(buildSnapshot({ ...base, idleSeconds: PAUSE_IDLE_SECONDS - 0.1 }).ready).toBe(false);
    expect(buildSnapshot({ ...base, idleSeconds: PAUSE_IDLE_SECONDS }).ready).toBe(true);
    // An unreadable idle time is not "paused".
    expect(buildSnapshot({ ...base, idleSeconds: null }).ready).toBe(false);
  });

  it("round trips through the page's strict decoder, and a malformed one is refused", () => {
    const snapshot = buildSnapshot({ status: { ...GRANTED, accessibility: "restricted" }, needs: ["accessibility"], pauseInput: false, idleSeconds: 5 });
    expect(decodePermissionSnapshot(JSON.parse(JSON.stringify(snapshot)))).toEqual(snapshot);
    const wire = JSON.parse(JSON.stringify(snapshot)) as Record<string, unknown>;
    expect(() => decodePermissionSnapshot({ ...wire, ready: "yes" })).toThrow(/ready/);
    expect(() => decodePermissionSnapshot({ ...wire, rows: [{ permission: "camera", state: "granted", needed: true, probed: true, action: "none" }] })).toThrow();
    expect(() => decodePermissionSnapshot({ ...wire, pollIntervalMs: 5 })).toThrow(/pollIntervalMs/);
    expect(() => decodePermissionSnapshot(null)).toThrow();
  });
});

describe("guide", () => {
  const snap = (status: PermissionStatus, needs: readonly ("accessibility" | "screen_recording")[], pauseInput = false, idleSeconds: number | null = null) =>
    buildSnapshot({ status, needs, pauseInput, idleSeconds });

  it("sends the person to the pane of the permission that is in the way, and does not let them continue", () => {
    const view = guideView(snap({ ...GRANTED, accessibility: "denied" }, ["accessibility"]), null);
    expect(view).toMatchObject({ kind: "open_settings", canContinue: false, first: "accessibility" });
    expect(view.rows[0]?.permission).toBe("accessibility");
  });

  it("enables Continue only when the snapshot says everything the run needs is granted", () => {
    const attention = parseComputerAttention(fixture("attention_open_settings.json"));
    expect(guideView(snap({ ...GRANTED, accessibility: "denied" }, ["accessibility"]), attention).canContinue).toBe(false);
    const granted = guideView(snap(GRANTED, ["accessibility"]), attention);
    expect(granted).toMatchObject({ kind: "ready", canContinue: true });
  });

  it("asks an administrator when policy restricts the permission", () => {
    const attention = parseComputerAttention(fixture("attention_restricted.json"));
    const view = guideView(snap({ ...GRANTED, accessibility: "restricted" }, ["accessibility"]), attention);
    expect(view).toMatchObject({ kind: "contact_administrator", canContinue: false, first: null });
  });

  it("a lapsed foreground grant asks for paused input and offers no Settings and no permission rows", () => {
    const attention = parseComputerAttention(fixture("attention_pause_input.json"));
    expect(requestFor(attention)).toEqual({ needs: [], pauseInput: true });
    const busy = guideView(snap(GRANTED, [], true, 0.3), attention);
    expect(busy).toMatchObject({ kind: "pause_input", canContinue: false, first: null });
    expect(busy.rows).toEqual([]);
    const paused = guideView(snap(GRANTED, [], true, PAUSE_IDLE_SECONDS + 1), attention);
    expect(paused).toMatchObject({ kind: "pause_input", canContinue: true });
  });

  it("stays out of the way where there are no such permissions", () => {
    const view = guideView(decodePermissionSnapshot({ applicable: false, rows: [], idleSeconds: null, pauseInput: false, ready: true, pollIntervalMs: 2000 }), null);
    expect(view.kind).toBe("not_applicable");
  });

  it("watches Accessibility until the runtime says what is missing", () => {
    expect(requestFor(null)).toEqual({ needs: ["accessibility"], pauseInput: false });
    expect(requestFor(parseComputerAttention(fixture("attention_open_settings.json")))).toEqual({ needs: ["accessibility"], pauseInput: false });
  });
});

// ------------------------------------------------------------------ what the runtime parked the run on

describe("the attention item", () => {
  it("parses exactly what the runtime writes", () => {
    expect(parseComputerAttention(fixture("attention_open_settings.json"))).toEqual({
      code: "COMPUTER_PERMISSION_REQUIRED",
      permission: "accessibility",
      status: "denied",
      guide: { action: "open_settings", pane: "Privacy_Accessibility" },
    });
    expect(parseComputerAttention(fixture("attention_pause_input.json"))).toEqual({
      code: "COMPUTER_FOREGROUND_CONTROL_LAPSED",
      permission: null,
      status: null,
      guide: { action: "pause_input", pane: null },
    });
    expect(parseComputerAttention(fixture("attention_restricted.json")).guide.action).toBe("contact_administrator");
  });

  it("refuses an item that asks for anything but the person's own resume", () => {
    const good = fixture("attention_open_settings.json") as Record<string, unknown>;
    expect(() => parseComputerAttention({ ...good, resume_command: "ApproveEffect" })).toThrow(/ResumeRun/);
    expect(() => parseComputerAttention({ ...good, notification_kind: "mention" })).toThrow(/blocked/);
    expect(() => parseComputerAttention({ ...good, autoOpen: true })).toThrow(/unknown field/);
    expect(() => parseComputerAttention({ ...good, guide: { action: "open_settings" } })).toThrow(/Privacy pane/);
    expect(() => parseComputerAttention({ ...good, guide: { action: "open_settings", pane: "https://example.com" } })).toThrow(/Privacy pane/);
    expect(() => parseComputerAttention({ ...good, guide: { action: "run_script", pane: "x" } })).toThrow();
    const pause = fixture("attention_pause_input.json") as Record<string, unknown>;
    expect(() => parseComputerAttention({ ...pause, guide: { action: "pause_input", pane: "Privacy_Accessibility" } })).toThrow(/no pane/);
    expect(() => parseComputerAttention({ ...pause, permission: "accessibility", status: "denied" })).toThrow(/names no permission/);
  });

  it("is taken from a parking event of this run and from nothing else", () => {
    const attention = fixture("attention_open_settings.json");
    const run = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
    const event = (type: string, runId: string, payload: unknown = attention): Record<string, unknown> => ({ type, payload: { run_id: runId, attention: payload } });
    expect(attentionFromEvent(event("run.suspended", run), run)?.code).toBe("COMPUTER_PERMISSION_REQUIRED");
    expect(attentionFromEvent(event("run.waiting", run), run)?.guide.action).toBe("open_settings");
    // Another run's event, another kind of event, no run, and a malformed item are all ignored.
    expect(attentionFromEvent(event("run.suspended", "run_01J8Z3K6F1N8VQ2X5W9Y0BBBBB"), run)).toBeNull();
    expect(attentionFromEvent(event("message.posted", run), run)).toBeNull();
    expect(attentionFromEvent(event("run.suspended", run), null)).toBeNull();
    expect(attentionFromEvent(event("run.suspended", run, { code: "COMPUTER_PERMISSION_REQUIRED" }), run)).toBeNull();
  });

  it("is resumed only by a ResumeRun naming a run, which the person sends", () => {
    expect(resumeCommand({ id: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", generation: 3 })).toEqual({
      name: "ResumeRun",
      params: { run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", generation: 3 },
    });
    expect(() => resumeCommand({ id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", generation: 1 })).toThrow(/run_/);
  });
});

// ------------------------------------------------------------------ the poll

describe("polling", () => {
  /** Timers the test fires by hand: nothing runs until `fire` is called. */
  function fakeTimers() {
    const pending: { id: number; callback: () => void; ms: number }[] = [];
    let next = 1;
    return {
      timers: {
        set: (callback: () => void, ms: number) => {
          const id = next++;
          pending.push({ id, callback, ms });
          return id;
        },
        clear: (handle: unknown) => {
          const at = pending.findIndex((entry) => entry.id === handle);
          if (at >= 0) {
            pending.splice(at, 1);
          }
        },
      },
      pending,
      fire: () => pending.shift()?.callback(),
    };
  }
  const wire = (ready: boolean): unknown => JSON.parse(JSON.stringify(buildSnapshot({ status: ready ? GRANTED : { ...GRANTED, accessibility: "denied" }, needs: ["accessibility"], pauseInput: false, idleSeconds: null })));
  const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

  it("re-arms a timeout after each read finishes, so reads never overlap, and stops when told", async () => {
    const fake = fakeTimers();
    const seen: boolean[] = [];
    let reads = 0;
    const slow: { release: (() => void) | null } = { release: null };
    const poller = new PermissionPoller({
      read: () => {
        reads += 1;
        if (reads === 2) {
          // A slow read: nothing is scheduled until it finishes.
          return new Promise((resolve) => {
            slow.release = () => {
              resolve(wire(true));
            };
          });
        }
        return Promise.resolve(wire(false));
      },
      onSnapshot: (snapshot) => seen.push(snapshot.ready),
      timers: fake.timers,
    });
    poller.start();
    poller.start();
    await settle();
    expect(reads).toBe(1);
    expect(seen).toEqual([false]);
    expect(fake.pending).toHaveLength(1);
    expect(fake.pending[0]?.ms).toBe(PERMISSION_POLL_MS);

    fake.fire();
    await settle();
    expect(reads).toBe(2);
    expect(fake.pending, "no timer while a read is in flight").toHaveLength(0);
    slow.release?.();
    await settle();
    expect(seen).toEqual([false, true]);
    expect(fake.pending).toHaveLength(1);

    poller.stop();
    expect(fake.pending).toHaveLength(0);
    expect(poller.running).toBe(false);
  });

  it("reports a failed read and tries again", async () => {
    const fake = fakeTimers();
    const errors: unknown[] = [];
    const seen: boolean[] = [];
    let reads = 0;
    const poller = new PermissionPoller({
      read: () => {
        reads += 1;
        return reads === 1 ? Promise.reject(new Error("main is busy")) : Promise.resolve(wire(true));
      },
      onSnapshot: (snapshot) => seen.push(snapshot.ready),
      onError: (error) => errors.push(error),
      timers: fake.timers,
    });
    poller.start();
    await settle();
    expect(errors).toHaveLength(1);
    expect(fake.pending, "the poll continues after an error").toHaveLength(1);
    fake.fire();
    await settle();
    expect(seen).toEqual([true]);
    poller.stop();
  });

  it("does not report a snapshot that arrives after it was stopped", async () => {
    const fake = fakeTimers();
    const seen: boolean[] = [];
    const slow: { release: (() => void) | null } = { release: null };
    const poller = new PermissionPoller({
      read: () =>
        new Promise((resolve) => {
          slow.release = () => {
            resolve(wire(true));
          };
        }),
      onSnapshot: (snapshot) => seen.push(snapshot.ready),
      timers: fake.timers,
    });
    poller.start();
    poller.stop();
    slow.release?.();
    await settle();
    expect(seen).toEqual([]);
    expect(fake.pending).toHaveLength(0);
  });
});

// ------------------------------------------------------------------ the bridge's two channels

describe("computer channels", () => {
  function bridge(overrides: { source?: PermissionSource; open?: (url: string) => Promise<void> } = {}): DesktopBridge {
    const dir = mkdtempSync(join(tmpdir(), "quansio-permissions-"));
    temporaryDirs.push(dir);
    return new DesktopBridge({
      serverOrigin: "http://127.0.0.1:8080",
      auth: { kind: "trusted-headers", scope: { tenantId: "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", userId: "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }, workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" },
      session: new SessionFile(dir),
      openSocket: () => {
        throw new Error("no socket in this test");
      },
      updates: new UpdateHooks("stable", null, "http://x", false),
      consumer: "desktop:test",
      push: () => undefined,
      ...(overrides.source ? { computerPermissions: overrides.source } : {}),
      ...(overrides.open ? { openSystemSettings: overrides.open } : {}),
    });
  }

  it("are allowlisted and handled", () => {
    expect(PRELOAD_ALLOWLIST).toContain("computer.permissions");
    expect(PRELOAD_ALLOWLIST).toContain("computer.openSettings");
    const handlers = bridge().handlers();
    expect(typeof handlers["computer.permissions"]).toBe("function");
    expect(typeof handlers["computer.openSettings"]).toBe("function");
  });

  it("answers the poll from what the system says and the permissions the page asked about", async () => {
    const status: PermissionStatus = { ...GRANTED, accessibility: "denied", input_monitoring: "not_determined" };
    const source: PermissionSource = { read: () => Promise.resolve({ status, unprobed: UNPROBED_PERMISSIONS }), idleSeconds: () => 9 };
    const handlers = bridge({ source }).handlers();
    const snapshot = decodePermissionSnapshot(await handlers["computer.permissions"]({ needs: ["accessibility"] }));
    expect(snapshot).toMatchObject({ applicable: true, ready: false, idleSeconds: 9 });
    expect(snapshot.rows.find((row) => row.permission === "accessibility")).toMatchObject({ state: "denied", needed: true, action: "open_settings" });
    expect(snapshot.rows.find((row) => row.permission === "input_monitoring")).toMatchObject({ probed: false, action: "none" });
    // The default request watches Accessibility.
    expect(decodePermissionSnapshot(await handlers["computer.permissions"](undefined)).ready).toBe(false);
    // The page's request is untrusted data.
    await expect(handlers["computer.permissions"]({ needs: ["camera"] })).rejects.toThrow(/permission names/);
    await expect(handlers["computer.permissions"]({ needs: [], pauseInput: "yes" })).rejects.toThrow(/boolean/);
    await expect(handlers["computer.permissions"]({ url: "x" })).rejects.toThrow(/unknown field/);
    await expect(handlers["computer.permissions"]("accessibility")).rejects.toThrow(/object/);
  });

  it("reports that it does not apply on a host without a permission source", async () => {
    const snapshot = decodePermissionSnapshot(await bridge().handlers()["computer.permissions"]({}));
    expect(snapshot.applicable).toBe(false);
    expect(guideView(snapshot, null).kind).toBe("not_applicable");
  });

  it("opens exactly the pane of the permission the person clicked, built by main from a fixed table", async () => {
    const opened: string[] = [];
    const handlers = bridge({ open: (url) => { opened.push(url); return Promise.resolve(); } }).handlers();
    for (const name of PERMISSION_NAMES) {
      expect(await handlers["computer.openSettings"]({ permission: name })).toEqual({ opened: true });
    }
    expect(opened).toEqual(PERMISSION_NAMES.map(settingsUrl));
  });

  it("cannot be pointed at any other destination, however the payload is shaped", async () => {
    const opened: string[] = [];
    const handlers = bridge({ open: (url) => { opened.push(url); return Promise.resolve(); } }).handlers();
    const hostile: unknown[] = [
      { permission: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles" },
      { permission: "https://example.com" },
      { permission: "file:///Applications/Calculator.app" },
      { permission: "accessibility", url: "https://example.com" },
      { permission: "accessibility", pane: "Privacy_AllFiles" },
      { url: "x-apple.systempreferences:" },
      { permission: ["accessibility"] },
      "accessibility",
      null,
      [],
    ];
    for (const payload of hostile) {
      await expect(handlers["computer.openSettings"](payload), JSON.stringify(payload)).rejects.toThrow();
    }
    expect(opened).toEqual([]);
    // Without a host that can open it, nothing is opened and the page is told.
    expect(await bridge().handlers()["computer.openSettings"]({ permission: "accessibility" })).toEqual({ opened: false });
    expect(parseOpenSettings({ permission: "microphone" })).toBe("microphone");
    expect(parsePermissionRequest({ needs: ["screen_recording", "screen_recording"], pauseInput: true })).toEqual({ needs: ["screen_recording"], pauseInput: true });
  });
});

// ------------------------------------------------------------------ the agent never drives System Settings

describe("System Settings", () => {
  function sources(directory: string): string[] {
    const found: string[] = [];
    for (const entry of readdirSync(directory)) {
      const path = join(directory, entry);
      if (statSync(path).isDirectory()) {
        found.push(...sources(path));
      } else if (/\.(ts|tsx)$/.test(entry)) {
        found.push(path);
      }
    }
    return found;
  }
  const all = sources(join(ROOT, "apps", "desktop", "src"));

  it("has one place that names a Settings URL and one handler that opens it", () => {
    const naming = all.filter((path) => readFileSync(path, "utf8").includes("x-apple.systempreferences")).map((path) => relative(ROOT, path));
    expect(naming).toEqual(["apps/desktop/src/main/permissions.ts"]);
    // The opener is supplied to the bridge and called in one place: the handler that takes a permission name.
    const calls = all.filter((path) => /openSystemSettings/.test(readFileSync(path, "utf8"))).map((path) => relative(ROOT, path)).sort();
    expect(calls).toEqual(["apps/desktop/src/main/app.ts", "apps/desktop/src/main/ipc.ts"]);
    const ipc = readFileSync(join(ROOT, "apps", "desktop", "src", "main", "ipc.ts"), "utf8");
    expect(ipc.match(/this\.#options\.openSystemSettings/g)).toHaveLength(1);
  });

  it("is reachable from the renderer only by a click handler that names a permission", () => {
    const renderer = all.filter((path) => path.includes("/renderer/"));
    const callers = renderer.filter((path) => readFileSync(path, "utf8").includes("openComputerSettings")).map((path) => relative(ROOT, path));
    expect(callers).toEqual(["apps/desktop/src/renderer/ComputerPermissions.tsx"]);
    const source = readFileSync(join(ROOT, "apps", "desktop", "src", "renderer", "ComputerPermissions.tsx"), "utf8");
    // It is called from the open handler, which only the row's button's onClick reaches, and with a permission name.
    expect(source).toMatch(/openComputerSettings\(permission\)/);
    expect(source).toMatch(/onClick=\{\(\) => \{ onOpen\(row\.permission\); \}\}/);
    // The run is resumed only from the Continue button, never from the poll.
    expect(source).toMatch(/onClick=\{\(\) => \{ void resume\(\); \}\}/);
    expect(source.match(/resumeCommand\(/g)).toHaveLength(1);
  });
});
