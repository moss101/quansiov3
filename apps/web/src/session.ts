/**
 * The browser session (APP-012): a bearer session from `/v1/auth/*`, kept for this tab
 * only (`sessionStorage`), refreshed once on `AUTH_EXPIRED`, cleared on sign-out. No
 * cookie, no other credential. A development harness may instead hand the app a
 * trusted-header scope; a production server ignores those headers.
 */

import { ApiError, WebClient, type Credential, type FetchLike } from "./client.js";

export interface StoredSession {
  readonly access_token: string;
  readonly refresh_token: string;
  readonly tenant_id: string;
  readonly user_id: string;
  readonly workspace_id?: string;
}

export interface SessionStorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export const SESSION_KEY = "quansio.web.session";

/** Decode a stored session; anything malformed is no session. */
export function parseStoredSession(raw: string | null): StoredSession | null {
  if (!raw) {
    return null;
  }
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    if (typeof value["access_token"] === "string" && typeof value["refresh_token"] === "string" && typeof value["tenant_id"] === "string" && typeof value["user_id"] === "string") {
      return {
        access_token: value["access_token"],
        refresh_token: value["refresh_token"],
        tenant_id: value["tenant_id"],
        user_id: value["user_id"],
        ...(typeof value["workspace_id"] === "string" ? { workspace_id: value["workspace_id"] } : {}),
      };
    }
  } catch {
    /* not a session */
  }
  return null;
}

export class WebSession {
  readonly origin: string;
  readonly #storage: SessionStorageLike | null;
  readonly #fetch: FetchLike;
  #session: StoredSession | null;
  readonly #harness: { tenantId: string; userId: string } | null;

  constructor(origin: string, options: { storage?: SessionStorageLike | null; fetchImpl?: FetchLike; harness?: { tenantId: string; userId: string } | null } = {}) {
    this.origin = origin;
    this.#storage = options.storage ?? null;
    this.#fetch = options.fetchImpl ?? ((input, init) => fetch(input, init));
    this.#harness = options.harness ?? null;
    this.#session = parseStoredSession(this.#storage?.getItem(SESSION_KEY) ?? null);
  }

  /** Whether a session (or a harness scope) is present. */
  get signedIn(): boolean {
    return this.#session !== null || this.#harness !== null;
  }

  get current(): StoredSession | null {
    return this.#session;
  }

  /** The credential the client presents. */
  credential(): Credential {
    if (this.#harness) {
      return { kind: "trusted-headers", tenantId: this.#harness.tenantId, userId: this.#harness.userId };
    }
    return { kind: "bearer", accessToken: () => this.#session?.access_token ?? null };
  }

  /** A client over this session; on `AUTH_EXPIRED` the access token is refreshed once. */
  client(): WebClient {
    const inner = new WebClient(this.origin, this.credential(), this.#fetch);
    const refresh = async (): Promise<boolean> => {
      if (!this.#session) {
        return false;
      }
      try {
        const renewed = await new WebClient(this.origin, this.credential(), this.#fetch).postUnauthenticated<Record<string, unknown>>("/v1/auth/refresh", { refresh_token: this.#session.refresh_token });
        this.#store(renewed);
        return true;
      } catch {
        return false;
      }
    };
    const retrying = new Proxy(inner, {
      get: (target, property, receiver) => {
        const value = Reflect.get(target, property, receiver) as unknown;
        if ((property === "get" || property === "invoke") && typeof value === "function") {
          return async (...args: unknown[]) => {
            try {
              return await (value as (...inner: unknown[]) => Promise<unknown>).apply(target, args);
            } catch (error) {
              if (error instanceof ApiError && error.code === "AUTH_EXPIRED" && (await refresh())) {
                return (value as (...inner: unknown[]) => Promise<unknown>).apply(new WebClient(this.origin, this.credential(), this.#fetch), args);
              }
              throw error;
            }
          };
        }
        return value;
      },
    });
    return retrying;
  }

  /** `POST /v1/auth/magic-link/start`: the server delivers the link; nothing is stored yet. */
  async startMagicLink(email: string, displayName: string): Promise<unknown> {
    return new WebClient(this.origin, this.credential(), this.#fetch).postUnauthenticated("/v1/auth/magic-link/start", { email, display_name: displayName });
  }

  /** `POST /v1/auth/magic-link/complete`: the token from the link becomes the session. */
  async completeMagicLink(token: string): Promise<StoredSession> {
    const session = await new WebClient(this.origin, this.credential(), this.#fetch).postUnauthenticated<Record<string, unknown>>("/v1/auth/magic-link/complete", { token });
    return this.#store(session);
  }

  /** `POST /v1/auth/sign-out`, then forget the session whatever the server said. */
  async signOut(): Promise<void> {
    try {
      await new WebClient(this.origin, this.credential(), this.#fetch).post("/v1/auth/sign-out", {});
    } catch {
      /* the session is forgotten regardless */
    }
    this.#session = null;
    this.#storage?.removeItem(SESSION_KEY);
  }

  #store(raw: Record<string, unknown>): StoredSession {
    const session = parseStoredSession(JSON.stringify(raw));
    if (!session) {
      throw new Error("the server returned no usable session");
    }
    this.#session = session;
    this.#storage?.setItem(SESSION_KEY, JSON.stringify(session));
    return session;
  }
}
