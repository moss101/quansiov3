/**
 * Secure preload bridge (APP-003).
 *
 * The renderer may call only these names. There is no `fs`, `child_process`, `net` or
 * `eval` channel. Main implements the other side against the Rust server (`main/ipc.ts`).
 * `preload/index.ts` is the script Electron loads; it exposes exactly `createPreloadApi`
 * on `window.quansio` through `contextBridge`.
 */

/** Channels the renderer may invoke (renderer → main, request/response). */
export const PRELOAD_ALLOWLIST = [
  "commands.invoke",
  "stream.subscribe",
  "stream.reconnect",
  "session.activeRun",
  "deepLink.open",
  "update.status",
] as const;

export type PreloadChannel = (typeof PRELOAD_ALLOWLIST)[number];

/** Channels main may push on (main → renderer, fire and forget). */
export const PRELOAD_PUSH_CHANNELS = [
  "stream.frame",
  "stream.state",
  "stream.error",
  "deepLink.intent",
  "update.status",
] as const;

export type PushChannel = (typeof PRELOAD_PUSH_CHANNELS)[number];

/** A command envelope sent through the bridge to the server. */
export interface BridgeCommand {
  readonly name: string;
  readonly commandId: string;
  readonly params: Record<string, unknown>;
}

/** Result of asking whether a renderer request is allowed. */
export function preloadAllows(channel: string): boolean {
  return (PRELOAD_ALLOWLIST as readonly string[]).includes(channel);
}

/** True when main may push on `channel`. */
export function pushAllows(channel: string): boolean {
  return (PRELOAD_PUSH_CHANNELS as readonly string[]).includes(channel);
}

/** Host APIs the renderer must never receive. */
export const FORBIDDEN_RENDERER_APIS = [
  "fs",
  "child_process",
  "net",
  "os",
  "process.binding",
  "eval",
  "Function",
] as const;

/** True when a candidate renderer API name is forbidden. */
export function isForbiddenRendererApi(name: string): boolean {
  return (FORBIDDEN_RENDERER_APIS as readonly string[]).includes(name);
}

export type BridgeSend = (channel: PreloadChannel, payload: unknown) => unknown;
export type BridgeSubscribe = (channel: PushChannel, listener: (payload: unknown) => void) => () => void;

/**
 * The object exposed on `window.quansio`. Invoke keys map one-to-one onto the allowlist
 * and subscription keys onto the push channels; `send`/`subscribe` are the IPC wrappers
 * supplied by the preload script. Tests freeze this shape.
 */
export function createPreloadApi(send: BridgeSend, subscribe: BridgeSubscribe = () => () => undefined) {
  return {
    invokeCommand: (command: BridgeCommand) => send("commands.invoke", command),
    subscribeStream: (cursor?: string) => send("stream.subscribe", { cursor }),
    reconnectStream: (cursor: string) => send("stream.reconnect", { cursor }),
    activeRun: () => send("session.activeRun", {}),
    openDeepLink: (url: string) => send("deepLink.open", { url }),
    updateStatus: () => send("update.status", {}),
    onStreamFrame: (listener: (payload: unknown) => void) => subscribe("stream.frame", listener),
    onStreamState: (listener: (payload: unknown) => void) => subscribe("stream.state", listener),
    onStreamError: (listener: (payload: unknown) => void) => subscribe("stream.error", listener),
    onDeepLink: (listener: (payload: unknown) => void) => subscribe("deepLink.intent", listener),
    onUpdateStatus: (listener: (payload: unknown) => void) => subscribe("update.status", listener),
  };
}

export type PreloadApi = ReturnType<typeof createPreloadApi>;
