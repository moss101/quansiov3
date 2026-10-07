/**
 * The shell's gates (ADP-004, DOMAIN.md 13.6 E, F, G): keep-awake only on AC power while a Turn runs, the offline grace
 * feeding the one existing reconnect, quitting and updating never interrupting work without the person's say, and the
 * words the two questions use.
 */

import { describe, expect, it } from "vitest";

import { Blocker, Clock, FakeUpdater } from "./shell-fakes.js";

import { ConnectivityMonitor, type ConnectivityState } from "../src/main/connectivity.js";
import { quitChosen, quitDialog, updateChoiceOf, updateDialog } from "../src/main/dialogs.js";
import { KeepAwake, shouldHold } from "../src/main/keep-awake.js";
import { QuitGate } from "../src/main/quit.js";
import { UpdateGate, type UpdateChoice } from "../src/main/update-gate.js";
import { UpdateHooks } from "../src/main/updates.js";
import { decodeMaintenance, describeWork, workStatus, type MaintenanceStatus, type WorkStatus } from "../src/main/work-status.js";

/** Let promise continuations run (the gates are async). */
async function settle(): Promise<void> {
  for (let i = 0; i < 6; i += 1) {
    await Promise.resolve();
  }
}

describe("keep-awake", () => {
  it("holds exactly when a Turn is active, the machine is on AC power and the person has not turned it off", () => {
    for (const running of [false, true]) {
      for (const onAcPower of [false, true]) {
        for (const enabled of [false, true]) {
          expect(shouldHold({ running, onAcPower, enabled }), `${String(running)}/${String(onAcPower)}/${String(enabled)}`).toBe(running && onAcPower && enabled);
        }
      }
    }
  });

  it("takes one blocker while work runs on AC power and drops it when the power source or the work changes", () => {
    const blocker = new Blocker();
    const awake = new KeepAwake(blocker);
    awake.update({ onAcPower: true, enabled: true });
    expect(awake.holding).toBe(false);
    awake.update({ running: true });
    awake.update({ running: true });
    expect(awake.holding).toBe(true);
    expect(blocker.started).toBe(1);
    // Unplugged: dropped at once, even though work continues.
    awake.update({ onAcPower: false });
    expect(awake.holding).toBe(false);
    expect(blocker.stopped).toEqual([1]);
    // Plugged back in: held again.
    awake.update({ onAcPower: true });
    expect(awake.holding).toBe(true);
    expect(blocker.started).toBe(2);
    // The person turns it off, then on; the work finishes.
    awake.update({ enabled: false });
    expect(awake.holding).toBe(false);
    awake.update({ enabled: true });
    expect(awake.holding).toBe(true);
    awake.update({ running: false });
    expect(awake.holding).toBe(false);
    expect(blocker.stopped.length).toBe(3);
  });

  it("never holds on battery, and releases on dispose", () => {
    const blocker = new Blocker();
    const awake = new KeepAwake(blocker);
    awake.update({ onAcPower: false, running: true });
    expect(blocker.started).toBe(0);
    awake.update({ onAcPower: true });
    awake.dispose();
    expect(awake.holding).toBe(false);
    expect(blocker.stopped).toEqual([1]);
  });
});

describe("connectivity grace", () => {
  function monitor(streamOpen = true): { monitor: ConnectivityMonitor; clock: Clock; reconnects: boolean[]; states: ConnectivityState[]; open: { value: boolean } } {
    const clock = new Clock();
    const reconnects: boolean[] = [];
    const states: ConnectivityState[] = [];
    const open = { value: streamOpen };
    return {
      monitor: new ConnectivityMonitor({
        graceMs: 10_000,
        minGapMs: 2_000,
        reconnect: (force) => reconnects.push(force),
        streamOpen: () => open.value,
        now: () => clock.now,
        setTimer: clock.setTimer,
        clearTimer: clock.clearTimer,
        onState: (state) => states.push(state),
      }),
      clock,
      reconnects,
      states,
      open,
    };
  }

  it("absorbs a flap shorter than the grace: nothing reconnects and the state never reaches offline", () => {
    const { monitor: m, clock, reconnects, states } = monitor();
    m.report(false);
    expect(m.state).toBe("offline_pending");
    clock.advance(9_000);
    m.report(true);
    clock.advance(60_000);
    expect(m.state).toBe("online");
    expect(reconnects).toEqual([]);
    expect(states).toEqual(["offline_pending", "online"]);
    expect(clock.pending).toBe(0);
  });

  it("goes offline after the grace and reconnects, forced, when the network returns", () => {
    const { monitor: m, clock, reconnects, states } = monitor();
    m.report(false);
    clock.advance(9_999);
    expect(m.state).toBe("offline_pending");
    clock.advance(1);
    expect(m.state).toBe("offline");
    // Repeated offline reports do not restart the grace.
    m.report(false);
    expect(m.state).toBe("offline");
    m.report(true);
    expect(m.state).toBe("online");
    expect(reconnects).toEqual([true]);
    expect(states).toEqual(["offline_pending", "offline", "online"]);
  });

  it("calls the one reconnect once however many signals arrive together", () => {
    const { monitor: m, clock, reconnects, open } = monitor(false);
    m.resumed();
    m.focused();
    m.report(true);
    expect(reconnects).toEqual([true]);
    clock.advance(1_999);
    m.resumed();
    expect(reconnects).toEqual([true]);
    clock.advance(1);
    m.resumed();
    expect(reconnects).toEqual([true, true]);
    open.value = true;
    clock.advance(10_000);
    m.focused();
    expect(reconnects).toEqual([true, true]);
  });

  it("reconnects on focus only when the stream is not open, and not while known to be offline", () => {
    const { monitor: m, clock, reconnects, open } = monitor(true);
    m.focused();
    expect(reconnects).toEqual([]);
    open.value = false;
    m.focused();
    expect(reconnects).toEqual([false]);
    clock.advance(5_000);
    m.report(false);
    clock.advance(10_000);
    m.focused();
    expect(reconnects).toEqual([false]);
  });

  it("a resume from sleep reconnects at once, forced", () => {
    const { monitor: m, reconnects } = monitor(true);
    m.resumed();
    expect(reconnects).toEqual([true]);
  });

  it("drops its timer on dispose", () => {
    const { monitor: m, clock } = monitor();
    m.report(false);
    expect(clock.pending).toBe(1);
    m.dispose();
    expect(clock.pending).toBe(0);
  });
});

const BUSY: WorkStatus = { kind: "busy", activeTurns: 2, inflightActions: 1 };

describe("work status", () => {
  const server = (overrides: Partial<{ idle: boolean; active_turns: number; inflight_actions: number; open_terminals: number }> = {}): unknown => ({
    idle: true,
    active_turns: 0,
    inflight_actions: 0,
    open_terminals: 0,
    workspace_id: null,
    as_of: "2026-10-07T00:00:00Z",
    ...overrides,
  });

  it("decodes the projection and refuses one that contradicts itself", () => {
    expect(decodeMaintenance(server())).toMatchObject({ idle: true, activeTurns: 0 });
    expect(decodeMaintenance(server({ idle: false, active_turns: 1 }))).toMatchObject({ idle: false, activeTurns: 1 });
    // idle: true with work counted, or a malformed body, is not trusted as idle.
    expect(decodeMaintenance(server({ idle: true, active_turns: 1 }))).toBeNull();
    expect(decodeMaintenance(server({ idle: false }))).toBeNull();
    expect(decodeMaintenance(server({ active_turns: -1, idle: false }))).toBeNull();
    expect(decodeMaintenance({ idle: true })).toBeNull();
    expect(decodeMaintenance(null)).toBeNull();
    expect(decodeMaintenance("idle")).toBeNull();
  });

  it("is busy if either source says so, idle only when the server says idle, and unknown when it cannot be read", () => {
    const idle = decodeMaintenance(server()) as MaintenanceStatus;
    const busy = decodeMaintenance(server({ idle: false, active_turns: 1, inflight_actions: 2 })) as MaintenanceStatus;
    expect(workStatus(idle, 0)).toEqual({ kind: "idle" });
    expect(workStatus(idle, 1)).toEqual({ kind: "busy", activeTurns: 1, inflightActions: 0 });
    expect(workStatus(busy, 0)).toEqual({ kind: "busy", activeTurns: 1, inflightActions: 2 });
    expect(workStatus(busy, 3)).toEqual({ kind: "busy", activeTurns: 3, inflightActions: 2 });
    expect(workStatus(null, 0)).toEqual({ kind: "unknown" });
    expect(workStatus(null, 2)).toEqual({ kind: "busy", activeTurns: 2, inflightActions: 0 });
  });

  it("describes what is running in words", () => {
    expect(describeWork({ kind: "busy", activeTurns: 1, inflightActions: 0 })).toBe("1 turn is running.");
    expect(describeWork(BUSY)).toBe("2 turns are running; 1 action is in progress.");
    expect(describeWork({ kind: "unknown" })).toMatch(/could not check/);
  });
});

describe("quit confirmation", () => {
  function gate(status: () => Promise<WorkStatus>, answer: boolean | (() => Promise<boolean>)): { gate: QuitGate; asked: WorkStatus[] } {
    const asked: WorkStatus[] = [];
    return {
      asked,
      gate: new QuitGate({
        probe: status,
        ask: (busy) => {
          asked.push(busy);
          return typeof answer === "boolean" ? Promise.resolve(answer) : answer();
        },
      }),
    };
  }

  it("quits without asking when nothing is running", async () => {
    const { gate: g, asked } = gate(() => Promise.resolve({ kind: "idle" }), false);
    expect(await g.confirm()).toBe(true);
    expect(asked).toEqual([]);
    expect(g.permitted).toBe(true);
  });

  it("asks while work is running, and quits only if the person says so", async () => {
    const stay = gate(() => Promise.resolve(BUSY), false);
    expect(await stay.gate.confirm()).toBe(false);
    expect(stay.gate.permitted).toBe(false);
    expect(stay.asked).toEqual([BUSY]);
    const go = gate(() => Promise.resolve(BUSY), true);
    expect(await go.gate.confirm()).toBe(true);
    expect(go.gate.permitted).toBe(true);
  });

  it("asks when the status cannot be read: a failed read is never idle", async () => {
    const unknown = gate(() => Promise.resolve({ kind: "unknown" }), false);
    expect(await unknown.gate.confirm()).toBe(false);
    expect(unknown.asked).toEqual([{ kind: "unknown" }]);
    const failing = gate(() => Promise.reject(new Error("offline")), false);
    expect(await failing.gate.confirm()).toBe(false);
    expect(failing.asked).toEqual([{ kind: "unknown" }]);
  });

  it("shares one question between quit requests that overlap", async () => {
    let release: (value: boolean) => void = () => undefined;
    const pending = new Promise<boolean>((resolve) => {
      release = resolve;
    });
    const { gate: g, asked } = gate(() => Promise.resolve(BUSY), () => pending);
    const first = g.confirm();
    const second = g.confirm();
    await settle();
    release(true);
    expect(await Promise.all([first, second])).toEqual([true, true]);
    expect(asked.length).toBe(1);
  });

  it("asks again after the person said no", async () => {
    const { gate: g, asked } = gate(() => Promise.resolve(BUSY), false);
    await g.confirm();
    await g.confirm();
    expect(asked.length).toBe(2);
  });

  it("an update that already asked can allow its own quit, and revoke it", async () => {
    const { gate: g, asked } = gate(() => Promise.resolve(BUSY), false);
    g.allow();
    expect(await g.confirm()).toBe(true);
    expect(asked).toEqual([]);
    g.revoke();
    expect(await g.confirm()).toBe(false);
    expect(asked.length).toBe(1);
  });
});

describe("the words of the two questions", () => {
  it("say the work is paused, not lost, and make the safe choice the default and the way out", () => {
    const quit = quitDialog(BUSY);
    expect(quit.detail).toMatch(/paused/);
    expect(quit.detail).toMatch(/continues from the server/);
    expect(quit.detail).not.toMatch(/lost|stopped|cancel/i);
    expect(quit.message).toBe("2 turns are running; 1 action is in progress.");
    expect(quit.buttons[quit.defaultId]).toBe("Keep working");
    expect(quit.buttons[quit.cancelId]).toBe("Keep working");
    expect(quitChosen(1)).toBe(true);
    expect(quitChosen(0)).toBe(false);
    expect(quitChosen(-1)).toBe(false);
  });

  it("say so when the status could not be read", () => {
    expect(quitDialog({ kind: "unknown" }).detail).toMatch(/could not check/);
  });

  it("offer to restart anyway, to install when idle, or to leave it, defaulting to leaving it", () => {
    const update = updateDialog(BUSY);
    expect(update.buttons).toEqual(["Later", "Install when idle", "Restart anyway"]);
    expect(update.buttons[update.defaultId]).toBe("Later");
    expect(update.buttons[update.cancelId]).toBe("Later");
    expect(update.detail).toMatch(/pauses running work/);
    expect(updateChoiceOf(2)).toBe("install");
    expect(updateChoiceOf(1)).toBe("when_idle");
    expect(updateChoiceOf(0)).toBe("later");
    expect(updateChoiceOf(-1)).toBe("later");
  });
});

describe("update deferral", () => {
  function setup(statuses: WorkStatus[] | (() => WorkStatus), choice: UpdateChoice = "later") {
    const updater = new FakeUpdater();
    const hooks = new UpdateHooks("stable", updater, "http://feed", true);
    updater.emit("update-downloaded", {}, "", "2.0.0");
    const clock = new Clock();
    const events: string[] = [];
    const asked: WorkStatus[] = [];
    let read = 0;
    const probe = (): Promise<WorkStatus> => {
      read += 1;
      return Promise.resolve(typeof statuses === "function" ? statuses() : (statuses[Math.min(read - 1, statuses.length - 1)] ?? { kind: "unknown" }));
    };
    const gate = new UpdateGate({
      updates: hooks,
      probe,
      ask: (status) => {
        asked.push(status);
        return Promise.resolve(choice);
      },
      allowQuit: () => events.push("allow"),
      revokeQuit: () => events.push("revoke"),
      pollMs: 30_000,
      setTimer: clock.setTimer,
      clearTimer: clock.clearTimer,
    });
    return { updater, hooks, gate, clock, events, asked };
  }

  it("installs without asking when nothing is running", async () => {
    const { gate, updater, asked, events } = setup([{ kind: "idle" }]);
    expect(await gate.requestInstall()).toEqual({ kind: "installing" });
    expect(updater.installs).toBe(1);
    expect(asked).toEqual([]);
    expect(events).toEqual(["allow"]);
  });

  it("does not install over running work without the person's choice: deferred by default", async () => {
    const { gate, updater, asked } = setup([BUSY], "later");
    expect(await gate.requestInstall()).toEqual({ kind: "deferred" });
    expect(updater.installs).toBe(0);
    expect(asked).toEqual([BUSY]);
    expect(gate.waitingForIdle).toBe(false);
  });

  it("installs over running work only when the person chooses to restart anyway", async () => {
    const { gate, updater } = setup([BUSY], "install");
    expect(await gate.requestInstall()).toEqual({ kind: "installing" });
    expect(updater.installs).toBe(1);
  });

  it("asks, and does not install, when the status cannot be read", async () => {
    const { gate, updater, asked } = setup([{ kind: "unknown" }], "later");
    expect(await gate.requestInstall()).toEqual({ kind: "deferred" });
    expect(asked).toEqual([{ kind: "unknown" }]);
    expect(updater.installs).toBe(0);
  });

  it("installs when idle: polls without an interval, never installs while busy or unreadable, and stops after installing", async () => {
    const { gate, updater, clock } = setup([BUSY, { kind: "unknown" }, BUSY, { kind: "idle" }], "when_idle");
    expect(await gate.requestInstall()).toEqual({ kind: "waiting_for_idle" });
    expect(gate.waitingForIdle).toBe(true);
    // The first reading was the question's (busy); each poll is one more reading.
    for (let poll = 0; poll < 3; poll += 1) {
      expect(updater.installs).toBe(0);
      clock.advance(30_000);
      await settle();
    }
    expect(updater.installs).toBe(1);
    expect(gate.waitingForIdle).toBe(false);
    expect(clock.pending).toBe(0);
  });

  it("stops waiting when the person cancels", async () => {
    const { gate, updater, clock } = setup(() => BUSY, "when_idle");
    await gate.requestInstall();
    gate.cancelWhenIdle();
    expect(gate.waitingForIdle).toBe(false);
    clock.advance(120_000);
    await settle();
    expect(updater.installs).toBe(0);
    expect(clock.pending).toBe(0);
  });

  it("refuses to install what was not downloaded", async () => {
    const updater = new FakeUpdater();
    const hooks = new UpdateHooks("stable", updater, "http://feed", true);
    const gate = new UpdateGate({ updates: hooks, probe: () => Promise.resolve({ kind: "idle" }), ask: () => Promise.resolve("install"), allowQuit: () => undefined, revokeQuit: () => undefined, pollMs: 30_000, setTimer: new Clock().setTimer, clearTimer: () => undefined });
    expect(await gate.requestInstall()).toEqual({ kind: "not_downloaded" });
    gate.installWhenIdle();
    expect(gate.waitingForIdle).toBe(false);
    expect(updater.installs).toBe(0);
  });

  it("a failed install turns install-on-quit off and takes back the permission to quit", async () => {
    const { gate, updater, hooks, events } = setup([{ kind: "idle" }]);
    const changes: boolean[] = [];
    hooks.onInstallOnQuit((enabled) => changes.push(enabled));
    hooks.setInstallOnQuit(true);
    updater.throwOnInstall = true;
    expect(await gate.requestInstall()).toEqual({ kind: "failed" });
    expect(events).toEqual(["allow", "revoke"]);
    expect(hooks.installOnQuit).toBe(false);
    expect(changes).toEqual([true, false]);
    expect(hooks.status()).toMatchObject({ state: "error" });
  });

  it("an installer that fails after it started also turns install-on-quit off", () => {
    const updater = new FakeUpdater();
    const hooks = new UpdateHooks("stable", updater, "http://feed", true, true);
    updater.emit("update-downloaded", {}, "", "2.0.0");
    expect(hooks.install()).toBe(true);
    expect(hooks.installOnQuit).toBe(true);
    updater.emit("error", new Error("signature mismatch"));
    expect(hooks.installOnQuit).toBe(false);
    expect(hooks.status()).toMatchObject({ state: "error", message: "signature mismatch" });
  });

  it("keeps the unsigned-build behaviour: unavailable, and nothing to install", () => {
    const hooks = new UpdateHooks("stable", new FakeUpdater(), "http://feed", false);
    expect(hooks.status().state).toBe("unavailable");
    expect(hooks.downloaded).toBe(false);
    expect(hooks.install()).toBe(false);
  });
});
