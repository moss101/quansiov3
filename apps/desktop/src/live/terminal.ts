/**
 * Terminal replay from the durable cursor (APP-007, DOMAIN.md §8.5, §9.3).
 *
 * A terminal session is a byte stream numbered absolutely; the server row keeps the
 * durable cursor (EXEC-006) and `terminal.bytes` live frames carry `{session_id, from,
 * bytes}` — the absolute offset the chunk starts at and its base64 bytes. The buffer here
 * is the client's half of the replay contract:
 *
 * * a chunk is accepted only in order: one that starts at or before the offset already
 *   consumed has its overlap trimmed (a re-sent chunk after a reconnect renders nothing
 *   twice); one that starts beyond it is a gap, reported and not rendered, because bytes
 *   cannot be invented between them;
 * * a chunk for another session is refused;
 * * on reconnect the replay position is the later of what this client consumed and the
 *   durable cursor: the same session, the same offset, and `lost` says how many bytes the
 *   conduit will not re-send because the durable cursor moved past them elsewhere.
 */

export interface TerminalBytesFrame {
  readonly kind: "terminal.bytes";
  readonly session_id: string;
  readonly from: number;
  readonly bytes: string;
}

export type Accepted =
  | { readonly kind: "appended"; readonly bytes: number; readonly trimmed: number }
  | { readonly kind: "duplicate" }
  | { readonly kind: "gap"; readonly expected: number; readonly received: number }
  | { readonly kind: "other-session" };

export interface ReplayPosition {
  readonly from: number;
  readonly lost: number;
}

/** Decode a live frame as a terminal chunk, or `null` when it is not one. */
export function decodeTerminalBytes(frame: unknown): TerminalBytesFrame | null {
  if (!frame || typeof frame !== "object") {
    return null;
  }
  const record = frame as Record<string, unknown>;
  if (record["kind"] !== "terminal.bytes" || typeof record["session_id"] !== "string" || typeof record["bytes"] !== "string") {
    return null;
  }
  const from = typeof record["from"] === "number" ? record["from"] : typeof record["from"] === "string" ? Number(record["from"]) : Number.NaN;
  if (!Number.isInteger(from) || from < 0) {
    return null;
  }
  return { kind: "terminal.bytes", session_id: record["session_id"], from, bytes: record["bytes"] };
}

/** Where to resume after a reconnect: the later of the local offset and the durable cursor. */
export function replayPosition(local: number, durableCursor: number): ReplayPosition {
  if (durableCursor > local) {
    return { from: durableCursor, lost: durableCursor - local };
  }
  return { from: local, lost: 0 };
}

function decodeBase64(text: string): Uint8Array {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

export class TerminalReplay {
  readonly sessionId: string;
  #cursor: number;
  #chunks: Uint8Array[] = [];
  #gaps = 0;

  /** Start at the durable cursor: nothing before it will be sent, so nothing before it is shown. */
  constructor(sessionId: string, durableCursor: number) {
    if (!sessionId.startsWith("tsn_")) {
      throw new Error("a terminal replay names a tsn_ session");
    }
    if (!Number.isInteger(durableCursor) || durableCursor < 0) {
      throw new Error("the durable cursor is a byte offset");
    }
    this.sessionId = sessionId;
    this.#cursor = durableCursor;
  }

  /** The absolute offset consumed so far. */
  get cursor(): number {
    return this.#cursor;
  }

  /** How many chunks were refused for starting beyond the cursor. */
  get gaps(): number {
    return this.#gaps;
  }

  accept(frame: TerminalBytesFrame): Accepted {
    if (frame.session_id !== this.sessionId) {
      return { kind: "other-session" };
    }
    const bytes = decodeBase64(frame.bytes);
    const end = frame.from + bytes.length;
    if (end <= this.#cursor) {
      return { kind: "duplicate" };
    }
    if (frame.from > this.#cursor) {
      this.#gaps += 1;
      return { kind: "gap", expected: this.#cursor, received: frame.from };
    }
    const trimmed = this.#cursor - frame.from;
    const fresh = trimmed > 0 ? bytes.subarray(trimmed) : bytes;
    this.#chunks.push(fresh);
    this.#cursor = end;
    return { kind: "appended", bytes: fresh.length, trimmed };
  }

  /**
   * Resume after a reconnect from the durable cursor the projection reports. The local
   * offset is kept when it is ahead (the durable cursor only moves forward, so a re-sent
   * chunk is trimmed by `accept`); when the durable cursor is ahead, the replay moves to
   * it and the bytes in between are reported lost rather than shown as a gap forever.
   */
  resume(durableCursor: number): ReplayPosition {
    const position = replayPosition(this.#cursor, durableCursor);
    this.#cursor = position.from;
    return position;
  }

  /** The bytes rendered so far, as UTF-8 text with invalid sequences replaced. */
  text(): string {
    const total = this.#chunks.reduce((sum, chunk) => sum + chunk.length, 0);
    const joined = new Uint8Array(total);
    let offset = 0;
    for (const chunk of this.#chunks) {
      joined.set(chunk, offset);
      offset += chunk.length;
    }
    return new TextDecoder("utf-8", { fatal: false }).decode(joined);
  }

  /** How many bytes this client has rendered (not the offset: rendering started at the cursor). */
  rendered(): number {
    return this.#chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  }
}
