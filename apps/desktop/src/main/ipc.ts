/**
 * Main-process side of the preload bridge (APP-003).
 *
 * Exactly the channels in `PRELOAD_ALLOWLIST` get a handler. Each handler validates the
 * renderer's payload as untrusted data, performs the operation through the typed clients
 * and the durable session, and returns a projection that carries no scope headers, tokens
 * or cursors the renderer could replay on its own. Frames from the stream are pushed to the
 * renderer over `PRELOAD_PUSH_CHANNELS`; main is the only process that opens a socket.
 */

import { LocalRuntimeClient } from "../client/local-runtime.js";
import { ServerClient, type CommandResponse, type FetchLike, type SessionScope } from "../client/server.js";
import { StreamClient, type EventFrame, type LiveFrame, type SocketFactory, type StreamErrorFrame, type StreamState } from "../client/stream.js";
import { PRELOAD_ALLOWLIST, PRELOAD_PUSH_CHANNELS, type BridgeCommand, type PreloadChannel, type PushChannel } from "../preload/bridge.js";
import { parseDeepLink, type DeepLinkIntent } from "./deeplinks.js";
import { SessionFile, type ActiveRunSession, type DesktopSession } from "./session-file.js";
import type { UpdateHooks, UpdateStatus } from "./updates.js";

/** The slice of `ipcMain` the bridge registers on. */
export interface IpcMainLike {
  handle(channel: string, listener: (event: unknown, payload: unknown) => unknown): void;
}

/** How main pushes to the renderer (`webContents.send`). */
export type PushSender = (channel: PushChannel, payload: unknown) => void;

/** What the renderer may learn about the session: identities and progress, never scope. */
export interface ActiveRunProjection {
  readonly connected: boolean;
  readonly workspaceId: string | null;
  readonly activeRun: ActiveRunSession | null;
  readonly streamState: StreamState;
}

export interface DesktopBridgeOptions {
  readonly serverOrigin: string;
  readonly scope: SessionScope;
  readonly workspaceId: string;
  readonly session: SessionFile;
  readonly openSocket: SocketFactory;
  readonly updates: UpdateHooks;
  readonly consumer: string;
  readonly push: PushSender;
  /** Injectable for tests; defaults to the global `fetch`. */
  readonly fetch?: FetchLike;
}

export class DesktopBridge {
  readonly server: ServerClient;
  readonly localRuntime: LocalRuntimeClient;
  readonly #options: DesktopBridgeOptions;
  #stream: StreamClient | null = null;
  #streamState: StreamState = "closed";

  constructor(options: DesktopBridgeOptions) {
    this.#options = options;
    this.server = new ServerClient(
      options.fetch
        ? { origin: options.serverOrigin, scope: options.scope, fetch: options.fetch }
        : { origin: options.serverOrigin, scope: options.scope },
    );
    this.localRuntime = new LocalRuntimeClient(this.server);
    const current = options.session.current();
    if (!current || current.serverOrigin !== this.server.origin || current.workspaceId !== options.workspaceId) {
      options.session.begin(this.server.origin, options.workspaceId);
    }
  }

  /** The handlers, keyed by allowlisted channel; `registerBridgeHandlers` installs them. */
  handlers(): Record<PreloadChannel, (payload: unknown) => Promise<unknown>> {
    return {
      // Every handler is async so a refused payload is a rejection the renderer sees as an
      // error result, never an exception escaping into the IPC layer.
      "commands.invoke": (payload) => Promise.resolve(payload).then((raw) => this.invoke(asBridgeCommand(raw))),
      "stream.subscribe": () => Promise.resolve().then(() => this.subscribe()),
      "stream.reconnect": () => Promise.resolve().then(() => this.reconnect()),
      "session.activeRun": () => Promise.resolve().then(() => this.activeRun()),
      "deepLink.open": (payload) => Promise.resolve(payload).then((raw) => this.openDeepLink(asUrl(raw))),
      "update.status": () => Promise.resolve().then(() => this.updateStatus()),
    };
  }

  async invoke(command: BridgeCommand): Promise<CommandResponse> {
    const response = await this.server.invoke(command.name, command.commandId, command.params);
    const result = response.result;
    if (command.name === "PostMessage" && result && typeof result === "object") {
      const { run_id: runId, thread_id: threadId } = result as { run_id?: unknown; thread_id?: unknown };
      if (typeof runId === "string" && typeof threadId === "string") {
        const session = this.#options.session.current();
        this.#options.session.attach({
          runId,
          threadId,
          generation: 1,
          streamCursor: session?.streamCursor ?? "",
          workspaceId: this.#options.workspaceId,
        });
      }
    }
    return response;
  }

  /** Subscribe from the durable cursor; the renderer's own idea of a cursor is never used. */
  subscribe(): ActiveRunProjection {
    if (this.#stream && this.#stream.state !== "closed") {
      return this.activeRun();
    }
    this.#openStream();
    return this.activeRun();
  }

  /** Reconnect: close the transport and resume the subscription from the durable cursor. */
  reconnect(): ActiveRunProjection {
    this.#stream?.close();
    this.#stream = null;
    this.#openStream();
    return this.activeRun();
  }

  activeRun(): ActiveRunProjection {
    const session = this.#options.session.current();
    return {
      connected: this.#streamState === "open",
      workspaceId: session?.workspaceId ?? null,
      activeRun: session?.activeRun ?? null,
      streamState: this.#streamState,
    };
  }

  /** Parse a deep link and, when valid, tell the renderer where to go. */
  openDeepLink(url: string): DeepLinkIntent | null {
    const intent = parseDeepLink(url);
    if (intent) {
      this.#options.push("deepLink.intent", intent);
    }
    return intent;
  }

  updateStatus(): UpdateStatus {
    return this.#options.updates.status();
  }

  /** The durable session, for tests and diagnostics; never sent to the renderer as-is. */
  session(): DesktopSession | null {
    return this.#options.session.current();
  }

  /** The stream cursor main will resume from. */
  cursor(): string | null {
    return this.#stream?.cursor ?? this.#options.session.current()?.streamCursor ?? null;
  }

  close(): void {
    this.#stream?.close();
    this.#stream = null;
  }

  /**
   * The desktop's one durable subscription. A cursor is bound to the channel set it was
   * issued under, so the set must not change across runs: the workspace channel carries
   * every run's durable events, and run-scoped subscriptions are for narrower consumers.
   */
  #channels(): string[] {
    return [`workspace:${this.#options.workspaceId}`];
  }

  #openStream(cursor: string | null = this.#options.session.current()?.streamCursor ?? null): void {
    const headers: Record<string, string> = {
      "x-quansio-tenant": this.#options.scope.tenantId,
      "x-quansio-user": this.#options.scope.userId,
      "x-quansio-consumer": this.#options.consumer,
    };
    // Callbacks check that they belong to the current client: a superseded client's late
    // close must not report on the subscription that replaced it.
    let client: StreamClient | null = null;
    const current = (): boolean => client !== null && client === this.#stream;
    client = new StreamClient({
      url: this.server.origin.replace(/^http/, "ws") + "/v1/stream",
      headers,
      channels: this.#channels(),
      consumer: this.#options.consumer,
      openSocket: this.#options.openSocket,
      cursor,
      onEvent: (frame) => {
        if (current()) {
          this.#onEvent(frame);
        }
      },
      onLive: (frame) => {
        if (current()) {
          this.#onLive(frame);
        }
      },
      onError: (error) => {
        if (current()) {
          this.#onError(error);
        }
      },
      onState: (state) => {
        if (current()) {
          this.#streamState = state;
          this.#options.push("stream.state", state);
        }
      },
    });
    this.#stream = client;
    client.connect();
  }

  #onError(error: StreamErrorFrame): void {
    this.#options.push("stream.error", error);
    if (!error.retryable) {
      // The server refused the subscription itself, so the durable cursor is no longer a
      // position it will honour. Resume from its current position: continuity is lost only
      // for the interval the server itself declined to replay, and that is reported above.
      this.#stream?.close();
      this.#stream = null;
      this.#options.session.resetCursor();
      this.#openStream(null);
    }
  }

  #onEvent(frame: EventFrame): void {
    this.#options.session.advance(frame.cursor);
    this.#options.push("stream.frame", frame);
  }

  #onLive(frame: LiveFrame): void {
    // Live frames (model deltas, terminal bytes, browser frames, progress) reach the renderer
    // only here; they are never persisted and never move the cursor.
    this.#options.push("stream.frame", frame);
  }
}

/** Install one handler per allowlisted channel and nothing else. */
export function registerBridgeHandlers(ipc: IpcMainLike, bridge: DesktopBridge): readonly string[] {
  const handlers = bridge.handlers();
  const registered: string[] = [];
  for (const channel of PRELOAD_ALLOWLIST) {
    ipc.handle(channel, (_event, payload) => handlers[channel](payload));
    registered.push(channel);
  }
  return registered;
}

export { PRELOAD_PUSH_CHANNELS };

function asBridgeCommand(payload: unknown): BridgeCommand {
  if (!payload || typeof payload !== "object") {
    throw new Error("commands.invoke needs {name, commandId, params}");
  }
  const record = payload as Record<string, unknown>;
  const name = record["name"];
  const commandId = record["commandId"];
  const params = record["params"];
  if (typeof name !== "string" || typeof commandId !== "string" || !/^cmd_[0-9A-HJKMNP-TV-Z]{26}$/.test(commandId)) {
    throw new Error("commands.invoke needs a command name and a cmd_ ULID");
  }
  if (params !== undefined && (params === null || typeof params !== "object" || Array.isArray(params))) {
    throw new Error("commands.invoke params must be an object");
  }
  return { name, commandId, params: (params as Record<string, unknown> | undefined) ?? {} };
}

function asUrl(payload: unknown): string {
  if (!payload || typeof payload !== "object" || typeof (payload as { url?: unknown }).url !== "string") {
    throw new Error("deepLink.open needs {url}");
  }
  return (payload as { url: string }).url;
}
