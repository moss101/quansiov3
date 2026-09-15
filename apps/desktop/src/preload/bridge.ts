/**
 * Secure preload bridge (APP-003).
 *
 * The renderer may call only these names. There is no `fs`, `child_process`, `net` or
 * `eval` channel. Main implements the other side against the Rust server.
 */

/** Channels the preload may expose. */
export const PRELOAD_ALLOWLIST = [
  "commands.invoke",
  "stream.subscribe",
  "stream.reconnect",
  "session.activeRun",
  "deepLink.open",
  "update.status",
] as const;

export type PreloadChannel = (typeof PRELOAD_ALLOWLIST)[number];

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

/**
 * The object exposed on `window.quansio`. Keys are the allowlist; values are IPC
 * wrappers supplied by main. Tests freeze this shape.
 */
export function createPreloadApi(send: (channel: PreloadChannel, payload: unknown) => unknown) {
  return {
    invokeCommand: (command: BridgeCommand) => send("commands.invoke", command),
    subscribeStream: (cursor?: string) => send("stream.subscribe", { cursor }),
    reconnectStream: (cursor: string) => send("stream.reconnect", { cursor }),
    activeRun: () => send("session.activeRun", {}),
    openDeepLink: (url: string) => send("deepLink.open", { url }),
    updateStatus: () => send("update.status", {}),
  };
}
