/**
 * Browser and computer live frames (APP-007, DOMAIN.md §8.4, §9.3).
 *
 * `browser.frame` is a transient screencast frame on the `target:` channel: the session it
 * belongs to, the encoded image, the page it shows and — when the agent is driving — where
 * the agent's cursor is and what it is doing. Frames are never persisted and may be absent
 * (§9.3); the page renders the latest one it has and the durable session state beside it.
 * A frame for another session is refused, so a second session cannot be shown as this one.
 */

export type CursorAction = "idle" | "move" | "click" | "type" | "scroll";

export interface AgentCursor {
  readonly x: number;
  readonly y: number;
  readonly action: CursorAction;
}

export interface BrowserFrame {
  readonly sessionId: string;
  /** The tab the picture shows, when the worker said (a person's input names a tab). */
  readonly tabId: string | null;
  /** The page's viewport in CSS pixels, which a click's coordinates are in; `null` when unknown. */
  readonly width: number | null;
  readonly height: number | null;
  /** `data:<media type>;base64,…`, ready for an `<img>`. */
  readonly dataUrl: string;
  readonly url: string | null;
  readonly cursor: AgentCursor | null;
  readonly sequence: number | null;
}

const MEDIA_TYPES = new Set(["image/jpeg", "image/png", "image/webp"]);

function cursorOf(value: unknown): AgentCursor | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const record = value as Record<string, unknown>;
  const x = record["x"];
  const y = record["y"];
  if (typeof x !== "number" || typeof y !== "number" || !Number.isFinite(x) || !Number.isFinite(y)) {
    return null;
  }
  const action = record["action"];
  return {
    x,
    y,
    action: action === "move" || action === "click" || action === "type" || action === "scroll" ? action : "idle",
  };
}

/** Decode a live frame as a screencast frame of `sessionId`, or `null` when it is not one. */
export function decodeBrowserFrame(frame: unknown, sessionId: string): BrowserFrame | null {
  if (!frame || typeof frame !== "object") {
    return null;
  }
  const record = frame as Record<string, unknown>;
  if (record["kind"] !== "browser.frame" || record["session_id"] !== sessionId) {
    return null;
  }
  const data = record["data"];
  if (typeof data !== "string" || data.length === 0 || !/^[A-Za-z0-9+/=]+$/.test(data)) {
    return null;
  }
  const mediaType = typeof record["media_type"] === "string" && MEDIA_TYPES.has(record["media_type"]) ? record["media_type"] : "image/jpeg";
  const sequence = typeof record["sequence"] === "number" && Number.isInteger(record["sequence"]) ? record["sequence"] : null;
  const dimension = (value: unknown): number | null =>
    typeof value === "number" && Number.isFinite(value) && value > 0 && value <= 100000 ? value : null;
  return {
    sessionId,
    tabId: typeof record["tab_id"] === "string" && record["tab_id"].length > 0 ? record["tab_id"] : null,
    width: dimension(record["width"]),
    height: dimension(record["height"]),
    dataUrl: `data:${mediaType};base64,${data}`,
    url: typeof record["url"] === "string" ? record["url"] : null,
    cursor: cursorOf(record["cursor"]),
    sequence,
  };
}

/** Keep the latest frame: an older sequence never replaces a newer one after a reorder. */
export function latestFrame(current: BrowserFrame | null, incoming: BrowserFrame): BrowserFrame {
  if (current && current.sequence !== null && incoming.sequence !== null && incoming.sequence < current.sequence) {
    return current;
  }
  return incoming;
}

/** Where a click on the displayed picture lands in the page, in the page's own CSS pixels. */
export interface PagePoint {
  readonly x: number;
  readonly y: number;
}

/** The rectangle the picture is displayed in, in the same coordinates as the click (`getBoundingClientRect`). */
export interface DisplayedRect {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

/**
 * Map a click on the displayed picture to the page's coordinates.
 *
 * The picture is a scaled copy of the page's viewport, so the click's fraction of the displayed rectangle is its fraction
 * of the viewport. `null` when the frame does not say how big the page is, or the click is outside the picture: a click
 * that cannot be placed is not sent.
 */
export function frameToPage(frame: BrowserFrame, rect: DisplayedRect, clientX: number, clientY: number): PagePoint | null {
  if (frame.width === null || frame.height === null || rect.width <= 0 || rect.height <= 0) {
    return null;
  }
  const fractionX = (clientX - rect.left) / rect.width;
  const fractionY = (clientY - rect.top) / rect.height;
  if (!(fractionX >= 0 && fractionX <= 1 && fractionY >= 0 && fractionY <= 1)) {
    return null;
  }
  return { x: Math.round(fractionX * frame.width * 100) / 100, y: Math.round(fractionY * frame.height * 100) / 100 };
}
