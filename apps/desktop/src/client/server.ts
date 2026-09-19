/**
 * Typed desktop client for the public API v1 (APP-003).
 *
 * The desktop is a client of `quansio-server` (DOSSIER.md §4.1): every mutation is a public
 * command, every read is a projection, and the client holds only session scope — never a
 * provider credential, connector token or worker secret. The main process owns the client;
 * the renderer reaches it through the preload bridge and never sees the scope headers.
 *
 * Wire contract (DOMAIN.md §14–§15, served by APP-001):
 *   POST /v1/commands/<Name>  {command_id, params}  →  {command_id, replayed, result}
 *   GET  /v1/runs/{id} | /v1/threads/{id} | /v1/messages?thread_id= | /v1/effects?run_id=
 *   GET  /v1/health
 *   errors: {code, message, correlation_id, retryable}
 */

export interface CommandResponse<TResult = unknown> {
  readonly command_id: string;
  readonly replayed: boolean;
  readonly result: TResult;
}

/** The §15 error body, surfaced as a typed error so callers can branch on `code`. */
export class ApiError extends Error {
  readonly code: string;
  readonly correlationId: string;
  readonly retryable: boolean;
  readonly status: number;

  constructor(status: number, body: ApiErrorBody) {
    super(`${body.code}: ${body.message}`);
    this.name = "ApiError";
    this.status = status;
    this.code = body.code;
    this.correlationId = body.correlation_id;
    this.retryable = body.retryable;
  }
}

export interface ApiErrorBody {
  readonly code: string;
  readonly message: string;
  readonly correlation_id: string;
  readonly retryable: boolean;
}

/** Session scope the main process attaches to every request (APP-002 supplies it). */
export interface SessionScope {
  readonly tenantId: string;
  readonly userId: string;
}

export interface HealthReport {
  readonly status: string;
  readonly commands: number;
  readonly read_projections: number;
}

/** The `PostMessage` result the walking skeleton establishes (APP-001). */
export interface PostMessageResult {
  readonly thread_id: string;
  readonly run_id: string;
}

export interface RunProjection {
  readonly id: string;
  readonly state?: string;
  readonly [key: string]: unknown;
}

/** Build the command URL the OpenAPI catalog defines. */
export function commandUrl(origin: string, name: string): string {
  assertCommandName(name);
  return `${trimOrigin(origin)}/v1/commands/${name}`;
}

/** Build the stream URL (DOMAIN.md §9.3). */
export function streamUrl(origin: string): string {
  return trimOrigin(origin).replace(/^http/, "ws") + "/v1/stream";
}

/** Headers the main process attaches; the renderer never sees the scope. */
export function scopeHeaders(scope: SessionScope): Record<string, string> {
  return {
    "content-type": "application/json",
    "x-quansio-tenant": scope.tenantId,
    "x-quansio-user": scope.userId,
  };
}

/** Command names are `UpperCamelCase` identifiers from the catalog; anything else is refused. */
export function assertCommandName(name: string): void {
  if (!/^[A-Z][A-Za-z0-9]{1,63}$/.test(name)) {
    throw new Error(`invalid command name: ${JSON.stringify(name)}`);
  }
}

function trimOrigin(origin: string): string {
  const trimmed = origin.replace(/\/+$/, "");
  if (!/^https?:\/\/[^/\s]+$/.test(trimmed)) {
    throw new Error(`server origin must be an http(s) origin without a path: ${JSON.stringify(origin)}`);
  }
  return trimmed;
}

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface ServerClientOptions {
  readonly origin: string;
  readonly scope: SessionScope;
  /** Injectable for tests; defaults to the global `fetch`. */
  readonly fetch?: FetchLike;
  /** Per-request timeout; the server's own limits still apply. */
  readonly timeoutMs?: number;
}

/** HTTP half of the desktop's server client. */
export class ServerClient {
  readonly origin: string;
  readonly #scope: SessionScope;
  readonly #fetch: FetchLike;
  readonly #timeoutMs: number;

  constructor(options: ServerClientOptions) {
    this.origin = trimOrigin(options.origin);
    this.#scope = options.scope;
    this.#fetch = options.fetch ?? ((input, init) => fetch(input, init));
    this.#timeoutMs = options.timeoutMs ?? 30_000;
  }

  /** Invoke a public command; `commandId` makes the call idempotent on the server. */
  async invoke<TResult = unknown>(
    name: string,
    commandId: string,
    params: Record<string, unknown>,
  ): Promise<CommandResponse<TResult>> {
    const body = JSON.stringify({ command_id: commandId, params });
    return this.#request<CommandResponse<TResult>>("POST", commandUrl(this.origin, name), body);
  }

  postMessage(commandId: string, workspaceId: string, content: string): Promise<CommandResponse<PostMessageResult>> {
    return this.invoke<PostMessageResult>("PostMessage", commandId, { workspace_id: workspaceId, content });
  }

  health(): Promise<HealthReport> {
    return this.#request<HealthReport>("GET", `${this.origin}/v1/health`);
  }

  run(runId: string): Promise<RunProjection> {
    return this.#request<RunProjection>("GET", `${this.origin}/v1/runs/${encodeURIComponent(runId)}`);
  }

  thread(threadId: string): Promise<Record<string, unknown>> {
    return this.#request("GET", `${this.origin}/v1/threads/${encodeURIComponent(threadId)}`);
  }

  messages(threadId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/messages?thread_id=${encodeURIComponent(threadId)}`);
  }

  effects(runId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/effects?run_id=${encodeURIComponent(runId)}`);
  }

  async #request<T>(method: "GET" | "POST", url: string, body?: string): Promise<T> {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      controller.abort();
    }, this.#timeoutMs);
    try {
      const init: RequestInit = { method, headers: scopeHeaders(this.#scope), signal: controller.signal };
      if (body !== undefined) {
        init.body = body;
      }
      const response = await this.#fetch(url, init);
      const text = await response.text();
      const decoded: unknown = text ? JSON.parse(text) : null;
      if (!response.ok) {
        throw new ApiError(response.status, asErrorBody(decoded, response.status));
      }
      return decoded as T;
    } finally {
      clearTimeout(timer);
    }
  }
}

function asErrorBody(decoded: unknown, status: number): ApiErrorBody {
  if (decoded && typeof decoded === "object" && "code" in decoded && "message" in decoded) {
    const body = decoded as Partial<ApiErrorBody>;
    return {
      code: typeof body.code === "string" ? body.code : "INTERNAL",
      message: typeof body.message === "string" ? body.message : "",
      correlation_id: typeof body.correlation_id === "string" ? body.correlation_id : "",
      retryable: body.retryable === true,
    };
  }
  return { code: "INTERNAL", message: `HTTP ${String(status)}`, correlation_id: "", retryable: status >= 500 };
}
