/**
 * Doubles for the shell's Electron-facing adapters (ADP-004): a window factory whose windows record what is done to
 * them, and a two-display screen. The logic under test (the window registry, the IPC guard, the shell host) is the real
 * code; only the operating-system edge is replaced, and the real-Electron smoke (`shell.e2e.test.ts`) covers that edge.
 */

import type { AutoUpdaterLike } from "../src/main/updates.js";
import type { PowerBlockerAdapter } from "../src/main/keep-awake.js";
import type { GlobalShortcutAdapter } from "../src/main/shortcuts.js";
import type { DisplayAdapter, DisplayInfo, Rect, WindowEvent, WindowFactory, WindowHandle, WindowSpec } from "../src/main/windows.js";

export class FakeWindow implements WindowHandle {
  readonly webContentsId: number;
  readonly spec: WindowSpec;
  readonly sent: { channel: string; payload: unknown }[] = [];
  readonly calls: string[] = [];
  loaded: string | null = null;
  destroyed = false;
  visible = false;
  focused = false;
  minimized = false;
  alwaysOnTop = false;
  ignoreMouse: { ignore: boolean; forward: boolean } | null = null;
  bounds: Rect;
  readonly #listeners = new Map<WindowEvent, () => void>();

  constructor(id: number, spec: WindowSpec) {
    this.webContentsId = id;
    this.spec = spec;
    this.bounds = { x: spec.options.x ?? 0, y: spec.options.y ?? 0, width: spec.options.width, height: spec.options.height };
    this.alwaysOnTop = spec.options.alwaysOnTop === true;
  }

  loadUrl(url: string): Promise<void> {
    this.loaded = url;
    return Promise.resolve();
  }
  show(): void {
    this.calls.push("show");
    this.visible = true;
  }
  showInactive(): void {
    this.calls.push("showInactive");
    this.visible = true;
  }
  hide(): void {
    this.calls.push("hide");
    this.visible = false;
  }
  focus(): void {
    this.calls.push("focus");
    this.focused = true;
  }
  minimize(): void {
    this.calls.push("minimize");
    this.minimized = true;
  }
  restore(): void {
    this.calls.push("restore");
    this.minimized = false;
  }
  close(): void {
    this.calls.push("close");
    this.destroyed = true;
    this.#listeners.get("closed")?.();
  }
  isDestroyed(): boolean {
    return this.destroyed;
  }
  isVisible(): boolean {
    return this.visible;
  }
  isMinimized(): boolean {
    return this.minimized;
  }
  isFocused(): boolean {
    return this.focused;
  }
  send(channel: string, payload: unknown): void {
    this.sent.push({ channel, payload });
  }
  setAlwaysOnTop(pinned: boolean): void {
    this.alwaysOnTop = pinned;
  }
  getBounds(): Rect {
    return this.bounds;
  }
  setBounds(bounds: Rect): void {
    this.bounds = bounds;
  }
  setIgnoreMouseEvents(ignore: boolean, forward: boolean): void {
    this.ignoreMouse = { ignore, forward };
  }
  on(event: WindowEvent, listener: () => void): void {
    this.#listeners.set(event, listener);
  }
  /** Play an event the operating system would deliver. */
  emit(event: WindowEvent): void {
    if (event === "focus") {
      this.focused = true;
    } else if (event === "blur") {
      this.focused = false;
    }
    this.#listeners.get(event)?.();
  }
}

export class FakeFactory implements WindowFactory {
  readonly windows: FakeWindow[] = [];
  #id = 100;

  create(spec: WindowSpec): WindowHandle {
    this.#id += 1;
    const window = new FakeWindow(this.#id, spec);
    this.windows.push(window);
    return window;
  }

  ofType(type: string): FakeWindow[] {
    return this.windows.filter((window) => window.spec.type === type);
  }
}

export const LEFT: DisplayInfo = { key: "d1", bounds: { x: 0, y: 0, width: 1920, height: 1080 }, workArea: { x: 0, y: 25, width: 1920, height: 1055 } };
export const RIGHT: DisplayInfo = { key: "d2", bounds: { x: 1920, y: 0, width: 1280, height: 800 }, workArea: { x: 1920, y: 0, width: 1280, height: 800 } };

export class FakeDisplays implements DisplayAdapter {
  attached: DisplayInfo[] = [LEFT, RIGHT];

  all(): readonly DisplayInfo[] {
    return this.attached;
  }
  primary(): DisplayInfo {
    return this.attached[0] ?? LEFT;
  }
  nearest(point: { x: number; y: number }): DisplayInfo {
    // Like the operating system's: the display the point is on, else the closest one.
    const distance = (display: DisplayInfo): number => {
      const dx = Math.max(display.bounds.x - point.x, 0, point.x - (display.bounds.x + display.bounds.width - 1));
      const dy = Math.max(display.bounds.y - point.y, 0, point.y - (display.bounds.y + display.bounds.height - 1));
      return dx * dx + dy * dy;
    };
    return [...this.attached].sort((a, b) => distance(a) - distance(b))[0] ?? this.primary();
  }
}

/** A frame URL the shell's own pages have. */
export const TRUSTED_URL = "app://desktop/index.html";

/** The shape of an `ipcMain.handle` event for a call from `webContentsId` at `url`. */
export function ipcEvent(webContentsId: number, url: string | null = TRUSTED_URL, parent: unknown = null): unknown {
  return { sender: { id: webContentsId }, senderFrame: url === null ? null : { url, parent } };
}

export const THREAD = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
export const OTHER_THREAD = "thr_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";
export const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
export const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

/** A clock whose timers fire only when the test advances it. */
export class Clock {
  now = 1_000_000;
  #next = 1;
  readonly #timers = new Map<number, { at: number; run: () => void }>();

  setTimer = (run: () => void, delay: number): unknown => {
    const id = this.#next++;
    this.#timers.set(id, { at: this.now + delay, run });
    return id;
  };

  clearTimer = (handle: unknown): void => {
    this.#timers.delete(handle as number);
  };

  get pending(): number {
    return this.#timers.size;
  }

  advance(ms: number): void {
    const target = this.now + ms;
    for (;;) {
      const due = [...this.#timers.entries()].filter(([, timer]) => timer.at <= target).sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) {
        break;
      }
      this.#timers.delete(due[0]);
      this.now = due[1].at;
      due[1].run();
    }
    this.now = target;
  }
}



/** An updater that lets a test play the platform: finish a download, or fail an install. */
export class FakeUpdater implements AutoUpdaterLike {
  installs = 0;
  throwOnInstall = false;
  readonly #listeners = new Map<string, ((...args: never[]) => void)[]>();
  setFeedURL(): void {
    return undefined;
  }
  checkForUpdates(): void {
    return undefined;
  }
  quitAndInstall(): void {
    this.installs += 1;
    if (this.throwOnInstall) {
      throw new Error("could not start the installer");
    }
  }
  on(event: string, listener: (...args: never[]) => void): this {
    this.#listeners.set(event, [...(this.#listeners.get(event) ?? []), listener]);
    return this;
  }
  emit(event: string, ...args: unknown[]): void {
    for (const listener of this.#listeners.get(event) ?? []) {
      (listener as (...rest: unknown[]) => void)(...args);
    }
  }
}



/** A global-shortcut adapter the test controls: which accelerators the "system" declines, and what it holds. */
export class FakeSystem implements GlobalShortcutAdapter {
  readonly held = new Map<string, () => void>();
  readonly registered: string[] = [];
  declined = new Set<string>();
  /** Decline registering `accelerator` once this many registrations have succeeded (to fail a restore). */
  throwOn = new Set<string>();

  register(accelerator: string, handler: () => void): boolean {
    this.registered.push(accelerator);
    if (this.throwOn.has(accelerator)) {
      throw new Error("the system refused");
    }
    if (this.declined.has(accelerator) || this.held.has(accelerator)) {
      return false;
    }
    this.held.set(accelerator, handler);
    return true;
  }

  unregister(accelerator: string): void {
    this.held.delete(accelerator);
  }
}



/** A power-save blocker that counts what is asked of it. */
export class Blocker implements PowerBlockerAdapter {
  started = 0;
  stopped: number[] = [];
  start(): number {
    this.started += 1;
    return this.started;
  }
  stop(id: number): void {
    this.stopped.push(id);
  }
}


