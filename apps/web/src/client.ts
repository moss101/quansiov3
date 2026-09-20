/**
 * The web app's client of the public API v1 (APP-012).
 *
 * The browser is a client exactly like the desktop (DOSSIER.md §4.1): every read is a
 * projection under `/v1/`, every mutation is `POST /v1/commands/<Name>` with a
 * client-minted `command_id`, and every refusal is the §15 error body — surfaced as
 * {@link ApiError} so a page branches on `code` and `details` (an RBAC refusal carries
 * `details.kind = "rbac"` and the action the server refused). The client holds a bearer
 * session from `/v1/auth/*` and nothing else: no host API, no cookie, no credential of
 * any other kind. The trusted-header mode exists for development harnesses only
 * (`QUANSIO_TRUST_SCOPE_HEADERS=1`); a production server ignores those headers.
 */

export interface ApiErrorBody {
  readonly code: string;
  readonly message: string;
  readonly correlation_id: string;
  readonly retryable: boolean;
  readonly details?: unknown;
}

/** The §15 error, typed. */
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

  /** Whether the server refused the action for the user's role. */
  get isRbacRefusal(): boolean {
    return this.code === "SCOPE_FORBIDDEN" && typeof this.details === "object" && this.details !== null && (this.details as { kind?: unknown }).kind === "rbac";
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

const PUBLIC_API = "/v1/";
const COMMAND_NAME = /^[A-Z][A-Za-z]{2,63}$/;

/** A public API path is always `/v1/…`; nothing else is addressed. */
export function publicPath(origin: string, resource: string): string {
  if (!resource.startsWith(PUBLIC_API)) {
    throw new Error(`not a public API path: ${resource}`);
  }
  return `${origin.replace(/\/+$/, "")}${resource}`;
}

/** A command id the server treats as an idempotency key: a client-minted ULID-shaped token. */
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

export class WebClient {
  readonly origin: string;
  readonly #credential: Credential;
  readonly #fetch: FetchLike;

  constructor(origin: string, credential: Credential, fetchImpl: FetchLike = (input, init) => fetch(input, init)) {
    this.origin = origin.replace(/\/+$/, "");
    this.#credential = credential;
    this.#fetch = fetchImpl;
  }

  /** `GET /v1/<resource>`. */
  get<T = unknown>(resource: string): Promise<T> {
    return this.#request<T>("GET", resource);
  }

  /** `POST /v1/commands/<Name>` with a fresh command id; returns the owner's result. */
  async invoke<T = unknown>(name: string, params: Record<string, unknown>): Promise<T> {
    if (!COMMAND_NAME.test(name)) {
      throw new Error(`not a command name: ${name}`);
    }
    const response = await this.#request<CommandResponse<T>>("POST", `/v1/commands/${name}`, JSON.stringify({ command_id: newCommandId(), params }));
    return response.result;
  }

  /** An authenticated `POST /v1/<resource>` that is not a command (`/v1/auth/sign-out`). */
  post<T = unknown>(resource: string, body: unknown): Promise<T> {
    return this.#request<T>("POST", resource, JSON.stringify(body));
  }

  /** `POST /v1/auth/*` before a session exists. */
  postUnauthenticated<T = unknown>(resource: string, body: unknown): Promise<T> {
    return this.#request<T>("POST", resource, JSON.stringify(body), false);
  }

  #headers(authenticated: boolean): Record<string, string> {
    const headers: Record<string, string> = { accept: "application/json" };
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

  async #request<T>(method: "GET" | "POST", resource: string, body?: string, authenticated = true): Promise<T> {
    const headers = this.#headers(authenticated);
    if (body !== undefined) {
      headers["content-type"] = "application/json";
    }
    const response = await this.#fetch(publicPath(this.origin, resource), { method, headers, ...(body !== undefined ? { body } : {}) });
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
