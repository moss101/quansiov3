/**
 * Live browser/computer/terminal surface (APP-007).
 *
 * Takeover never mints a replacement session. While the user owns control, agent
 * input is fenced. Reconnect restores the same session id and durable terminal cursor.
 */

export type Controller = "agent" | "user";

export interface LiveSession {
  readonly sessionId: string;
  readonly kind: "browser" | "computer" | "terminal";
  readonly controller: Controller;
  readonly paused: boolean;
  readonly terminalCursor: string | null;
}

export function takeover(session: LiveSession): LiveSession {
  return { ...session, controller: "user", paused: true };
}

export function handback(session: LiveSession): LiveSession {
  return { ...session, controller: "agent", paused: false };
}

export function agentInputAllowed(session: LiveSession): boolean {
  return session.controller === "agent" && !session.paused;
}

export function reconnect(session: LiveSession): LiveSession {
  return { ...session };
}

export function isSameSession(before: LiveSession, after: LiveSession): boolean {
  return before.sessionId === after.sessionId;
}
