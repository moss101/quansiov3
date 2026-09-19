/**
 * Session manager (APP-002): sign-in, the in-memory access token, refresh rotation, and
 * the refresh token's custody. Owned by the main process; the renderer sees only a
 * projection (`signedIn`, the user's email, the workspace) through the bridge.
 */

import type { Credential, MeView, OAuthProvider, ServerClient, Session } from "../client/server.js";
import type { StoredSession, TokenStore } from "./token-store.js";

export type SessionState =
  | { readonly kind: "signed-out"; readonly persistent: boolean }
  | { readonly kind: "link-sent"; readonly email: string; readonly expiresAt: string; readonly delivery: string }
  | { readonly kind: "signed-in"; readonly userId: string; readonly tenantId: string; readonly workspaceId: string | null; readonly onboarded: boolean };

export interface SessionManagerOptions {
  readonly serverOrigin: string;
  readonly tokens: TokenStore;
  /** Builds a client bound to this manager's credential. */
  readonly client: (credential: Credential) => ServerClient;
  readonly onState?: (state: SessionState) => void;
}

/** The desktop's callback URL for OAuth (a deep link the main process owns). */
export const OAUTH_REDIRECT_URI = "quansio://auth/callback";

export class SessionManager {
  readonly client: ServerClient;
  #accessToken: string | null = null;
  #refreshToken: string | null = null;
  #session: Session | null = null;
  #state: SessionState;
  #pendingOAuth: { provider: OAuthProvider; state: string } | null = null;
  readonly #options: SessionManagerOptions;

  constructor(options: SessionManagerOptions) {
    this.#options = options;
    this.#state = { kind: "signed-out", persistent: options.tokens.persistent() };
    this.client = options.client({
      kind: "session",
      accessToken: () => this.#accessToken,
      refresh: () => this.refresh(),
    });
  }

  state(): SessionState {
    return this.#state;
  }

  /** The current session (tokens included); main only. */
  session(): Session | null {
    return this.#session;
  }

  accessToken(): string | null {
    return this.#accessToken;
  }

  /** Resume from the keychain: a stored refresh token is rotated into a live session. */
  async resume(): Promise<boolean> {
    const stored = this.#options.tokens.load();
    if (!stored || stored.serverOrigin !== this.client.origin) {
      return false;
    }
    if (new Date(stored.refreshExpiresAt).getTime() <= Date.now()) {
      this.#options.tokens.clear();
      return false;
    }
    this.#refreshToken = stored.refreshToken;
    const refreshed = await this.refresh();
    if (!refreshed) {
      return false;
    }
    await this.#hydrate();
    return true;
  }

  async startMagicLink(email: string, displayName: string): Promise<SessionState> {
    const started = await this.client.magicLinkStart(email, displayName);
    this.#set({ kind: "link-sent", email: started.email, expiresAt: started.expires_at, delivery: started.delivery });
    return this.#state;
  }

  async completeMagicLink(token: string): Promise<SessionState> {
    this.#adopt(await this.client.magicLinkComplete(token));
    await this.#hydrate();
    return this.#state;
  }

  async startOAuth(provider: OAuthProvider): Promise<{ authorizationUrl: string; state: string }> {
    const started = await this.client.oauthStart(provider, OAUTH_REDIRECT_URI);
    this.#pendingOAuth = { provider, state: started.state };
    return { authorizationUrl: started.authorization_url, state: started.state };
  }

  /** Complete the OAuth flow from the callback deep link's `code` and `state`. */
  async completeOAuth(code: string, state: string): Promise<SessionState> {
    const pending = this.#pendingOAuth;
    if (!pending || pending.state !== state) {
      throw new Error("no OAuth request is pending for this state");
    }
    this.#pendingOAuth = null;
    this.#adopt(await this.client.oauthComplete(pending.provider, code, state, OAUTH_REDIRECT_URI));
    await this.#hydrate();
    return this.#state;
  }

  /** Rotate the refresh token; on refusal the session is gone and custody is cleared. */
  async refresh(): Promise<string | null> {
    const refreshToken = this.#refreshToken;
    if (!refreshToken) {
      return null;
    }
    try {
      this.#adopt(await this.client.refreshSession(refreshToken));
      return this.#accessToken;
    } catch {
      this.#forget();
      return null;
    }
  }

  async signOut(): Promise<void> {
    try {
      if (this.#accessToken) {
        await this.client.signOut();
      }
    } finally {
      this.#forget();
    }
  }

  me(): Promise<MeView> {
    return this.client.me();
  }

  #adopt(session: Session): void {
    this.#session = session;
    this.#accessToken = session.access_token;
    this.#refreshToken = session.refresh_token;
    const stored: StoredSession = {
      serverOrigin: this.client.origin,
      refreshToken: session.refresh_token,
      refreshExpiresAt: session.refresh_expires_at,
      userId: session.user_id,
      tenantId: session.tenant_id,
    };
    this.#options.tokens.save(stored);
    this.#set({
      kind: "signed-in",
      userId: session.user_id,
      tenantId: session.tenant_id,
      workspaceId: session.workspace_id ?? (this.#state.kind === "signed-in" ? this.#state.workspaceId : null),
      onboarded: session.onboarded ?? false,
    });
  }

  async #hydrate(): Promise<void> {
    if (this.#state.kind !== "signed-in" || this.#state.workspaceId) {
      return;
    }
    const me = await this.client.me();
    const workspace = me.workspaces[0];
    this.#set({ ...this.#state, workspaceId: workspace ? workspace.id : null });
  }

  #forget(): void {
    this.#session = null;
    this.#accessToken = null;
    this.#refreshToken = null;
    this.#pendingOAuth = null;
    this.#options.tokens.clear();
    this.#set({ kind: "signed-out", persistent: this.#options.tokens.persistent() });
  }

  #set(state: SessionState): void {
    this.#state = state;
    this.#options.onState?.(state);
  }
}
