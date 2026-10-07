/**
 * The desktop shell's host (ADP-004): the one object that composes the shell's parts and answers its IPC channels.
 *
 * It owns no authority and imports no Electron. `app.ts` supplies the platform pieces (windows, global shortcuts,
 * the power-save blocker, dialogs, the menu and tray) as adapters; the host wires them to the existing bridge:
 * stream frames become activity items, activity items drive keep-awake and the indicator, connectivity signals
 * call the bridge's one reconnect, an update installs only through the gate, and the quick-entry window posts
 * through the same `PostMessage` command path the main window uses.
 *
 * Every shell channel validates its payload against a schema first (`schema.ts`); the window-type and own-thread
 * rules are enforced before a handler is called (`ipc-trust.ts`), so a handler here may assume its caller is a
 * window that is allowed the channel.
 */

import type { EventFrame } from "../client/stream.js";
import type { DesktopNotice } from "../automation/notifications.js";
import type { ShellChannel } from "../preload/bridge.js";
import { settled } from "./ipc.js";
import { ActivityTracker, type ActivityItem } from "./activity.js";
import { ConnectivityMonitor } from "./connectivity.js";
import { KeepAwake } from "./keep-awake.js";
import { buildMenu, type MenuItem, type ShellCommand } from "./menu.js";
import type { PreferencesFile } from "./preferences.js";
import * as s from "./schema.js";
import type { DesktopShellConfig } from "./shell-config.js";
import { ShortcutRegistry, type GlobalShortcutId, type SetResult } from "./shortcuts.js";
import type { QuitGate } from "./quit.js";
import type { UpdateGate } from "./update-gate.js";
import type { UpdateHooks, UpdateStatus } from "./updates.js";
import { indicatorOrigin, indicatorPosition, THREAD_ID, WindowManager, type DisplayAdapter, type ManagedWindow, type Point, type WindowEvent } from "./windows.js";
import { workStatus, type MaintenanceStatus, type WorkStatus } from "./work-status.js";
import type { WindowHandler } from "./ipc-trust.js";

/** The part of `DesktopBridge` the host uses. */
export interface ShellBridge {
  postMessage(raw: unknown): Promise<unknown>;
  signedIn(): boolean;
  streamOpen(): boolean;
  resumeStream(force: boolean): boolean;
  maintenance(): Promise<MaintenanceStatus | null>;
}

export interface ShellHostOptions {
  readonly platform: string;
  readonly config: DesktopShellConfig;
  readonly preferences: PreferencesFile;
  readonly windows: WindowManager;
  readonly bridge: ShellBridge;
  readonly shortcuts: ShortcutRegistry;
  readonly keepAwake: KeepAwake;
  readonly quit: QuitGate;
  readonly updateGate: UpdateGate;
  readonly updates: UpdateHooks;
  readonly connectivity: ConnectivityMonitor;
  readonly activity: ActivityTracker;
  readonly displays: DisplayAdapter;
  /** The pointer's position on screen (where quick entry is summoned and a pop-out is dropped). */
  readonly pointer: () => Point;
  /** Apply a menu (the Electron adapter builds the native menu from it). */
  readonly setMenu: (items: readonly MenuItem[]) => void;
  readonly checkForUpdates: () => void;
  readonly quitApp: () => void;
  readonly log: (line: string) => void;
}

const SHORTCUT_ID = s.string({ min: 1, max: 32, pattern: /^[a-z_]+$/ });
const AGENT_ID = /^[a-z]{1,4}_[0-9A-HJKMNP-TV-Z]{26}$/;

const SET_SHORTCUT = s.object<{ id: string; binding: string | null }>({ id: SHORTCUT_ID, binding: s.nullable(s.string({ min: 1, max: 64 })) });
const RESET_SHORTCUT = s.object<{ id: string }>({ id: SHORTCUT_ID });
const PREFERENCES_SET = s.object<{ keepAwake?: boolean; installWhenIdle?: boolean; installOnQuit?: boolean }>({
  keepAwake: s.optional(s.boolean()),
  installWhenIdle: s.optional(s.boolean()),
  installOnQuit: s.optional(s.boolean()),
});
const ONLINE = s.object<{ online: boolean }>({ online: s.boolean() });
const PINNED = s.object<{ pinned: boolean }>({ pinned: s.boolean() });
const INTERACTIVE = s.object<{ active: boolean }>({ active: s.boolean() });
const COORDINATE = s.integer({ min: -100000, max: 100000 });
const MOVE = s.object<{ x: number; y: number }>({ x: COORDINATE, y: COORDINATE });
const KEY = s.object<{ key: string }>({ key: s.string({ min: 1, max: 160 }) });
const INSTALL = s.object<{ mode: "now" | "when_idle" | "cancel_when_idle" }>({ mode: s.oneOf(["now", "when_idle", "cancel_when_idle"] as const) });
const QUICK_ENTRY = s.object<{ content: string; teammateId?: string | null }>({
  content: s.string({ min: 1, max: 16000 }),
  teammateId: s.optional(s.nullable(s.string({ min: 5, max: 40, pattern: AGENT_ID }))),
});
const POPOUT_OPEN = s.object<{ threadId: string; tab?: "conversation" | "inspector"; sidebar?: boolean; x?: number; y?: number }>({
  threadId: s.string({ min: 30, max: 30, pattern: THREAD_ID }),
  tab: s.optional(s.oneOf(["conversation", "inspector"] as const)),
  sidebar: s.optional(s.boolean()),
  x: s.optional(COORDINATE),
  y: s.optional(COORDINATE),
});
const NOTHING = s.empty();

type Handlers = Record<ShellChannel, WindowHandler>;

/** A typed refusal the shell's own channels answer with. */
function refused(code: string, message: string): { readonly ok: false; readonly error: { readonly code: string; readonly message: string; readonly retryable: false; readonly details: null } } {
  return { ok: false, error: { code, message, retryable: false, details: null } };
}

/**
 * A shortcut change as the shell's channels answer: `{ok: true, result: {snapshot}}`, or `{ok: false, error}` whose
 * code is the typed outcome (`invalid`, `conflict`, `registration_failed`, `unknown_shortcut`) and whose details
 * name what it conflicts with or why it is invalid. The previous binding is in force after every failure.
 */
export function shortcutResult(result: SetResult): unknown {
  if (result.ok) {
    return { ok: true, result: { snapshot: result.snapshot } };
  }
  const details = result.code === "conflict" ? { with: result.with } : result.code === "invalid" ? { problem: result.problem } : null;
  return { ok: false, error: { code: result.code, message: result.message, retryable: false, details } };
}

/** Where a click on a notification goes. */
export function commandForNotice(notice: DesktopNotice): ShellCommand | null {
  const ref = notice.ref;
  const text = (key: string): string | null => (typeof ref[key] === "string" ? ref[key] : null);
  const thread = text("thread_id");
  if (thread !== null && THREAD_ID.test(thread)) {
    return { kind: "open-thread", threadId: thread };
  }
  const approval = text("approval_id");
  if (approval !== null) {
    return { kind: "open-approval", approvalId: approval };
  }
  const run = text("run_id");
  return run === null ? null : { kind: "open-run", runId: run };
}

export class ShellHost {
  readonly #o: ShellHostOptions;
  #focused: ManagedWindow | null = null;

  constructor(options: ShellHostOptions) {
    this.#o = options;
    options.shortcuts.onChange(() => {
      this.refreshMenu();
      this.#o.windows.broadcast(["main"], "shell.shortcuts", this.#o.shortcuts.snapshot());
    });
    options.shortcuts.onRecordingEnded((reason) => {
      this.#o.windows.broadcast(["main"], "shell.recording.ended", { reason });
    });
    options.activity.onChange(() => {
      this.#activityChanged();
    });
    options.windows.onWindowEvent((window, event) => {
      this.#windowEvent(window, event);
    });
  }

  // ---- start-up and the events the platform reports ----

  /** Create the windows that exist for the whole session, build the menu and register the stored shortcuts. */
  start(): void {
    this.#o.shortcuts.registerStored();
    this.#o.keepAwake.update({ enabled: this.keepAwakeEnabled() });
    this.refreshMenu();
  }

  keepAwakeEnabled(): boolean {
    return this.#o.preferences.current().keepAwake ?? this.#o.config.keepAwakeDefault;
  }

  installWhenIdleEnabled(): boolean {
    return this.#o.preferences.current().installWhenIdle ?? this.#o.config.installWhenIdleDefault;
  }

  /** The machine's power source changed. */
  onPowerSource(onAcPower: boolean): void {
    this.#o.keepAwake.update({ onAcPower });
  }

  /** The machine woke from sleep. */
  onResume(): void {
    this.#o.connectivity.resumed();
  }

  /** The set of displays or their geometry changed: the indicator is put back inside a display. */
  onDisplaysChanged(): void {
    this.#placeIndicator();
  }

  /** A durable event frame from the stream: the activity list and keep-awake follow it. */
  onStreamFrame(frame: EventFrame): void {
    this.#o.activity.apply(frame.event);
  }

  /** The update status changed (a download finished, an install failed). */
  onUpdateStatus(status: UpdateStatus): void {
    this.#o.windows.broadcast(["main"], "update.status", status);
    if (status.state === "downloaded" && this.installWhenIdleEnabled()) {
      this.#o.updateGate.installWhenIdle();
    }
  }

  /** What a click on a native notification does: the main window opens where it points. */
  onNoticeClicked(notice: DesktopNotice): void {
    const command = commandForNotice(notice);
    this.command(command ?? { kind: "find" }, command === null);
  }

  /** Run a command in the main window, bringing it forward unless told not to. Queued if it has not subscribed yet. */
  command(command: ShellCommand, quiet = false): void {
    const main = quiet ? this.#o.windows.openMain() : this.#o.windows.showMain();
    this.#o.windows.deliver(main, "shell.command", command);
  }

  /** A global shortcut fired. */
  onGlobalShortcut(id: GlobalShortcutId): void {
    if (id === "toggle_window") {
      const main = this.#o.windows.single("main");
      if (main && main.handle.isVisible() && main.handle.isFocused() && !main.handle.isMinimized()) {
        main.handle.hide();
      } else {
        this.#o.windows.showMain();
      }
      return;
    }
    this.#o.windows.summonQuickEntry(this.#o.pointer());
  }

  /** The menu's "Check for Updates…". */
  checkForUpdates(): void {
    this.#o.checkForUpdates();
  }

  /** Quit through the confirmation (the menu, the tray and `Cmd+Q` all come here). */
  requestQuit(): void {
    this.#o.quitApp();
  }

  // ---- work status ----

  /** What is running, from the server's maintenance status and what this desktop has seen. */
  async probe(): Promise<WorkStatus> {
    if (!this.#o.bridge.signedIn()) {
      return { kind: "idle" };
    }
    return workStatus(await this.#o.bridge.maintenance(), this.#o.activity.runningCount());
  }

  // ---- menu, indicator, activity ----

  refreshMenu(): void {
    this.#o.setMenu(
      buildMenu({
        platform: this.#o.platform,
        bindings: this.#o.shortcuts.windowBindings(),
        focused: this.#focused?.type ?? null,
        maxBindingLength: this.#o.config.shortcutMaxLength,
        paused: this.#o.shortcuts.recording,
      }),
    );
  }

  #windowEvent(window: ManagedWindow, event: WindowEvent): void {
    const id = window.handle.webContentsId;
    if (event === "focus") {
      this.#focused = window;
      this.refreshMenu();
      this.#o.connectivity.focused();
      this.#updateIndicatorVisibility();
    } else if (event === "blur") {
      if (this.#focused === window) {
        this.#focused = null;
        this.refreshMenu();
      }
      this.#o.shortcuts.windowEvent(id, "blur");
      if (window.type === "quick_entry" && window.handle.isVisible()) {
        window.handle.hide();
      }
      this.#updateIndicatorVisibility();
    } else if (event === "navigated" || event === "crashed" || event === "closed") {
      this.#o.shortcuts.windowEvent(id, event === "navigated" ? "navigation" : event === "crashed" ? "crash" : "closed");
      if (event === "closed" && this.#focused === window) {
        this.#focused = null;
        this.refreshMenu();
      }
    } else {
      // "moved": nothing to do (the indicator persists its position when the person drags it).
    }
  }

  #activityChanged(): void {
    this.#o.keepAwake.update({ running: this.#o.activity.runningCount() > 0 });
    const items = this.#o.activity.items();
    this.#o.windows.broadcast(["main", "indicator"], "shell.activity", { items });
    this.#updateIndicatorVisibility();
  }

  /** The indicator shows while something is listed and the person is not already looking at the app. */
  #updateIndicatorVisibility(): void {
    const listed = this.#o.activity.items().length > 0;
    // The window is made the first time there is something to show, and only hidden afterwards.
    const indicator = listed ? this.#o.windows.indicator() : this.#o.windows.single("indicator");
    if (!indicator || indicator.handle.isDestroyed()) {
      return;
    }
    const looking = this.#o.windows.ofType("main").concat(this.#o.windows.ofType("popout")).some((window) => window.handle.isFocused());
    const show = listed && !looking;
    if (show && !indicator.handle.isVisible()) {
      this.#placeIndicator();
      indicator.handle.showInactive();
    } else if (!show && indicator.handle.isVisible()) {
      indicator.handle.hide();
    }
  }

  #placeIndicator(): void {
    const indicator = this.#o.windows.single("indicator");
    if (!indicator || indicator.handle.isDestroyed()) {
      return;
    }
    const saved = this.#o.preferences.current().indicator;
    const displays = this.#o.displays.all();
    // The display the person last left it on, if still attached; otherwise the primary one.
    const display = displays.find((candidate) => saved[candidate.key] !== undefined) ?? this.#o.displays.primary();
    const origin = indicatorOrigin(display, saved);
    const bounds = indicator.handle.getBounds();
    indicator.handle.setBounds({ x: origin.x, y: origin.y, width: bounds.width, height: bounds.height });
  }

  // ---- the shell's IPC channels ----

  handlers(): Handlers {
    const o = this.#o;
    const parse = <T>(schema: s.Schema<T>, payload: unknown): { readonly ok: true; readonly value: T } | { readonly ok: false; readonly error: ReturnType<typeof refused> } => {
      const parsed = s.safeParse(schema, payload);
      return parsed.ok ? parsed : { ok: false, error: refused("VALIDATION_SCHEMA", parsed.error.message) };
    };
    const preferences = (): Record<string, unknown> => ({
      keepAwake: this.keepAwakeEnabled(),
      installWhenIdle: this.installWhenIdleEnabled(),
      installOnQuit: o.updates.installOnQuit,
      waitingForIdle: o.updateGate.waitingForIdle,
      updateStatus: o.updates.status(),
      platform: o.platform,
    });
    return {
      "shell.window.context": (window, payload) =>
        Promise.resolve(parse(NOTHING, payload)).then((input) =>
          input.ok ? { ok: true, result: { type: window.type, launch: window.launch, platform: o.platform, indicatorEnabled: o.config.indicatorEnabled, quickEntryEnabled: o.config.quickEntryEnabled } } : input.error,
        ),
      "shell.pending.consume": (window, payload) =>
        Promise.resolve(parse(NOTHING, payload)).then((input) => (input.ok ? { ok: true, result: { events: o.windows.consumePending(window) } } : input.error)),
      "shell.connectivity.report": (_window, payload) =>
        Promise.resolve(parse(ONLINE, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          o.connectivity.report(input.value.online);
          return { ok: true, result: { state: o.connectivity.state } };
        }),
      "shell.shortcuts.get": (_window, payload) =>
        Promise.resolve(parse(NOTHING, payload)).then((input) => (input.ok ? { ok: true, result: { ...o.shortcuts.snapshot(), platform: o.platform } } : input.error)),
      "shell.shortcuts.set": (_window, payload) =>
        Promise.resolve(parse(SET_SHORTCUT, payload)).then((input) => (input.ok ? shortcutResult(o.shortcuts.setBinding(input.value.id, input.value.binding)) : input.error)),
      "shell.shortcuts.reset": (_window, payload) =>
        Promise.resolve(parse(RESET_SHORTCUT, payload)).then((input) => (input.ok ? shortcutResult(o.shortcuts.resetBinding(input.value.id)) : input.error)),
      "shell.recording.begin": (window, payload) =>
        Promise.resolve(parse(NOTHING, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          return o.shortcuts.beginRecording(window.handle.webContentsId) ? { ok: true, result: { recording: true } } : refused("CONFLICT_STATE", "another window is recording a shortcut");
        }),
      "shell.recording.end": (window, payload) =>
        Promise.resolve(parse(NOTHING, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          // Only the window that is recording may end it; any other window's call changes nothing.
          const ended = o.shortcuts.recordingWindow() === window.handle.webContentsId ? o.shortcuts.endRecording("done") : false;
          return { ok: true, result: { ended } };
        }),
      "shell.preferences.get": (_window, payload) => Promise.resolve(parse(NOTHING, payload)).then((input) => (input.ok ? { ok: true, result: preferences() } : input.error)),
      "shell.preferences.set": (_window, payload) =>
        Promise.resolve(parse(PREFERENCES_SET, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          const change = input.value;
          o.preferences.update((previous) => ({
            ...previous,
            keepAwake: change.keepAwake ?? previous.keepAwake,
            installWhenIdle: change.installWhenIdle ?? previous.installWhenIdle,
            installOnQuit: change.installOnQuit ?? previous.installOnQuit,
          }));
          if (change.keepAwake !== undefined) {
            o.keepAwake.update({ enabled: change.keepAwake });
          }
          if (change.installOnQuit !== undefined) {
            o.updates.setInstallOnQuit(change.installOnQuit);
          }
          if (change.installWhenIdle === true) {
            o.updateGate.installWhenIdle();
          } else if (change.installWhenIdle === false) {
            o.updateGate.cancelWhenIdle();
          }
          return { ok: true, result: preferences() };
        }),
      "shell.popout.open": (_window, payload) =>
        Promise.resolve(parse(POPOUT_OPEN, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          const { threadId, tab, sidebar, x, y } = input.value;
          if ((x === undefined) !== (y === undefined)) {
            return refused("VALIDATION_SCHEMA", "x and y are given together or not at all");
          }
          const opened = o.windows.openPopout({ threadId, tab: tab ?? "conversation", sidebar: sidebar ?? false }, x !== undefined && y !== undefined ? { x, y } : undefined);
          return opened.ok ? { ok: true, result: { reused: opened.reused } } : refused(opened.code, opened.message);
        }),
      "shell.popout.pin": (window, payload) =>
        Promise.resolve(parse(PINNED, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          return o.windows.pinPopout(window, input.value.pinned) ? { ok: true, result: { pinned: input.value.pinned } } : refused("POLICY_DENIED", "this window cannot be kept on top");
        }),
      "shell.window.close": (window, payload) =>
        Promise.resolve(parse(NOTHING, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          // Quick entry is summoned again and again: closing it hides it.
          if (window.type === "quick_entry") {
            window.handle.hide();
          } else {
            window.handle.close();
          }
          return { ok: true, result: { closed: true } };
        }),
      "shell.quickEntry.submit": (window, payload) =>
        Promise.resolve(parse(QUICK_ENTRY, payload)).then(async (input) => {
          if (!input.ok) {
            return input.error;
          }
          // The same PostMessage command path the main window uses: a new thread, no attachments, no authority of its own.
          const result = await settled(o.bridge.postMessage({ threadId: null, content: input.value.content, ...(input.value.teammateId ? { teammateId: input.value.teammateId } : {}) }));
          if (result.ok) {
            window.handle.hide();
          }
          return result;
        }),
      "shell.activity.list": (_window, payload) => Promise.resolve(parse(NOTHING, payload)).then((input) => (input.ok ? { ok: true, result: { items: o.activity.items() } } : input.error)),
      "shell.activity.dismiss": (_window, payload) =>
        Promise.resolve(parse(KEY, payload)).then((input) => (input.ok ? { ok: true, result: { dismissed: o.activity.dismiss(input.value.key) } } : input.error)),
      "shell.activity.open": (_window, payload) =>
        Promise.resolve(parse(KEY, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          const item = o.activity.get(input.value.key);
          if (item === null) {
            return refused("NOT_FOUND", "that item is no longer listed");
          }
          const command = commandForItem(item);
          if (command === null) {
            return refused("NOT_FOUND", "that item points at nothing to open");
          }
          this.command(command);
          return { ok: true, result: { opened: true } };
        }),
      "shell.indicator.interactive": (window, payload) =>
        Promise.resolve(parse(INTERACTIVE, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          // Only the list and the anchor take the mouse; everywhere else it falls through to what is underneath.
          window.handle.setIgnoreMouseEvents(!input.value.active, true);
          return { ok: true, result: { interactive: input.value.active } };
        }),
      "shell.indicator.moveTo": (window, payload) =>
        Promise.resolve(parse(MOVE, payload)).then((input) => {
          if (!input.ok) {
            return input.error;
          }
          const display = o.displays.nearest(input.value);
          const placed = indicatorPosition(display, input.value);
          const bounds = window.handle.getBounds();
          window.handle.setBounds({ x: placed.origin.x, y: placed.origin.y, width: bounds.width, height: bounds.height });
          o.preferences.update((previous) => ({ ...previous, indicator: { ...previous.indicator, [placed.key]: placed.position } }));
          return { ok: true, result: { x: placed.origin.x, y: placed.origin.y } };
        }),
      "update.install": (_window, payload) =>
        Promise.resolve(parse(INSTALL, payload)).then(async (input) => {
          if (!input.ok) {
            return input.error;
          }
          if (input.value.mode === "cancel_when_idle") {
            o.updateGate.cancelWhenIdle();
            return { ok: true, result: { kind: "deferred" } };
          }
          if (input.value.mode === "when_idle") {
            if (!o.updates.downloaded) {
              return { ok: true, result: { kind: "not_downloaded" } };
            }
            o.updateGate.installWhenIdle();
            return { ok: true, result: { kind: "waiting_for_idle" } };
          }
          return { ok: true, result: await o.updateGate.requestInstall() };
        }),
    };
  }
}

/** Where opening an activity item goes. */
export function commandForItem(item: ActivityItem): ShellCommand | null {
  if (item.ref.approvalId) {
    return { kind: "open-approval", approvalId: item.ref.approvalId };
  }
  if (item.ref.runId) {
    return { kind: "open-run", runId: item.ref.runId };
  }
  return null;
}
