/**
 * Desktop trace context and diagnostic frames (OPS-003).
 *
 * The desktop is where a user's action enters Quansio, so it starts the trace: every call the
 * server client makes carries a fresh W3C `traceparent`, the server continues it through the
 * runtime turn into the intelligence plane, and the server echoes its own span back. The client
 * records one redacted frame per call — method, path, status and trace id, never a body — in a
 * bounded ring, which is what the desktop contributes to a diagnostic export alongside the
 * server's tenant-scoped bundle.
 */

import { DEFAULT_BUNDLE_LIMIT, redactText } from "./bundle.js";

export const TRACEPARENT = "traceparent";
export const FRAME_RING_CAPACITY = 200;

function randomHex(bytes: number): string {
  const buffer = new Uint8Array(bytes);
  globalThis.crypto.getRandomValues(buffer);
  if (buffer.every((byte) => byte === 0)) {
    buffer[0] = 1;
  }
  return Array.from(buffer, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** A new trace's root span, as a W3C `traceparent` header value. */
export function newTraceparent(): string {
  return `00-${randomHex(16)}-${randomHex(8)}-01`;
}

/** The trace id of a valid `traceparent`, or null. */
export function traceIdOf(traceparent: string | null | undefined): string | null {
  const match = /^00-([0-9a-f]{32})-([0-9a-f]{16})-[0-9a-f]{2}$/.exec(traceparent?.trim() ?? "");
  if (!match || /^0+$/.test(match[1] ?? "") || /^0+$/.test(match[2] ?? "")) {
    return null;
  }
  return match[1] ?? null;
}

/** One call the desktop made, as it may appear in a diagnostic export. */
export interface DesktopFrame {
  readonly at: string;
  readonly traceId: string;
  readonly service: "desktop";
  readonly msg: string;
}

/** The bounded ring of recent desktop frames. */
export class FrameRing {
  readonly #frames: DesktopFrame[] = [];
  readonly #capacity: number;

  constructor(capacity = FRAME_RING_CAPACITY) {
    this.#capacity = Math.max(1, capacity);
  }

  /** Record a call: `GET /v1/me -> 200`; the path loses its query string, the text is redacted. */
  record(traceId: string, method: string, url: string, status: number): void {
    let path = url;
    try {
      path = new URL(url).pathname;
    } catch {
      path = url.split("?")[0] ?? url;
    }
    this.#frames.push({
      at: new Date().toISOString(),
      traceId,
      service: "desktop",
      msg: redactText(`${method} ${path} -> ${String(status)}`),
    });
    if (this.#frames.length > this.#capacity) {
      this.#frames.splice(0, this.#frames.length - this.#capacity);
    }
  }

  frames(): readonly DesktopFrame[] {
    return [...this.#frames];
  }
}

/** A merged export: the server's tenant bundle plus the desktop's own frames. */
export interface DiagnosticExport {
  readonly generatedAt: string;
  readonly server: unknown;
  readonly desktop: readonly DesktopFrame[];
  readonly truncated: boolean;
}

/**
 * Merge, redact and bound: desktop frames are dropped oldest-first until the whole export fits,
 * and the export is redacted as a whole before it is returned.
 */
export function mergeExport(
  server: unknown,
  frames: readonly DesktopFrame[],
  maxBytes = DEFAULT_BUNDLE_LIMIT * 2,
): DiagnosticExport {
  let kept = [...frames];
  let truncated = false;
  const encode = (items: readonly DesktopFrame[]) =>
    redactText(JSON.stringify({ generatedAt: "", server, desktop: items, truncated }));
  while (encode(kept).length > maxBytes && kept.length > 0) {
    kept = kept.slice(1);
    truncated = true;
  }
  const redacted = JSON.parse(
    redactText(JSON.stringify({ server, desktop: kept })),
  ) as { server: unknown; desktop: DesktopFrame[] };
  return { generatedAt: new Date().toISOString(), server: redacted.server, desktop: redacted.desktop, truncated };
}
