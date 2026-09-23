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
  /** The §15 `details` object, when the server typed the refusal further. */
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
}

export interface ApiErrorBody {
  readonly code: string;
  readonly message: string;
  readonly correlation_id: string;
  readonly retryable: boolean;
  readonly details?: unknown;
}

/** Trusted-header scope, used only by development harnesses (`QUANSIO_TRUST_SCOPE_HEADERS=1`). */
export interface SessionScope {
  readonly tenantId: string;
  readonly userId: string;
}

/** A session as `POST /v1/auth/*` returns it (DOMAIN.md §2). */
export interface Session {
  readonly session_id: string;
  readonly user_id: string;
  readonly tenant_id: string;
  readonly access_token: string;
  readonly access_expires_at: string;
  readonly refresh_token: string;
  readonly refresh_expires_at: string;
  readonly workspace_id?: string;
  readonly onboarded?: boolean;
}

/** `GET /v1/me`. */
export interface MeView {
  readonly user: { readonly id: string; readonly primary_email: string; readonly display_name: string; readonly auth_methods: readonly string[] };
  readonly session: { readonly id: string; readonly tenant_id: string };
  readonly tenants: readonly { readonly id: string; readonly name: string; readonly personal: boolean; readonly role: string }[];
  readonly workspaces: readonly { readonly id: string; readonly name: string; readonly role: string; readonly effective_settings: Record<string, unknown> }[];
  readonly auth_methods_offered: { readonly email_magic_link: boolean; readonly oauth: readonly string[] };
}

export type OAuthProvider = "google" | "microsoft" | "github";

/**
 * How requests are authenticated. A session credential presents the access token and, on
 * `AUTH_EXPIRED`, asks the source for a refreshed one exactly once before failing.
 */
export type Credential =
  | { readonly kind: "session"; readonly accessToken: () => string | null; readonly refresh: () => Promise<string | null> }
  | { readonly kind: "trusted-headers"; readonly scope: SessionScope };

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

/** Bytes of an artifact version as the server served them (with the label it gave them). */
export interface ArtifactBytes {
  readonly bytes: Uint8Array;
  readonly mediaType: string;
  /** The recorded digest, when the server labels the bytes with one (evidence). */
  readonly digest?: string;
}

function scopeQuery(scope: { readonly workspaceId?: string; readonly runId?: string; readonly status?: string }): string {
  const params = new URLSearchParams();
  if (scope.runId) {
    params.set("run_id", scope.runId);
  } else if (scope.workspaceId) {
    params.set("workspace_id", scope.workspaceId);
  }
  if (scope.status) {
    params.set("status", scope.status);
  }
  return params.toString();
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

/** Headers for a credential; a session without an access token sends none and lets the server refuse. */
export function credentialHeaders(credential: Credential): Record<string, string> {
  if (credential.kind === "trusted-headers") {
    return scopeHeaders(credential.scope);
  }
  const token = credential.accessToken();
  return token ? { "content-type": "application/json", authorization: `Bearer ${token}` } : { "content-type": "application/json" };
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
  readonly credential: Credential;
  /** Injectable for tests; defaults to the global `fetch`. */
  readonly fetch?: FetchLike;
  /** Per-request timeout; the server's own limits still apply. */
  readonly timeoutMs?: number;
}

/** HTTP half of the desktop's server client. */
export class ServerClient {
  readonly origin: string;
  readonly #credential: Credential;
  readonly #fetch: FetchLike;
  readonly #timeoutMs: number;

  constructor(options: ServerClientOptions) {
    this.origin = trimOrigin(options.origin);
    this.#credential = options.credential;
    this.#fetch = options.fetch ?? ((input, init) => fetch(input, init));
    this.#timeoutMs = options.timeoutMs ?? 30_000;
  }

  /** The credential in force (main uses it for the stream's upgrade headers). */
  get credential(): Credential {
    return this.#credential;
  }

  // ---- session endpoints (DOMAIN.md §2); none of these carry a credential ----

  magicLinkStart(email: string, displayName: string): Promise<{ challenge_id: string; email: string; expires_at: string; delivery: string }> {
    return this.#request("POST", `${this.origin}/v1/auth/magic-link/start`, JSON.stringify({ email, display_name: displayName }), false);
  }

  magicLinkComplete(token: string): Promise<Session> {
    return this.#request<Session>("POST", `${this.origin}/v1/auth/magic-link/complete`, JSON.stringify({ token }), false);
  }

  oauthStart(provider: OAuthProvider, redirectUri: string): Promise<{ provider: string; authorization_url: string; state: string; expires_at: string }> {
    return this.#request("GET", `${this.origin}/v1/auth/oauth/${provider}/start?redirect_uri=${encodeURIComponent(redirectUri)}`, undefined, false);
  }

  oauthComplete(provider: OAuthProvider, code: string, state: string, redirectUri: string): Promise<Session> {
    return this.#request<Session>("POST", `${this.origin}/v1/auth/oauth/${provider}/complete`, JSON.stringify({ code, state, redirect_uri: redirectUri }), false);
  }

  refreshSession(refreshToken: string): Promise<Session> {
    return this.#request<Session>("POST", `${this.origin}/v1/auth/refresh`, JSON.stringify({ refresh_token: refreshToken }), false);
  }

  signOut(): Promise<{ signed_out: boolean; session_id: string }> {
    return this.#request("POST", `${this.origin}/v1/auth/sign-out`, "{}");
  }

  me(): Promise<MeView> {
    return this.#request<MeView>("GET", `${this.origin}/v1/me`);
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

  // ---- the conversation surface (APP-004) ----

  threads(workspaceId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/threads?workspace_id=${encodeURIComponent(workspaceId)}`);
  }

  timeline(runId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/runs/${encodeURIComponent(runId)}/timeline`);
  }

  questions(scope: { readonly threadId: string } | { readonly runId: string }): Promise<unknown> {
    const query = "threadId" in scope ? `thread_id=${encodeURIComponent(scope.threadId)}` : `run_id=${encodeURIComponent(scope.runId)}`;
    return this.#request("GET", `${this.origin}/v1/questions?${query}`);
  }

  artifacts(workspaceId: string, filter: { readonly kind?: string | null; readonly runId?: string | null } = {}): Promise<unknown> {
    const params = new URLSearchParams({ workspace_id: workspaceId });
    if (filter.kind) {
      params.set("kind", filter.kind);
    }
    if (filter.runId) {
      params.set("run_id", filter.runId);
    }
    return this.#request("GET", `${this.origin}/v1/artifacts?${params.toString()}`);
  }

  // ---- the artifact workspace (APP-008) ----

  artifactVersions(artifactId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/artifacts/${encodeURIComponent(artifactId)}/versions`);
  }

  /** A version's bytes, digest-verified by the server; never interpreted here. */
  artifactContent(artifactId: string, versionId: string): Promise<ArtifactBytes> {
    return this.#requestBytes(`${this.origin}/v1/artifacts/${encodeURIComponent(artifactId)}/versions/${encodeURIComponent(versionId)}/content`);
  }

  evidence(evidenceId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/evidence/${encodeURIComponent(evidenceId)}`);
  }

  /** `GET /v1/search?…` with a query string the typed builder produced. */
  search(queryString: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/search?${queryString}`);
  }

  // ---- approvals, effects and evidence (APP-006) ----

  approvals(scope: { readonly workspaceId?: string; readonly runId?: string; readonly status?: string }): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/approvals?${scopeQuery(scope)}`);
  }

  effectsIn(scope: { readonly workspaceId?: string; readonly runId?: string; readonly status?: string }): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/effects?${scopeQuery(scope)}`);
  }

  /** Evidence bytes with the digest the server recorded them under (`x-quansio-digest`). */
  async evidenceContent(evidenceId: string): Promise<{ bytes: Uint8Array; digest: string }> {
    const url = `${this.origin}/v1/evidence/${encodeURIComponent(evidenceId)}/content`;
    const fetched = await this.#requestBytes(url);
    return { bytes: fetched.bytes, digest: fetched.digest ?? "" };
  }

  // ---- knowledge, memory and skills (APP-011) ----

  knowledge(workspaceId: string, filter: { readonly status?: string; readonly includeDeleted?: boolean } = {}): Promise<unknown> {
    const params = new URLSearchParams({ workspace_id: workspaceId });
    if (filter.status) {
      params.set("status", filter.status);
    }
    if (filter.includeDeleted) {
      params.set("include_deleted", "true");
    }
    return this.#request("GET", `${this.origin}/v1/knowledge?${params.toString()}`);
  }

  memory(workspaceId: string, filter: { readonly scope?: string; readonly subjectRef?: string; readonly includeDeleted?: boolean } = {}): Promise<unknown> {
    const params = new URLSearchParams({ workspace_id: workspaceId });
    if (filter.scope) {
      params.set("scope", filter.scope);
    }
    if (filter.subjectRef) {
      params.set("subject_ref", filter.subjectRef);
    }
    if (filter.includeDeleted) {
      params.set("include_deleted", "true");
    }
    return this.#request("GET", `${this.origin}/v1/memory?${params.toString()}`);
  }

  skills(): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/skills`);
  }

  // ---- the teammate roster (APP-014) ----

  teammates(workspaceId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/teammates?workspace_id=${encodeURIComponent(workspaceId)}`);
  }

  // ---- routines and notifications (APP-010) ----

  routines(workspaceId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/routines?workspace_id=${encodeURIComponent(workspaceId)}`);
  }

  notifications(workspaceId: string, unreadOnly = false): Promise<unknown> {
    const unread = unreadOnly ? "&unread=true" : "";
    return this.#request("GET", `${this.origin}/v1/notifications?workspace_id=${encodeURIComponent(workspaceId)}${unread}`);
  }

  // ---- the live browser/computer/terminal surface (APP-007) ----

  targets(workspaceId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/targets?workspace_id=${encodeURIComponent(workspaceId)}`);
  }

  browserSession(sessionId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/browser-sessions/${encodeURIComponent(sessionId)}`);
  }

  // ---- the plan / WorkGraph surface (APP-005) ----

  workgraph(workspaceId: string): Promise<unknown> {
    return this.#request("GET", `${this.origin}/v1/workgraph?workspace_id=${encodeURIComponent(workspaceId)}`);
  }

  async #requestBytes(url: string): Promise<ArtifactBytes> {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      controller.abort();
    }, this.#timeoutMs);
    try {
      const response = await this.#fetch(url, { method: "GET", headers: credentialHeaders(this.#credential), signal: controller.signal });
      if (!response.ok) {
        const text = await response.text();
        const decoded: unknown = text ? JSON.parse(text) : null;
        throw new ApiError(response.status, asErrorBody(decoded, response.status));
      }
      const bytes = new Uint8Array(await response.arrayBuffer());
      const mediaType = response.headers.get("content-type") ?? "application/octet-stream";
      const digest = response.headers.get("x-quansio-digest");
      return digest ? { bytes, mediaType, digest } : { bytes, mediaType };
    } finally {
      clearTimeout(timer);
    }
  }

  async #request<T>(method: "GET" | "POST", url: string, body?: string, authenticated = true): Promise<T> {
    const attempt = async (headers: Record<string, string>): Promise<{ status: number; ok: boolean; decoded: unknown }> => {
      const controller = new AbortController();
      const timer = setTimeout(() => {
        controller.abort();
      }, this.#timeoutMs);
      try {
        const init: RequestInit = { method, headers, signal: controller.signal };
        if (body !== undefined) {
          init.body = body;
        }
        const response = await this.#fetch(url, init);
        const text = await response.text();
        return { status: response.status, ok: response.ok, decoded: text ? (JSON.parse(text) as unknown) : null };
      } finally {
        clearTimeout(timer);
      }
    };
    const headers = authenticated ? credentialHeaders(this.#credential) : { "content-type": "application/json" };
    let result = await attempt(headers);
    if (!result.ok && authenticated && this.#credential.kind === "session") {
      const error = asErrorBody(result.decoded, result.status);
      if (error.code === "AUTH_EXPIRED") {
        // One refresh, then the same request again; a second failure is the caller's.
        const refreshed = await this.#credential.refresh();
        if (refreshed) {
          result = await attempt({ "content-type": "application/json", authorization: `Bearer ${refreshed}` });
        }
      }
    }
    if (!result.ok) {
      throw new ApiError(result.status, asErrorBody(result.decoded, result.status));
    }
    return result.decoded as T;
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
      details: body.details ?? null,
    };
  }
  return { code: "INTERNAL", message: `HTTP ${String(status)}`, correlation_id: "", retryable: status >= 500 };
}
