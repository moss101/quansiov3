/**
 * Client stream (DOMAIN.md §9.3) for the desktop main process (APP-003).
 *
 * `GET /v1/stream` over WebSocket. The client sends one subscribe request, then receives
 * durable `event` frames (each carrying the cursor to resume from) and transient `live`
 * frames (`model.delta`, `terminal.bytes`, `browser.frame`, `progress`) that are never
 * persisted and may be absent. Reconnecting with the last cursor replays missed events in
 * order; a slow consumer is told `STREAM_BACKPRESSURE` and must resubscribe.
 *
 * The renderer receives model deltas, terminal bytes and browser frames only through this
 * client (task build item 3): main forwards frames over the preload bridge, and nothing else
 * in the desktop opens a socket.
 */

export type StreamChannel = `workspace:${string}` | `thread:${string}` | `run:${string}` | `target:${string}`;

export interface EventFrame {
  readonly kind: "event";
  readonly cursor: string;
  readonly event: Record<string, unknown>;
}

export type LiveFrameKind = "model.delta" | "terminal.bytes" | "browser.frame" | "progress";

export interface LiveFrame {
  readonly kind: "live";
  readonly channel: string;
  readonly frame: { readonly kind: string; readonly [key: string]: unknown };
}

export interface StreamErrorFrame {
  readonly code: string;
  readonly message: string;
  readonly correlation_id: string;
  readonly retryable: boolean;
}

export type StreamFrame = EventFrame | LiveFrame;

/** Parse one text frame; anything that is not a §9.3 frame or a §15 error is refused. */
export function parseStreamFrame(text: string): StreamFrame | StreamErrorFrame {
  const decoded: unknown = JSON.parse(text);
  if (!decoded || typeof decoded !== "object") {
    throw new Error("stream frame is not an object");
  }
  const frame = decoded as Record<string, unknown>;
  if (frame["kind"] === "event" && typeof frame["cursor"] === "string" && frame["event"] && typeof frame["event"] === "object") {
    return { kind: "event", cursor: frame["cursor"], event: frame["event"] as Record<string, unknown> };
  }
  if (frame["kind"] === "live" && typeof frame["channel"] === "string" && frame["frame"] && typeof frame["frame"] === "object") {
    return { kind: "live", channel: frame["channel"], frame: frame["frame"] as LiveFrame["frame"] };
  }
  if (typeof frame["code"] === "string" && typeof frame["message"] === "string") {
    return {
      code: frame["code"],
      message: frame["message"],
      correlation_id: typeof frame["correlation_id"] === "string" ? frame["correlation_id"] : "",
      retryable: Boolean(frame["retryable"]),
    };
  }
  throw new Error(`unrecognized stream frame: ${text.slice(0, 120)}`);
}

export function isErrorFrame(frame: StreamFrame | StreamErrorFrame): frame is StreamErrorFrame {
  return "code" in frame;
}

/** Bounded exponential backoff: 250ms doubling to 8s, never beyond `maxAttempts`. */
export function reconnectDelayMs(attempt: number): number {
  if (attempt < 1) {
    throw new Error("attempt is 1-based");
  }
  return Math.min(250 * 2 ** (attempt - 1), 8_000);
}

/** The subscribe request the server expects; the cursor resumes a previous subscription. */
export function subscribeRequest(
  channels: readonly string[],
  cursor: string | null,
  consumer: string,
): { subscribe: { channel: string }[]; cursor?: string; consumer: string } {
  if (channels.length === 0) {
    throw new Error("subscribe needs at least one channel");
  }
  for (const channel of channels) {
    if (!/^(workspace|thread|run|target):[a-z]+_[0-9A-Za-z]+$/.test(channel)) {
      throw new Error(`invalid stream channel: ${channel}`);
    }
  }
  const request: { subscribe: { channel: string }[]; cursor?: string; consumer: string } = {
    subscribe: channels.map((channel) => ({ channel })),
    consumer,
  };
  if (cursor) {
    request.cursor = cursor;
  }
  return request;
}

/** The subset of a WebSocket the client drives; `ws` and the DOM socket both satisfy it. */
export interface SocketLike {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  readonly readyState: number;
  addEventListener(type: "open", listener: () => void): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "close", listener: (event: { code: number; reason: string }) => void): void;
  addEventListener(type: "error", listener: (event: unknown) => void): void;
}

export type SocketFactory = (url: string, headers: Record<string, string>) => SocketLike;

export interface StreamClientOptions {
  readonly url: string;
  /** Upgrade headers, or a function evaluated at each connect (the access token rotates). */
  readonly headers: Record<string, string> | (() => Record<string, string>);
  readonly channels: readonly string[];
  readonly consumer: string;
  readonly openSocket: SocketFactory;
  /** Cursor to resume from on the first connect (from the durable session). */
  readonly cursor?: string | null;
  readonly maxAttempts?: number;
  readonly onEvent?: (frame: EventFrame) => void;
  readonly onLive?: (frame: LiveFrame) => void;
  readonly onError?: (error: StreamErrorFrame) => void;
  readonly onState?: (state: StreamState) => void;
  /** Injectable timer for tests. */
  readonly setTimer?: (callback: () => void, delayMs: number) => unknown;
}

export type StreamState = "connecting" | "open" | "reconnecting" | "closed";

/**
 * Keeps one subscription alive across socket drops, resuming from the last durable cursor.
 * The cursor is the only state that matters for correctness; everything else is rebuilt.
 */
export class StreamClient {
  #cursor: string | null;
  #socket: SocketLike | null = null;
  #attempt = 0;
  #closed = false;
  #state: StreamState = "closed";
  readonly #options: StreamClientOptions;

  constructor(options: StreamClientOptions) {
    this.#options = options;
    this.#cursor = options.cursor ?? null;
  }

  /** The cursor of the last durable event, or the one the client was resumed with. */
  get cursor(): string | null {
    return this.#cursor;
  }

  get state(): StreamState {
    return this.#state;
  }

  /** Open the socket and subscribe; resolves once the subscribe request has been sent. */
  connect(): void {
    this.#closed = false;
    this.#attempt += 1;
    this.#setState(this.#attempt === 1 ? "connecting" : "reconnecting");
    const headers = typeof this.#options.headers === "function" ? this.#options.headers() : this.#options.headers;
    const socket = this.#options.openSocket(this.#options.url, headers);
    this.#socket = socket;
    socket.addEventListener("open", () => {
      socket.send(JSON.stringify(subscribeRequest(this.#options.channels, this.#cursor, this.#options.consumer)));
      this.#attempt = 0;
      this.#setState("open");
    });
    socket.addEventListener("message", (event) => {
      this.#onMessage(typeof event.data === "string" ? event.data : String(event.data));
    });
    socket.addEventListener("error", () => {
      /* the close event that follows carries the retry decision */
    });
    socket.addEventListener("close", () => {
      this.#socket = null;
      if (this.#closed) {
        this.#setState("closed");
        return;
      }
      this.#scheduleReconnect();
    });
  }

  /** Close for good; no reconnect is scheduled. */
  close(): void {
    this.#closed = true;
    const socket = this.#socket;
    this.#socket = null;
    socket?.close(1000, "client closed");
    this.#setState("closed");
  }

  /** Drop the transport without closing the subscription: the next connect resumes from the cursor. */
  dropForTest(): void {
    this.#socket?.close(4000, "dropped");
  }

  #onMessage(text: string): void {
    let frame: StreamFrame | StreamErrorFrame;
    try {
      frame = parseStreamFrame(text);
    } catch {
      return; // an unparseable frame is dropped; the cursor does not move
    }
    if (isErrorFrame(frame)) {
      if (!frame.retryable) {
        // The server has refused this subscription as such (for example a cursor issued for
        // another channel set): reconnecting with the same request can only repeat the refusal.
        this.#closed = true;
      }
      this.#options.onError?.(frame);
      return;
    }
    if (frame.kind === "event") {
      this.#cursor = frame.cursor;
      this.#options.onEvent?.(frame);
    } else {
      this.#options.onLive?.(frame);
    }
  }

  #scheduleReconnect(): void {
    const maxAttempts = this.#options.maxAttempts ?? 8;
    if (this.#attempt >= maxAttempts) {
      this.#closed = true;
      this.#setState("closed");
      return;
    }
    this.#setState("reconnecting");
    const delay = reconnectDelayMs(this.#attempt + 1);
    const setTimer = this.#options.setTimer ?? ((callback, ms) => setTimeout(callback, ms));
    setTimer(() => {
      if (!this.#closed) {
        this.connect();
      }
    }, delay);
  }

  #setState(state: StreamState): void {
    this.#state = state;
    this.#options.onState?.(state);
  }
}
