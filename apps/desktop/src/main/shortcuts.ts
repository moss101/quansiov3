/**
 * Shortcuts (ADP-004, DOMAIN.md §13.6 D): rebindable window shortcuts, global shortcuts, conflict detection,
 * rollback and the recording mode.
 *
 * Window shortcuts are menu accelerators the person may rebind; global shortcuts are registered with the
 * operating system and fire in any application. Both kinds share one rule: a binding is applied only when it is
 * valid, conflicts with nothing, and (for a global shortcut) the operating system accepted it. Any other outcome
 * leaves the previous binding exactly as it was and says why with a typed result, so a half-applied change cannot
 * exist. The registry registers only what the person's stored preferences name (plus the configured default for
 * the one toggle shortcut), never a shortcut nobody configured.
 *
 * While the person records a new binding, every global shortcut is released and the menu drops its accelerators,
 * so the key they press reaches the page and not a shortcut. Recording ends on its own when the window loses
 * focus, navigates, closes or its renderer crashes; the shortcuts come back.
 */

import { bindingProblem, parseBinding, toAccelerator, displayBinding, type BindingProblem } from "../shortcut-grammar.js";
import type { PreferencesFile } from "./preferences.js";

export const WINDOW_SHORTCUTS = ["new_thread", "find", "open_settings", "popout_thread"] as const;
export const GLOBAL_SHORTCUTS = ["toggle_window", "quick_entry"] as const;
export type WindowShortcutId = (typeof WINDOW_SHORTCUTS)[number];
export type GlobalShortcutId = (typeof GLOBAL_SHORTCUTS)[number];
export type ShortcutId = WindowShortcutId | GlobalShortcutId;

export function isWindowShortcut(id: string): id is WindowShortcutId {
  return (WINDOW_SHORTCUTS as readonly string[]).includes(id);
}

export function isGlobalShortcut(id: string): id is GlobalShortcutId {
  return (GLOBAL_SHORTCUTS as readonly string[]).includes(id);
}

/** What a window shortcut is bound to until the person rebinds it. */
export const WINDOW_DEFAULTS: Readonly<Record<WindowShortcutId, string>> = {
  new_thread: "mod+n",
  find: "mod+k",
  open_settings: "mod+comma",
  popout_thread: "mod+shift+o",
};

/**
 * The menu's own accelerators, which cannot be rebound and which no binding may take. The menu is built from
 * this table (`menu.ts`), so the conflict check and the menu cannot disagree.
 */
export const FIXED_BINDINGS: Readonly<Record<string, string>> = {
  close_window: "mod+w",
  minimize: "mod+m",
  quit: "mod+q",
  hide: "mod+h",
  zoom_in: "mod+equal",
  zoom_out: "mod+minus",
  zoom_reset: "mod+0",
  undo: "mod+z",
  redo: "mod+shift+z",
  cut: "mod+x",
  copy: "mod+c",
  paste: "mod+v",
  select_all: "mod+a",
};

/** The slice of Electron's `globalShortcut` the registry uses; `register` answers whether the OS accepted it. */
export interface GlobalShortcutAdapter {
  register(accelerator: string, handler: () => void): boolean;
  unregister(accelerator: string): void;
}

export type SetFailure =
  | { readonly ok: false; readonly code: "unknown_shortcut"; readonly message: string }
  | { readonly ok: false; readonly code: "invalid"; readonly problem: BindingProblem; readonly message: string }
  | { readonly ok: false; readonly code: "conflict"; readonly with: string; readonly message: string }
  | { readonly ok: false; readonly code: "registration_failed"; readonly message: string };

export interface ShortcutView {
  readonly id: ShortcutId;
  readonly scope: "window" | "global";
  /** The binding in force; `null` when the person cleared it or none is set. */
  readonly binding: string | null;
  readonly display: string | null;
  readonly default: string | null;
  /** For a global shortcut: whether the operating system holds it now. */
  readonly registered: boolean;
  /** Why a stored global binding is not registered, when it is not. */
  readonly failure: string | null;
}

export interface ShortcutSnapshot {
  readonly shortcuts: readonly ShortcutView[];
  readonly recording: boolean;
}

export type SetResult = { readonly ok: true; readonly snapshot: ShortcutSnapshot } | SetFailure;

export type RecordingEnd = "done" | "blur" | "navigation" | "crash" | "closed" | "set";

export interface ShortcutRegistryOptions {
  readonly platform: string;
  readonly maxLength: number;
  readonly toggleDefault: string | null;
  readonly adapter: GlobalShortcutAdapter;
  readonly preferences: PreferencesFile;
  /** Called when a global shortcut fires. */
  readonly onGlobal: (id: GlobalShortcutId) => void;
  readonly log: (line: string) => void;
}

export class ShortcutRegistry {
  readonly #options: ShortcutRegistryOptions;
  /** Global shortcuts the operating system holds, with the accelerator each was registered under. */
  readonly #held = new Map<GlobalShortcutId, string>();
  readonly #failures = new Map<GlobalShortcutId, string>();
  readonly #changed = new Set<() => void>();
  readonly #recordingEnded = new Set<(reason: RecordingEnd) => void>();
  #recording: number | null = null;

  constructor(options: ShortcutRegistryOptions) {
    this.#options = options;
  }

  /** Called whenever a binding, a registration or the recording state changes. */
  onChange(listener: () => void): () => void {
    this.#changed.add(listener);
    return () => this.#changed.delete(listener);
  }

  /** Called when recording ends, whichever way it ended. */
  onRecordingEnded(listener: (reason: RecordingEnd) => void): () => void {
    this.#recordingEnded.add(listener);
    return () => this.#recordingEnded.delete(listener);
  }

  #defaultOf(id: ShortcutId): string | null {
    if (isWindowShortcut(id)) {
      return WINDOW_DEFAULTS[id];
    }
    return id === "toggle_window" ? this.#options.toggleDefault : null;
  }

  /** The binding in force: stored when the person chose (including "none"), the default otherwise. */
  binding(id: ShortcutId): string | null {
    const stored = this.#options.preferences.current().shortcuts[id];
    return stored === undefined ? this.#defaultOf(id) : stored;
  }

  /** Window shortcut bindings for the menu, which drops them all while a binding is being recorded. */
  windowBindings(): Readonly<Record<WindowShortcutId, string | null>> {
    const out = {} as Record<WindowShortcutId, string | null>;
    for (const id of WINDOW_SHORTCUTS) {
      out[id] = this.#recording === null ? this.binding(id) : null;
    }
    return out;
  }

  /** Whether accelerators are paused for a recording in progress. */
  get recording(): boolean {
    return this.#recording !== null;
  }

  snapshot(): ShortcutSnapshot {
    const shortcuts: ShortcutView[] = [...WINDOW_SHORTCUTS, ...GLOBAL_SHORTCUTS].map((id) => {
      const binding = this.binding(id);
      const global = isGlobalShortcut(id);
      return {
        id,
        scope: global ? "global" : "window",
        binding,
        display: binding === null ? null : displayBinding(binding, this.#options.platform),
        default: this.#defaultOf(id),
        registered: global && this.#held.has(id),
        failure: global ? (this.#failures.get(id) ?? null) : null,
      };
    });
    return { shortcuts, recording: this.#recording !== null };
  }

  #emit(): void {
    for (const listener of this.#changed) {
      listener();
    }
  }

  #acceleratorOf(binding: string): string | null {
    const parsed = parseBinding(binding, this.#options.maxLength);
    return parsed === null ? null : toAccelerator(parsed, this.#options.platform);
  }

  /** Try to register `binding` for a global shortcut; the reason it did not take, or `null`. */
  #register(id: GlobalShortcutId, binding: string): string | null {
    const accelerator = this.#acceleratorOf(binding);
    if (accelerator === null) {
      return "the stored binding is not valid";
    }
    try {
      const accepted = this.#options.adapter.register(accelerator, () => {
        this.#options.onGlobal(id);
      });
      if (!accepted) {
        return "the system declined it (another application may hold it)";
      }
    } catch (error) {
      return error instanceof Error ? error.message : "the system refused it";
    }
    this.#held.set(id, accelerator);
    return null;
  }

  #release(id: GlobalShortcutId): void {
    const accelerator = this.#held.get(id);
    if (accelerator !== undefined) {
      this.#held.delete(id);
      try {
        this.#options.adapter.unregister(accelerator);
      } catch (error) {
        this.#options.log(`quansio desktop: could not release ${accelerator}: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }

  /**
   * Register the global shortcuts the person's preferences name. Nothing else is registered: a shortcut that
   * is cleared (stored `null`) or has no default and no stored binding stays unregistered.
   */
  registerStored(): void {
    for (const id of GLOBAL_SHORTCUTS) {
      if (this.#held.has(id)) {
        continue;
      }
      const binding = this.binding(id);
      if (binding === null) {
        this.#failures.delete(id);
        continue;
      }
      const failure = this.#register(id, binding);
      if (failure === null) {
        this.#failures.delete(id);
      } else {
        this.#failures.set(id, failure);
        this.#options.log(`quansio desktop: global shortcut ${id} (${binding}) is not registered: ${failure}`);
      }
    }
    this.#emit();
  }

  /** Release every global shortcut (quitting). */
  releaseAll(): void {
    for (const id of GLOBAL_SHORTCUTS) {
      this.#release(id);
    }
  }

  /** Why `binding` cannot be taken by `id`: the shortcut or menu entry that already has it. */
  #conflict(id: ShortcutId, binding: string): string | null {
    for (const [name, fixed] of Object.entries(FIXED_BINDINGS)) {
      if (fixed === binding) {
        return `the menu's ${name.replace(/_/g, " ")}`;
      }
    }
    for (const other of [...WINDOW_SHORTCUTS, ...GLOBAL_SHORTCUTS]) {
      if (other !== id && this.binding(other) === binding) {
        return other.replace(/_/g, " ");
      }
    }
    return null;
  }

  /**
   * Set (or clear, with `null`) a shortcut. Every refusal leaves the previous binding, and the operating
   * system's registrations, exactly as they were.
   */
  setBinding(id: string, binding: string | null): SetResult {
    if (!isWindowShortcut(id) && !isGlobalShortcut(id)) {
      return { ok: false, code: "unknown_shortcut", message: `${id} is not a shortcut` };
    }
    // A change made while recording ends the recording first, so the others are back before this one is tried.
    this.endRecording("set");
    if (binding !== null) {
      const problem = bindingProblem(binding, this.#options.maxLength);
      if (problem !== null) {
        return { ok: false, code: "invalid", problem, message: `${binding} is not a valid binding (${problem.replace(/_/g, " ")})` };
      }
      const taken = this.#conflict(id, binding);
      if (taken !== null) {
        return { ok: false, code: "conflict", with: taken, message: `${binding} is already ${taken}` };
      }
    }
    if (isGlobalShortcut(id)) {
      const previous = this.#held.get(id);
      const previousBinding = this.binding(id);
      if (previous !== undefined) {
        this.#release(id);
      }
      if (binding !== null) {
        const failure = this.#register(id, binding);
        if (failure !== null) {
          // Roll back: the previous binding comes back, and nothing is stored.
          let restored = previous === undefined;
          if (previous !== undefined && previousBinding !== null) {
            restored = this.#register(id, previousBinding) === null;
          }
          if (!restored) {
            this.#failures.set(id, "the previous binding could not be restored after a failed change");
          }
          this.#emit();
          return { ok: false, code: "registration_failed", message: `${binding} could not be registered: ${failure}` };
        }
      }
      this.#failures.delete(id);
    }
    this.#options.preferences.update((previous) => ({ ...previous, shortcuts: { ...previous.shortcuts, [id]: binding } }));
    this.#emit();
    return { ok: true, snapshot: this.snapshot() };
  }

  /** Restore one shortcut to its default (the stored choice is removed). */
  resetBinding(id: string): SetResult {
    if (!isWindowShortcut(id) && !isGlobalShortcut(id)) {
      return { ok: false, code: "unknown_shortcut", message: `${id} is not a shortcut` };
    }
    const result = this.setBinding(id, this.#defaultOf(id));
    if (result.ok) {
      this.#options.preferences.update((previous) => ({
        ...previous,
        shortcuts: Object.fromEntries(Object.entries(previous.shortcuts).filter(([key]) => key !== id)),
      }));
    }
    return result;
  }

  /**
   * Start recording for one window: every global shortcut is released and the menu loses its accelerators until
   * {@link endRecording}. A second window cannot start while one is recording.
   */
  beginRecording(windowId: number): boolean {
    if (this.#recording !== null && this.#recording !== windowId) {
      return false;
    }
    if (this.#recording === null) {
      this.#recording = windowId;
      for (const id of GLOBAL_SHORTCUTS) {
        this.#release(id);
      }
      this.#emit();
    }
    return true;
  }

  /** The window that is recording, if any. */
  recordingWindow(): number | null {
    return this.#recording;
  }

  /** End recording (a no-op when none is in progress); the global shortcuts are registered again. */
  endRecording(reason: RecordingEnd): boolean {
    if (this.#recording === null) {
      return false;
    }
    this.#recording = null;
    this.registerStored();
    for (const listener of this.#recordingEnded) {
      listener(reason);
    }
    return true;
  }

  /** The window that was recording lost focus, navigated, closed or crashed: recording ends. */
  windowEvent(windowId: number, event: "blur" | "navigation" | "crash" | "closed"): void {
    if (this.#recording === windowId) {
      this.endRecording(event);
    }
  }
}
