/**
 * Desktop session reconnect (APP-003).
 *
 * Active run identity and the stream cursor live in main-process durable state. The
 * renderer may read a projection of them; it cannot invent a new run by writing files.
 */

export interface ActiveRunSession {
  readonly runId: string;
  readonly generation: number;
  readonly streamCursor: string;
  readonly workspaceId: string;
}

/** In-memory stand-in for the main-process session file. */
export class DesktopSessionStore {
  #session: ActiveRunSession | null = null;

  /** Record the run the desktop is watching. */
  attach(session: ActiveRunSession): void {
    this.#session = session;
  }

  /** Drop the session (user signed out or run terminal). */
  clear(): void {
    this.#session = null;
  }

  /** Current session, if any. */
  active(): ActiveRunSession | null {
    return this.#session;
  }

  /**
   * Reconnect after the window or stream dropped. The same run and cursor come back
   * so the server can replay missed events (DOMAIN.md §9.3).
   */
  reconnect(): ActiveRunSession | null {
    return this.#session;
  }
}
