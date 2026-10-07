/**
 * IPC trust (ADP-004, DOMAIN.md §13.6 B): who may call what, decided in main before any handler runs.
 *
 * Electron delivers an `invoke` with the frame that sent it. Every call is authorised in three steps, in this order:
 *
 * 1. **The sender's URL.** The frame must be the main frame of a page the shell itself loaded: its URL passes the
 *    same `navigationAllowed` check navigation uses (`app://desktop/…`). Anything else, a subframe, a `data:` page, a
 *    remote site, a frame that is gone, is a logged no-op that answers nothing and never reaches a handler.
 * 2. **The window.** The sender must be a window the shell created (`WindowManager`), and its **type** must be
 *    allowed the channel. The type is what main recorded when it made the window, never anything the page says.
 *    `POLICY` is an exhaustive table: a channel nobody listed is main-only, so a new channel cannot be reachable from
 *    a quick-entry window or the indicator by omission.
 * 3. **The scope.** A pop-out acts on its own thread only. It names its thread, or an id (a question, a run) that its
 *    own reads returned; anything else is refused with `SCOPE_FORBIDDEN`, as is a quick-entry call outside its channels.
 *
 * Steps 2 and 3 answer a refusal as data (`{ok: false, error}`, the shape the settled channels already use); step 1
 * answers nothing at all.
 */

import { BRIDGE_CHANNELS, FILE_CHANNELS, PRELOAD_ALLOWLIST, SHELL_CHANNELS, type BridgeChannel, type PreloadChannel } from "../preload/bridge.js";
import { navigationAllowed } from "./security.js";
import type { ManagedWindow, WindowLookup, WindowType } from "./windows.js";

/** What main reads from an Electron `IpcMainInvokeEvent`. */
export interface SenderInfo {
  readonly webContentsId: number | null;
  readonly frameUrl: string | null;
  /** Whether the sending frame is the page's main frame. */
  readonly isMainFrame: boolean;
}

/** Read the sender out of an event without trusting its shape (a test double and Electron's event both pass). */
export function senderOf(event: unknown): SenderInfo {
  const record = event !== null && typeof event === "object" ? (event as Record<string, unknown>) : {};
  const sender = record["sender"] !== null && typeof record["sender"] === "object" ? (record["sender"] as Record<string, unknown>) : {};
  const frame = record["senderFrame"] !== null && typeof record["senderFrame"] === "object" ? (record["senderFrame"] as Record<string, unknown>) : null;
  const id = sender["id"];
  const url = frame?.["url"];
  return {
    webContentsId: typeof id === "number" ? id : null,
    frameUrl: typeof url === "string" ? url : null,
    // A subframe has a parent frame; the top-level frame's parent is null (Electron's `WebFrameMain.parent`).
    isMainFrame: frame !== null && (frame["parent"] === null || frame["parent"] === undefined),
  };
}

export type ScopeCheck = (payload: unknown, window: ManagedWindow) => string | null;

export interface ChannelPolicy {
  readonly windows: readonly WindowType[];
  /** For a pop-out: why this call is outside its thread, or `null`. */
  readonly popout?: ScopeCheck;
}

function record(payload: unknown): Record<string, unknown> {
  return payload !== null && typeof payload === "object" && !Array.isArray(payload) ? (payload as Record<string, unknown>) : {};
}

const own =
  (field: string): ScopeCheck =>
  (payload, window) =>
    record(payload)[field] === window.threadId ? null : `a pop-out acts only on its own thread (${field})`;

const known =
  (field: string, set: "questions" | "runs"): ScopeCheck =>
  (payload, window) => {
    const id = record(payload)[field];
    return typeof id === "string" && window.known[set].has(id) ? null : `a pop-out acts only on ${set === "questions" ? "questions" : "runs"} its own thread returned`;
  };

/** The bridge channels a pop-out may use, each with the rule that keeps it on its own thread. */
const POPOUT: Partial<Record<BridgeChannel, ScopeCheck>> = {
  "session.state": () => null,
  "session.me": () => null,
  "session.activeRun": () => null,
  "stream.subscribe": () => null,
  "appearance.set": () => null,
  "update.status": () => null,
  "threads.messages": own("threadId"),
  "threads.view": own("threadId"),
  "questions.list": (payload, window) => (record(payload)["runId"] === undefined ? own("threadId")(payload, window) : "a pop-out lists questions by its own thread"),
  "questions.answer": known("questionId", "questions"),
  "turns.cancel": known("runId", "runs"),
  "runs.timeline": known("runId", "runs"),
  "messages.post": own("threadId"),
  "artifacts.upload": own("threadId"),
  "teammates.list": () => null,
};

/** What a quick-entry window may read about the person; it posts through `shell.quickEntry.submit` only. */
const QUICK_ENTRY: readonly BridgeChannel[] = ["session.state", "session.me", "appearance.set", "teammates.list"];

const ALL_TYPES: readonly WindowType[] = ["main", "popout", "quick_entry", "indicator"];

const SHELL_POLICY: Readonly<Record<(typeof SHELL_CHANNELS)[number], ChannelPolicy>> = {
  "shell.window.context": { windows: ALL_TYPES },
  "shell.pending.consume": { windows: ALL_TYPES },
  "shell.connectivity.report": { windows: ["main"] },
  "shell.shortcuts.get": { windows: ["main"] },
  "shell.shortcuts.set": { windows: ["main"] },
  "shell.shortcuts.reset": { windows: ["main"] },
  "shell.recording.begin": { windows: ["main"] },
  "shell.recording.end": { windows: ["main"] },
  "shell.preferences.get": { windows: ["main"] },
  "shell.preferences.set": { windows: ["main"] },
  "shell.popout.open": { windows: ["main"] },
  "shell.popout.pin": { windows: ["popout"] },
  "shell.window.close": { windows: ["popout", "quick_entry"] },
  "shell.quickEntry.submit": { windows: ["quick_entry"] },
  "shell.activity.list": { windows: ["main", "indicator"] },
  "shell.activity.dismiss": { windows: ["main", "indicator"] },
  "shell.activity.open": { windows: ["main", "indicator"] },
  "shell.indicator.interactive": { windows: ["indicator"] },
  "shell.indicator.moveTo": { windows: ["indicator"] },
  "update.install": { windows: ["main"] },
};

/**
 * The local-file channels (ADP-005, DOMAIN.md §13.7). The grant channels are the composer's attach control, so a
 * pop-out may use them for its own thread only (a token is also bound to the window that holds it); folders, entries,
 * media for entries, edits and watches are `main` only. A quick-entry window and the indicator reach none of them.
 */
const FILE_POLICY: Readonly<Record<(typeof FILE_CHANNELS)[number], ChannelPolicy>> = {
  "files.pick": { windows: ["main", "popout"], popout: own("threadId") },
  "files.grant.attach": { windows: ["main", "popout"], popout: own("threadId") },
  "files.grant.discard": { windows: ["main", "popout"], popout: () => null },
  "files.grant.preview": { windows: ["main", "popout"], popout: () => null },
  "files.root.pick": { windows: ["main"] },
  "files.root.list": { windows: ["main"] },
  "files.root.release": { windows: ["main"] },
  "files.root.entries": { windows: ["main"] },
  "files.media.url": { windows: ["main"] },
  "files.edit.open": { windows: ["main"] },
  "files.edit.save": { windows: ["main"] },
  "files.edit.close": { windows: ["main"] },
  "files.watch.subscribe": { windows: ["main"] },
  "files.watch.unsubscribe": { windows: ["main"] },
};

function bridgePolicy(channel: BridgeChannel): ChannelPolicy {
  const popout = POPOUT[channel];
  const windows: WindowType[] = ["main"];
  if (popout) {
    windows.push("popout");
  }
  if (QUICK_ENTRY.includes(channel)) {
    windows.push("quick_entry");
  }
  return popout ? { windows, popout } : { windows };
}

/** The policy of every allowlisted channel. A bridge channel nobody lists above is main-only. */
export const POLICY: Readonly<Record<PreloadChannel, ChannelPolicy>> = Object.freeze(
  Object.fromEntries([...BRIDGE_CHANNELS.map((channel) => [channel, bridgePolicy(channel)] as const), ...Object.entries(SHELL_POLICY), ...Object.entries(FILE_POLICY)]) as Record<PreloadChannel, ChannelPolicy>,
);

export type Refusal =
  | { readonly ok: false; readonly noop: true; readonly reason: string }
  | { readonly ok: false; readonly noop: false; readonly response: RefusedResponse };

/** What a window gets back for a call it may not make. */
export interface RefusedResponse {
  readonly ok: false;
  readonly error: { readonly code: "SCOPE_FORBIDDEN"; readonly message: string; readonly retryable: false; readonly details: null };
}

export type Verdict = { readonly ok: true; readonly window: ManagedWindow } | Refusal;

export class IpcGuard {
  readonly #windows: WindowLookup;
  readonly #log: (line: string) => void;

  constructor(windows: WindowLookup, log: (line: string) => void) {
    this.#windows = windows;
    this.#log = log;
  }

  /** Decide whether `sender` may call `channel` with `payload`. */
  check(channel: PreloadChannel, sender: SenderInfo, payload: unknown): Verdict {
    if (sender.frameUrl === null || !sender.isMainFrame || !navigationAllowed(sender.frameUrl)) {
      return this.#noop(channel, `untrusted sender (${sender.frameUrl === null ? "no frame" : origin(sender.frameUrl)})`);
    }
    const window = sender.webContentsId === null ? null : this.#windows.byWebContents(sender.webContentsId);
    if (window === null || window.closed) {
      return this.#noop(channel, "not a window the shell created");
    }
    const policy = POLICY[channel];
    if (!policy.windows.includes(window.type)) {
      return this.#refuse(channel, window, `a ${window.type.replace("_", "-")} window may not call ${channel}`);
    }
    if (window.type === "popout" && policy.popout) {
      const problem = policy.popout(payload, window);
      if (problem !== null) {
        return this.#refuse(channel, window, problem);
      }
    }
    return { ok: true, window };
  }

  #noop(channel: string, reason: string): Refusal {
    this.#log(`quansio desktop: ignored ${channel}: ${reason}`);
    return { ok: false, noop: true, reason };
  }

  #refuse(channel: string, window: ManagedWindow, message: string): Refusal {
    this.#log(`quansio desktop: refused ${channel} from a ${window.type} window: ${message}`);
    return { ok: false, noop: false, response: { ok: false, error: { code: "SCOPE_FORBIDDEN", message, retryable: false, details: null } } };
  }

  /** Remember the ids a pop-out's own reads returned, the only ones it may later act on. */
  learn(window: ManagedWindow, channel: PreloadChannel, result: unknown): void {
    if (window.type !== "popout") {
      return;
    }
    const body = record(result);
    const list = (key: string): Record<string, unknown>[] => (Array.isArray(body[key]) ? (body[key] as unknown[]).map(record) : []);
    if (channel === "questions.list") {
      for (const question of list("questions")) {
        if (typeof question["id"] === "string") {
          window.known.questions.add(question["id"]);
        }
        if (typeof question["run_id"] === "string") {
          window.known.runs.add(question["run_id"]);
        }
      }
    }
    if (channel === "messages.post" && typeof body["run_id"] === "string") {
      // The run the pop-out's own message started is its own to cancel and inspect.
      window.known.runs.add(body["run_id"]);
    }
    if (channel === "threads.messages") {
      for (const message of list("messages")) {
        if (typeof message["run_id"] === "string") {
          window.known.runs.add(message["run_id"]);
        }
      }
    }
  }
}

function origin(url: string): string {
  try {
    return new URL(url).origin;
  } catch {
    return "unparseable url";
  }
}

/** The slice of `ipcMain` the guard registers on. */
export interface GuardedIpc {
  handle(channel: string, listener: (event: unknown, payload: unknown) => unknown): void;
}

/** A handler that is told which window called it. */
export type WindowHandler = (window: ManagedWindow, payload: unknown) => Promise<unknown>;

/**
 * Install one handler per allowlisted channel, each behind the guard. A channel without a handler, or a handler
 * for a channel the allowlist does not have, is a programming error and throws at start-up.
 */
export function registerGuardedHandlers(ipc: GuardedIpc, guard: IpcGuard, handlers: Readonly<Record<PreloadChannel, WindowHandler>>): readonly string[] {
  const registered: string[] = [];
  for (const channel of PRELOAD_ALLOWLIST) {
    const handler = handlers[channel];
    ipc.handle(channel, (event, payload) => {
      const verdict = guard.check(channel, senderOf(event), payload);
      if (!verdict.ok) {
        return Promise.resolve(verdict.noop ? undefined : verdict.response);
      }
      return handler(verdict.window, payload).then((result) => {
        guard.learn(verdict.window, channel, result);
        return result;
      });
    });
    registered.push(channel);
  }
  return registered;
}
