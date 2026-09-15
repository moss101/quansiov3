/**
 * Typed desktop client for the public API (APP-003).
 *
 * Talks only HTTP/WebSocket to quansio-server. No credentials in the renderer: the
 * main process attaches the tenant header from the OS keychain (APP-002).
 */

export interface CommandResponse {
  readonly command_id: string;
  readonly replayed: boolean;
  readonly result: unknown;
}

export interface StreamFrame {
  readonly kind: "event" | "live";
  readonly cursor?: string;
}

/** Build the command URL the OpenAPI catalog defines. */
export function commandUrl(origin: string, name: string): string {
  return `${origin.replace(/\/$/, "")}/v1/commands/${name}`;
}

/** Build the stream URL (DOMAIN.md §9.3). */
export function streamUrl(origin: string): string {
  const http = origin.replace(/\/$/, "");
  return http.replace(/^http/, "ws") + "/v1/stream";
}

/** Headers main attaches; renderer never sees the raw token. */
export function tenantHeaders(tenantId: string): Record<string, string> {
  return {
    "content-type": "application/json",
    "x-quansio-tenant": tenantId,
  };
}
