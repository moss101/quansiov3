/**
 * Durable desktop session (APP-003).
 *
 * The main process keeps the active run identity and the last stream cursor on disk so a
 * window reload, a dropped socket or a desktop restart resumes the same subscription and
 * the same run without the renderer holding anything. The file is advisory, never
 * authority: the server owns runs and events, and losing the file only means the next
 * subscribe starts from the server's current position.
 */

import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export interface ActiveRunSession {
  readonly runId: string;
  readonly threadId: string;
  readonly generation: number;
  readonly streamCursor: string;
  readonly workspaceId: string;
}

export interface DesktopSession {
  readonly serverOrigin: string;
  readonly workspaceId: string;
  readonly activeRun: ActiveRunSession | null;
  readonly streamCursor: string | null;
  readonly updatedAt: string;
}

export const SESSION_FILE_NAME = "session.json";
const SCHEMA_VERSION = 1;

/** Validate a decoded session object; anything malformed is treated as no session. */
export function parseSession(decoded: unknown): DesktopSession | null {
  if (!decoded || typeof decoded !== "object") {
    return null;
  }
  const record = decoded as Record<string, unknown>;
  if (record["schema_version"] !== SCHEMA_VERSION) {
    return null;
  }
  const serverOrigin = record["server_origin"];
  const workspaceId = record["workspace_id"];
  if (typeof serverOrigin !== "string" || typeof workspaceId !== "string") {
    return null;
  }
  const cursor = record["stream_cursor"];
  const rawRun = record["active_run"];
  let activeRun: ActiveRunSession | null = null;
  if (rawRun && typeof rawRun === "object") {
    const run = rawRun as Record<string, unknown>;
    if (
      typeof run["run_id"] === "string" &&
      typeof run["thread_id"] === "string" &&
      typeof run["generation"] === "number" &&
      typeof run["stream_cursor"] === "string" &&
      typeof run["workspace_id"] === "string"
    ) {
      activeRun = {
        runId: run["run_id"],
        threadId: run["thread_id"],
        generation: run["generation"],
        streamCursor: run["stream_cursor"],
        workspaceId: run["workspace_id"],
      };
    }
  }
  return {
    serverOrigin,
    workspaceId,
    activeRun,
    streamCursor: typeof cursor === "string" ? cursor : null,
    updatedAt: typeof record["updated_at"] === "string" ? record["updated_at"] : "",
  };
}

export function serializeSession(session: DesktopSession): string {
  return JSON.stringify(
    {
      schema_version: SCHEMA_VERSION,
      server_origin: session.serverOrigin,
      workspace_id: session.workspaceId,
      stream_cursor: session.streamCursor,
      active_run: session.activeRun
        ? {
            run_id: session.activeRun.runId,
            thread_id: session.activeRun.threadId,
            generation: session.activeRun.generation,
            stream_cursor: session.activeRun.streamCursor,
            workspace_id: session.activeRun.workspaceId,
          }
        : null,
      updated_at: session.updatedAt,
    },
    null,
    2,
  );
}

/** Session store backed by an atomically rewritten JSON file in the user-data directory. */
export class SessionFile {
  readonly path: string;
  #session: DesktopSession | null;

  constructor(userDataDir: string) {
    this.path = join(userDataDir, SESSION_FILE_NAME);
    this.#session = this.#load();
  }

  current(): DesktopSession | null {
    return this.#session;
  }

  /** Start (or replace) the session for a server and workspace; the cursor starts empty. */
  begin(serverOrigin: string, workspaceId: string): DesktopSession {
    return this.#write({ serverOrigin, workspaceId, activeRun: null, streamCursor: null, updatedAt: now() });
  }

  /** Record the run the desktop is watching. */
  attach(run: ActiveRunSession): DesktopSession {
    const base = this.#require();
    return this.#write({ ...base, activeRun: run, streamCursor: run.streamCursor, updatedAt: now() });
  }

  /** Advance the durable cursor after a durable event; live frames never move it. */
  advance(cursor: string): DesktopSession {
    const base = this.#require();
    const activeRun = base.activeRun ? { ...base.activeRun, streamCursor: cursor } : null;
    return this.#write({ ...base, activeRun, streamCursor: cursor, updatedAt: now() });
  }

  /** Forget the cursor: the next subscribe starts from the server's current position. */
  resetCursor(): DesktopSession {
    const base = this.#require();
    const activeRun = base.activeRun ? { ...base.activeRun, streamCursor: "" } : null;
    return this.#write({ ...base, activeRun, streamCursor: null, updatedAt: now() });
  }

  /** Drop the run (terminal or signed out) but keep the cursor for the workspace stream. */
  detach(): DesktopSession {
    const base = this.#require();
    return this.#write({ ...base, activeRun: null, updatedAt: now() });
  }

  clear(): void {
    this.#session = null;
    this.#persist(null);
  }

  #require(): DesktopSession {
    if (!this.#session) {
      throw new Error("no desktop session; call begin() first");
    }
    return this.#session;
  }

  #write(session: DesktopSession): DesktopSession {
    this.#session = session;
    this.#persist(session);
    return session;
  }

  #persist(session: DesktopSession | null): void {
    mkdirSync(dirname(this.path), { recursive: true, mode: 0o700 });
    const temporary = `${this.path}.tmp`;
    writeFileSync(temporary, session ? serializeSession(session) : "null\n", { mode: 0o600 });
    renameSync(temporary, this.path);
  }

  #load(): DesktopSession | null {
    try {
      return parseSession(JSON.parse(readFileSync(this.path, "utf8")));
    } catch {
      return null;
    }
  }
}

function now(): string {
  return new Date().toISOString();
}
