/**
 * Windows (ADP-004, DOMAIN.md §13.6 B): the four window types, how each is created, and the registry the IPC trust
 * check consults.
 *
 * The main process creates every window and remembers it by its `webContents` id and its type. A window it did
 * not create is unmanaged and is refused everything. All types share one baseline (`RENDERER_WEB_PREFERENCES`:
 * sandbox, context isolation, no Node, `webSecurity`) and the one preload, load only the `app:` scheme and are
 * built from the same description (`windowSpec`), so a type cannot be created with a weaker setting by omission.
 *
 * Nothing here touches Electron: a {@link WindowFactory} turns a {@link WindowSpec} into a real `BrowserWindow`
 * (`app.ts`), and tests supply a fake one.
 */

import { RENDERER_ENTRY_URL, RENDERER_WEB_PREFERENCES, navigationAllowed } from "./security.js";
import * as s from "./schema.js";
import type { DesktopShellConfig } from "./shell-config.js";
import type { IndicatorPosition } from "./preferences.js";

export const WINDOW_TYPES = ["main", "popout", "quick_entry", "indicator"] as const;
export type WindowType = (typeof WINDOW_TYPES)[number];

export interface Rect {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

export interface Point {
  readonly x: number;
  readonly y: number;
}

// ---- launch data ----

export const THREAD_ID = /^thr_[0-9A-HJKMNP-TV-Z]{26}$/;

/** What a pop-out opens with (validated before the window exists). */
export interface PopoutLaunch {
  readonly threadId: string;
  readonly tab: "conversation" | "inspector";
  readonly sidebar: boolean;
}

export const POPOUT_LAUNCH = s.object<PopoutLaunch>({
  threadId: s.string({ min: 30, max: 30, pattern: THREAD_ID }),
  tab: s.oneOf(["conversation", "inspector"] as const),
  sidebar: s.boolean(),
});

export type LaunchData =
  | { readonly type: "main" }
  | ({ readonly type: "popout" } & PopoutLaunch)
  | { readonly type: "quick_entry" }
  | { readonly type: "indicator" };

// ---- window description ----

/** The part of a `BrowserWindow`'s construction the types differ in; everything else is the shared baseline. */
export interface WindowOptions {
  readonly width: number;
  readonly height: number;
  readonly minWidth?: number;
  readonly minHeight?: number;
  readonly x?: number;
  readonly y?: number;
  readonly show: false;
  readonly frame?: boolean;
  readonly transparent?: boolean;
  readonly alwaysOnTop?: boolean;
  readonly skipTaskbar?: boolean;
  readonly resizable?: boolean;
  readonly focusable?: boolean;
  readonly hasShadow?: boolean;
  readonly fullscreenable?: boolean;
  readonly titleBarStyle?: "hiddenInset";
  readonly trafficLightPosition?: Point;
  readonly webPreferences: typeof RENDERER_WEB_PREFERENCES;
}

/** Behaviour applied after the window exists. */
export interface WindowBehaviour {
  /** Mouse events pass through the window except over the parts the page marks interactive (the indicator). */
  readonly clickThrough: boolean;
  /** macOS: visible on every Space and over full-screen apps. */
  readonly visibleOnAllWorkspaces: boolean;
  /** macOS: not shown by Mission Control. */
  readonly hiddenInMissionControl: boolean;
  /** The window hides itself when it loses focus (quick entry). */
  readonly hideOnBlur: boolean;
}

export interface WindowSpec {
  readonly type: WindowType;
  readonly options: WindowOptions;
  readonly behaviour: WindowBehaviour;
  /** Always the bundled renderer under the `app:` scheme. */
  readonly url: string;
  readonly launch: LaunchData;
}

export const INDICATOR_SIZE = { width: 320, height: 380 } as const;
export const QUICK_ENTRY_SIZE = { width: 640, height: 168 } as const;
export const POPOUT_SIZE = { width: 560, height: 760 } as const;

const NO_BEHAVIOUR: WindowBehaviour = { clickThrough: false, visibleOnAllWorkspaces: false, hiddenInMissionControl: false, hideOnBlur: false };

export interface SpecContext {
  readonly platform: string;
  readonly preload: string;
  /** Where a pop-out opens (top-left); absent, the system places it. */
  readonly origin?: Point;
  readonly pinned?: boolean;
}

/** The description of a window of `type`. Every type is built on the shared baseline. */
export function windowSpec(launch: LaunchData, context: SpecContext): WindowSpec {
  const webPreferences = { ...RENDERER_WEB_PREFERENCES, preload: context.preload };
  // The query only names the window for a reader of the log; what a window is comes from main's registry.
  const url = launch.type === "main" ? RENDERER_ENTRY_URL : `${RENDERER_ENTRY_URL}?w=${launch.type}`;
  switch (launch.type) {
    case "main":
      return {
        type: "main",
        url,
        launch,
        behaviour: NO_BEHAVIOUR,
        options: {
          width: 1280,
          height: 860,
          minWidth: 640,
          minHeight: 520,
          show: false,
          // A macOS window with no title bar, the traffic lights inset over the sidebar.
          ...(context.platform === "darwin" ? { titleBarStyle: "hiddenInset" as const, trafficLightPosition: { x: 16, y: 15 } } : {}),
          webPreferences,
        },
      };
    case "popout":
      return {
        type: "popout",
        url,
        launch,
        behaviour: NO_BEHAVIOUR,
        options: {
          ...POPOUT_SIZE,
          minWidth: 420,
          minHeight: 480,
          show: false,
          ...(context.pinned ? { alwaysOnTop: true } : {}),
          ...(context.origin ? { x: Math.round(context.origin.x), y: Math.round(context.origin.y) } : {}),
          ...(context.platform === "darwin" ? { titleBarStyle: "hiddenInset" as const, trafficLightPosition: { x: 16, y: 15 } } : {}),
          webPreferences,
        },
      };
    case "quick_entry":
      return {
        type: "quick_entry",
        url,
        launch,
        behaviour: { ...NO_BEHAVIOUR, hideOnBlur: true },
        options: { ...QUICK_ENTRY_SIZE, show: false, frame: false, resizable: false, skipTaskbar: true, alwaysOnTop: true, fullscreenable: false, webPreferences },
      };
    case "indicator":
      return {
        type: "indicator",
        url,
        launch,
        behaviour: { clickThrough: true, visibleOnAllWorkspaces: true, hiddenInMissionControl: true, hideOnBlur: false },
        options: {
          ...INDICATOR_SIZE,
          show: false,
          frame: false,
          transparent: true,
          hasShadow: false,
          resizable: false,
          focusable: false,
          skipTaskbar: true,
          alwaysOnTop: true,
          fullscreenable: false,
          webPreferences,
        },
      };
  }
}

/** Whether a spec meets the baseline every window type must: the checks the tests (and a startup assertion) use. */
export function specMeetsBaseline(spec: WindowSpec): boolean {
  const p = spec.options.webPreferences;
  return (
    p.sandbox &&
    p.contextIsolation &&
    !p.nodeIntegration &&
    !p.nodeIntegrationInWorker &&
    !p.nodeIntegrationInSubFrames &&
    p.webSecurity &&
    !p.allowRunningInsecureContent &&
    !p.webviewTag &&
    !p.enableRemoteModule &&
    p.preload.length > 0 &&
    navigationAllowed(spec.url) &&
    spec.url.startsWith("app://desktop/")
  );
}

// ---- the handle a real window provides ----

export type WindowEvent = "closed" | "blur" | "focus" | "navigated" | "crashed" | "moved";

/** What the manager needs from a created window (Electron's `BrowserWindow`, or a fake in tests). */
export interface WindowHandle {
  readonly webContentsId: number;
  loadUrl(url: string): Promise<void>;
  show(): void;
  showInactive(): void;
  hide(): void;
  focus(): void;
  minimize(): void;
  restore(): void;
  close(): void;
  isDestroyed(): boolean;
  isVisible(): boolean;
  isMinimized(): boolean;
  isFocused(): boolean;
  send(channel: string, payload: unknown): void;
  setAlwaysOnTop(pinned: boolean): void;
  getBounds(): Rect;
  setBounds(bounds: Rect): void;
  setIgnoreMouseEvents(ignore: boolean, forward: boolean): void;
  on(event: WindowEvent, listener: () => void): void;
}

export interface WindowFactory {
  create(spec: WindowSpec): WindowHandle;
}

export interface DisplayInfo {
  /** A key stable enough to remember a position by. */
  readonly key: string;
  readonly bounds: Rect;
  readonly workArea: Rect;
}

export interface DisplayAdapter {
  all(): readonly DisplayInfo[];
  primary(): DisplayInfo;
  nearest(point: Point): DisplayInfo;
}

// ---- pending events ----

/** One event waiting for a window to subscribe. */
export interface PendingEvent {
  readonly channel: string;
  readonly payload: unknown;
}

/** Events sent to a window before its renderer subscribes: bounded, oldest dropped first. */
export class PendingQueue {
  readonly #cap: number;
  #events: PendingEvent[] = [];

  constructor(cap: number) {
    this.#cap = cap;
  }

  push(event: PendingEvent): void {
    this.#events.push(event);
    if (this.#events.length > this.#cap) {
      this.#events = this.#events.slice(this.#events.length - this.#cap);
    }
  }

  /** Take everything, oldest first. */
  drain(): PendingEvent[] {
    const events = this.#events;
    this.#events = [];
    return events;
  }

  clear(): void {
    this.#events = [];
  }

  get size(): number {
    return this.#events.length;
  }
}

// ---- the registry ----

/** A window the shell created. */
export class ManagedWindow {
  readonly type: WindowType;
  readonly launch: LaunchData;
  readonly handle: WindowHandle;
  readonly pending: PendingQueue;
  /** True once the renderer has pulled its pending events: later events are pushed. */
  subscribed = false;
  /** Ids a pop-out's own reads returned: the only ones it may act on besides its thread (DOMAIN.md §13.6 B). */
  readonly known = { questions: new Set<string>(), runs: new Set<string>() };
  closed = false;

  constructor(type: WindowType, launch: LaunchData, handle: WindowHandle, cap: number) {
    this.type = type;
    this.launch = launch;
    this.handle = handle;
    this.pending = new PendingQueue(cap);
  }

  /** The thread a pop-out is for; `null` for every other type. */
  get threadId(): string | null {
    return this.launch.type === "popout" ? this.launch.threadId : null;
  }
}

/** The part of the registry the IPC guard needs. */
export interface WindowLookup {
  byWebContents(id: number): ManagedWindow | null;
}

export type OpenResult =
  | { readonly ok: true; readonly window: ManagedWindow; readonly reused: boolean }
  | { readonly ok: false; readonly code: "VALIDATION_SCHEMA" | "UNAVAILABLE"; readonly message: string };

export interface WindowManagerOptions {
  readonly factory: WindowFactory;
  readonly platform: string;
  readonly preload: string;
  readonly config: DesktopShellConfig;
  readonly displays: DisplayAdapter;
  readonly log: (line: string) => void;
}

export type ManagerEvent = (window: ManagedWindow, event: WindowEvent) => void;

export class WindowManager implements WindowLookup {
  readonly #options: WindowManagerOptions;
  readonly #windows = new Map<number, ManagedWindow>();
  readonly #listeners = new Set<ManagerEvent>();

  constructor(options: WindowManagerOptions) {
    this.#options = options;
  }

  /** Called for every lifecycle event of every managed window. */
  onWindowEvent(listener: ManagerEvent): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  byWebContents(id: number): ManagedWindow | null {
    return this.#windows.get(id) ?? null;
  }

  ofType(type: WindowType): ManagedWindow[] {
    return [...this.#windows.values()].filter((window) => window.type === type && !window.closed);
  }

  /** The one window of a type that has at most one (main, quick entry, indicator). */
  single(type: Exclude<WindowType, "popout">): ManagedWindow | null {
    return this.ofType(type)[0] ?? null;
  }

  #create(launch: LaunchData, context: Omit<SpecContext, "platform" | "preload"> = {}): ManagedWindow {
    const spec = windowSpec(launch, { ...context, platform: this.#options.platform, preload: this.#options.preload });
    if (!specMeetsBaseline(spec)) {
      // Unreachable by construction; refusing is cheaper than reasoning about what a broken spec would do.
      throw new Error(`the ${launch.type} window would not meet the renderer security baseline`);
    }
    const handle = this.#options.factory.create(spec);
    const window = new ManagedWindow(spec.type, launch, handle, this.#options.config.pendingQueueCap);
    this.#windows.set(handle.webContentsId, window);
    for (const event of ["closed", "blur", "focus", "navigated", "crashed", "moved"] as const) {
      handle.on(event, () => {
        if (event === "closed") {
          // The window is gone: its queue is cleared and nothing can reach it again.
          window.closed = true;
          window.pending.clear();
          this.#windows.delete(handle.webContentsId);
        }
        if (event === "navigated") {
          // A navigation starts a new page that has subscribed to nothing.
          window.subscribed = false;
        }
        for (const listener of this.#listeners) {
          listener(window, event);
        }
      });
    }
    handle.loadUrl(spec.url).catch((error: unknown) => {
      this.#options.log(`quansio desktop: could not load the ${spec.type} window: ${error instanceof Error ? error.message : String(error)}`);
    });
    return window;
  }

  /** The main window (created on first use). */
  openMain(): ManagedWindow {
    const existing = this.single("main");
    if (existing) {
      return existing;
    }
    return this.#create({ type: "main" });
  }

  /** Bring the main window forward, restoring it if it was minimized or hidden. */
  showMain(): ManagedWindow {
    const window = this.openMain();
    if (window.handle.isMinimized()) {
      window.handle.restore();
    }
    window.handle.show();
    window.handle.focus();
    return window;
  }

  /**
   * Open a pop-out for a thread. The launch data is validated first; a thread that already has a pop-out gets
   * that window brought forward instead of a second one.
   */
  openPopout(raw: unknown, origin?: Point): OpenResult {
    const parsed = s.safeParse(POPOUT_LAUNCH, raw, "launch");
    if (!parsed.ok) {
      return { ok: false, code: "VALIDATION_SCHEMA", message: parsed.error.message };
    }
    const existing = this.ofType("popout").find((window) => window.threadId === parsed.value.threadId);
    if (existing) {
      existing.handle.show();
      existing.handle.focus();
      return { ok: true, window: existing, reused: true };
    }
    const placed = origin ? this.#placePopout(origin) : undefined;
    const window = this.#create({ type: "popout", ...parsed.value }, placed ? { origin: placed } : {});
    const show = (): void => {
      window.handle.show();
      window.handle.focus();
    };
    show();
    return { ok: true, window, reused: false };
  }

  /** A pop-out's top-left so the window opens under the point it was dropped at, inside the display's work area. */
  #placePopout(drop: Point): Point {
    const area = this.#options.displays.nearest(drop).workArea;
    return clampOrigin({ x: drop.x - POPOUT_SIZE.width / 2, y: drop.y - 24 }, POPOUT_SIZE, area);
  }

  /** Pin or unpin a pop-out above other windows (when the configuration allows it). */
  pinPopout(window: ManagedWindow, pinned: boolean): boolean {
    if (window.type !== "popout" || (pinned && !this.#options.config.popoutAlwaysOnTopAllowed)) {
      return false;
    }
    window.handle.setAlwaysOnTop(pinned);
    return true;
  }

  /** The quick-entry window, created hidden on first use. */
  quickEntry(): ManagedWindow | null {
    if (!this.#options.config.quickEntryEnabled) {
      return null;
    }
    return this.single("quick_entry") ?? this.#create({ type: "quick_entry" });
  }

  /** Summon quick entry: centred near the top of the display the pointer is on, focused. */
  summonQuickEntry(pointer: Point): ManagedWindow | null {
    const window = this.quickEntry();
    if (!window) {
      return null;
    }
    const area = this.#options.displays.nearest(pointer).workArea;
    window.handle.setBounds({
      width: QUICK_ENTRY_SIZE.width,
      height: QUICK_ENTRY_SIZE.height,
      x: Math.round(area.x + (area.width - QUICK_ENTRY_SIZE.width) / 2),
      y: Math.round(area.y + area.height * 0.18),
    });
    window.handle.show();
    window.handle.focus();
    return window;
  }

  /** The activity indicator window, created hidden on first use. */
  indicator(): ManagedWindow | null {
    if (!this.#options.config.indicatorEnabled) {
      return null;
    }
    const existing = this.single("indicator");
    if (existing) {
      return existing;
    }
    const window = this.#create({ type: "indicator" });
    window.handle.setIgnoreMouseEvents(true, true);
    return window;
  }

  /**
   * Send to a window: pushed when its renderer has subscribed, otherwise queued (bounded) until it pulls. Only
   * channels that must not be lost go through here; the stream's own frames are replayed from the cursor.
   */
  deliver(window: ManagedWindow, channel: string, payload: unknown): void {
    if (window.closed || window.handle.isDestroyed()) {
      return;
    }
    if (window.subscribed) {
      window.handle.send(channel, payload);
    } else {
      window.pending.push({ channel, payload });
    }
  }

  /** Push to every managed window of the given types (no queueing). */
  broadcast(types: readonly WindowType[], channel: string, payload: unknown): void {
    for (const window of this.#windows.values()) {
      if (types.includes(window.type) && !window.closed && !window.handle.isDestroyed()) {
        window.handle.send(channel, payload);
      }
    }
  }

  /** The renderer is ready: it takes what waited, and what follows is pushed. */
  consumePending(window: ManagedWindow): PendingEvent[] {
    window.subscribed = true;
    return window.pending.drain();
  }

  all(): ManagedWindow[] {
    return [...this.#windows.values()].filter((window) => !window.closed);
  }

  /** Close every window (quitting). */
  closeAll(): void {
    for (const window of this.all()) {
      if (!window.handle.isDestroyed()) {
        window.handle.close();
      }
    }
  }
}

// ---- geometry ----

/** `origin` moved the least needed to put a window of `size` fully inside `area`. */
export function clampOrigin(origin: Point, size: { readonly width: number; readonly height: number }, area: Rect): Point {
  const x = Math.min(Math.max(origin.x, area.x), Math.max(area.x, area.x + area.width - size.width));
  const y = Math.min(Math.max(origin.y, area.y), Math.max(area.y, area.y + area.height - size.height));
  return { x: Math.round(x), y: Math.round(y) };
}

/** Where the indicator sits before the person has moved it: the top-right corner of the work area. */
export function defaultIndicatorOrigin(area: Rect, margin = 16): Point {
  return clampOrigin({ x: area.x + area.width - INDICATOR_SIZE.width - margin, y: area.y + margin }, INDICATOR_SIZE, area);
}

/**
 * The indicator's top-left on `display`: the position the person left it at there (stored relative to the
 * display), clamped into the work area in case the display changed size, or the default corner.
 */
export function indicatorOrigin(display: DisplayInfo, saved: Readonly<Record<string, IndicatorPosition>>): Point {
  const position = saved[display.key];
  if (!position) {
    return defaultIndicatorOrigin(display.workArea);
  }
  return clampOrigin({ x: display.bounds.x + position.x, y: display.bounds.y + position.y }, INDICATOR_SIZE, display.workArea);
}

/** The position to remember for `origin` on `display`: relative to the display, clamped into its work area. */
export function indicatorPosition(display: DisplayInfo, origin: Point): { readonly key: string; readonly position: IndicatorPosition; readonly origin: Point } {
  const clamped = clampOrigin(origin, INDICATOR_SIZE, display.workArea);
  return { key: display.key, position: { x: clamped.x - display.bounds.x, y: clamped.y - display.bounds.y }, origin: clamped };
}
