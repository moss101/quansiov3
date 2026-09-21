/**
 * Quansio public API v1 client for TypeScript (APP-013).
 *
 * Canonical owner: `sdk/typescript`. The operation table (`OPERATIONS`, `READ_PROJECTIONS`,
 * `ERROR_CODES`) is generated from the DOMAIN.md catalog by GOV-004's `gen_contracts.py`
 * and the request/response types from the OpenAPI document; this module is the transport:
 * a bearer session (or a development harness's trusted headers), `POST /v1/commands/<Name>`
 * with a client-minted command id, `GET /v1/…` projections, and the §15 error body as a
 * typed {@link ApiError}. The client addresses operations by id, so an operation the
 * catalog does not have is a type error before it is a request.
 */

import { ERROR_CODES, OPERATIONS, READ_PROJECTIONS, type ErrorCode, type OperationId } from "./generated/operations.js";

export { ERROR_CODES, OPERATIONS, READ_PROJECTIONS };
export type { ErrorCode, OperationId };

export const PUBLIC_API_VERSION = "v1" as const;

/** The operation ids this SDK exposes: every command the catalog names. */
export const GENERATED_OPERATIONS = Object.keys(OPERATIONS) as readonly OperationId[];

// A resource: lowercase path segments with canonical ids (uppercase ULIDs) and a query.
const RESOURCE_PATTERN = /^[a-z0-9][A-Za-z0-9/_\-{}?=&.,%]{0,255}$/;

/** Build a public API path: `/v1/<resource>` with the resource validated. */
export function publicApiPath(resource: string): string {
  if (!RESOURCE_PATTERN.test(resource) || resource.includes("..")) {
    throw new Error(`invalid public API resource: ${JSON.stringify(resource)}`);
  }
  return `/${PUBLIC_API_VERSION}/${resource}`;
}

export interface ApiErrorBody {
  readonly code: string;
  readonly message: string;
  readonly correlation_id: string;
  readonly retryable: boolean;
  readonly details?: unknown;
}

/** The §15 error, typed; `code` is one of {@link ERROR_CODES} when the server is current. */
export class ApiError extends Error {
  readonly code: string;
  readonly correlationId: string;
  readonly retryable: boolean;
  readonly status: number;
  readonly details: unknown;

  constructor(status: number, body: ApiErrorBody) {
    super(`${body.code}: ${body.message}`);
    this.name = "ApiError";
    this.status = status;
    this.code = body.code;
    this.correlationId = body.correlation_id;
    this.retryable = body.retryable;
    this.details = body.details ?? null;
  }

  /** Whether the code is in the catalogued taxonomy. */
  get isCatalogued(): boolean {
    return (ERROR_CODES as readonly string[]).includes(this.code);
  }
}

export interface CommandResponse<TResult = unknown> {
  readonly command_id: string;
  readonly replayed: boolean;
  readonly result: TResult;
}

export type Credential =
  | { readonly kind: "bearer"; readonly accessToken: () => string | null }
  | { readonly kind: "trusted-headers"; readonly tenantId: string; readonly userId: string };

export type FetchLike = (input: string, init: RequestInit) => Promise<Response>;

/** A client-minted, ULID-shaped command id (the server's idempotency key). */
export function newCommandId(): string {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  let time = Date.now();
  let stamp = "";
  for (let index = 0; index < 10; index += 1) {
    stamp = (alphabet[time % 32] ?? "0") + stamp;
    time = Math.floor(time / 32);
  }
  const random = new Uint8Array(16);
  crypto.getRandomValues(random);
  let entropy = "";
  for (const byte of random) {
    entropy += alphabet[byte % 32] ?? "0";
  }
  return `cmd_${stamp}${entropy}`;
}

export class QuansioClient {
  readonly origin: string;
  readonly #credential: Credential;
  readonly #fetch: FetchLike;

  constructor(origin: string, credential: Credential, fetchImpl: FetchLike = (input, init) => fetch(input, init)) {
    this.origin = origin.replace(/\/+$/, "");
    this.#credential = credential;
    this.#fetch = fetchImpl;
  }

  /** `GET /v1/<resource>` (a read projection). */
  get<T = unknown>(resource: string): Promise<T> {
    return this.#request<T>("GET", publicApiPath(resource));
  }

  /** `POST /v1/commands/<Name>` for a catalogued operation; returns the owner's result. */
  async command<T = unknown>(operation: OperationId, params: Record<string, unknown>, commandId: string = newCommandId()): Promise<T> {
    const entry = OPERATIONS[operation];
    const response = await this.#request<CommandResponse<T>>(entry.method, entry.path, JSON.stringify({ command_id: commandId, params }));
    return response.result;
  }

  /** The whole envelope, for callers that need `replayed`. */
  commandEnvelope<T = unknown>(operation: OperationId, params: Record<string, unknown>, commandId: string = newCommandId()): Promise<CommandResponse<T>> {
    const entry = OPERATIONS[operation];
    return this.#request<CommandResponse<T>>(entry.method, entry.path, JSON.stringify({ command_id: commandId, params }));
  }

  /** `GET /v1/health`. */
  health(): Promise<{ status: string; commands: number; read_projections: number }> {
    return this.get("health");
  }

  /** `GET /v1/me`. */
  me<T = unknown>(): Promise<T> {
    return this.get("me");
  }

  /** `POST /v1/auth/magic-link/start` — no session yet. */
  startMagicLink(email: string, displayName = ""): Promise<unknown> {
    return this.#request("POST", "/v1/auth/magic-link/start", JSON.stringify({ email, display_name: displayName }), false);
  }

  /** `POST /v1/auth/magic-link/complete` — the issued session. */
  completeMagicLink<T = unknown>(token: string): Promise<T> {
    return this.#request<T>("POST", "/v1/auth/magic-link/complete", JSON.stringify({ token }), false);
  }

  #headers(authenticated: boolean): Record<string, string> {
    const headers: Record<string, string> = { accept: "application/json", "x-quansio-client": "sdk-typescript" };
    if (!authenticated) {
      return headers;
    }
    if (this.#credential.kind === "bearer") {
      const token = this.#credential.accessToken();
      if (token) {
        headers["authorization"] = `Bearer ${token}`;
      }
    } else {
      headers["x-quansio-tenant"] = this.#credential.tenantId;
      headers["x-quansio-user"] = this.#credential.userId;
    }
    return headers;
  }

  async #request<T>(method: string, path: string, body?: string, authenticated = true): Promise<T> {
    if (!path.startsWith(`/${PUBLIC_API_VERSION}/`)) {
      throw new Error(`not a public API path: ${path}`);
    }
    const headers = this.#headers(authenticated);
    if (body !== undefined) {
      headers["content-type"] = "application/json";
    }
    const response = await this.#fetch(`${this.origin}${path}`, { method, headers, ...(body !== undefined ? { body } : {}) });
    const text = await response.text();
    let decoded: unknown = null;
    if (text.length > 0) {
      try {
        decoded = JSON.parse(text);
      } catch {
        decoded = null;
      }
    }
    if (!response.ok) {
      const error = decoded && typeof decoded === "object" ? (decoded as Partial<ApiErrorBody>) : {};
      throw new ApiError(response.status, {
        code: typeof error.code === "string" ? error.code : "INTERNAL",
        message: typeof error.message === "string" ? error.message : `HTTP ${String(response.status)}`,
        correlation_id: typeof error.correlation_id === "string" ? error.correlation_id : "",
        retryable: error.retryable === true,
        details: error.details ?? null,
      });
    }
    return decoded as T;
  }
}
