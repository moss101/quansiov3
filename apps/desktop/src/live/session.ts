/**
 * Live session state (APP-007, DOMAIN.md §8.4–§8.6).
 *
 * The desktop observes and takes over the same session the agent uses. Everything here is
 * a view of the server's `GET /v1/browser-sessions/{id}` projection and of the
 * `browser.control_changed` / `target.control_changed` / `run.*` events on the stream; the
 * renderer holds no authority of its own. Three rules the page relies on:
 *
 * * **the session id never changes hands.** A takeover or handback is applied to the view
 *   only when the event names this session; an event for another session is ignored, so a
 *   replacement session can never appear behind the user's back (acceptance 1).
 * * **the fence is the server's.** `agentInputAllowed` is what the projection's
 *   `automation.agent_input_fenced` said, or what the last control event implies; the page
 *   shows it and offers the one control that is legal from here (acceptance 2).
 * * **a policy pause is not a takeover.** `paused_policy` offers no handback: only the
 *   server's policy decision lifts it.
 */

export type Controller = "agent" | "user" | "none";
export type SessionStatus = "active" | "paused_takeover" | "paused_policy" | "closed";
export type PauseReason = "user_control" | "policy" | "closed";

export interface Tab {
  readonly targetId: string;
  readonly url: string;
  readonly title: string;
  /** The opaque revision of the document the tab shows (ADP-001): it changes when the page does. */
  readonly documentRevision: string | null;
}

/**
 * What the hardened browser driver reported about the session (ADP-001, DOMAIN.md §8.4.1 E). Every
 * string a page produced — a dialog's message, a download's name — is untrusted: the page renders it as
 * text, never as markup or as an instruction.
 */
export interface GuardDialogView {
  readonly id: string;
  readonly kind: string;
  readonly message: string;
  readonly url: string;
}

export interface GuardCertificateView {
  readonly id: string;
  readonly origin: string;
  readonly errorType: string;
  readonly status: "pending" | "continued" | "cancelled" | "expired";
  readonly certificateSha256: string | null;
  readonly decidedBy: string | null;
}

export interface GuardDownloadView {
  readonly id: string;
  readonly suggestedFilename: string;
  readonly state: "in_progress" | "completed" | "cancelled";
  readonly receivedBytes: number;
  readonly failure: string | null;
}

export interface GuardTabView {
  readonly tabId: string;
  readonly url: string;
  readonly documentRevision: string;
  readonly dialog: GuardDialogView | null;
  readonly certificates: readonly GuardCertificateView[];
  readonly downloads: readonly GuardDownloadView[];
  readonly popupsBlocked: number;
}

export interface InDoubtView {
  readonly callId: string;
  readonly actionId: string;
  readonly reason: string;
}

export interface GuardView {
  readonly reportedAt: string | null;
  readonly tabs: readonly GuardTabView[];
  readonly inDoubt: readonly InDoubtView[];
}

/** Something on this session that is waiting for a person rather than for the agent. */
export type AttentionKind = "dialog" | "certificate" | "in_doubt";

export interface Attention {
  readonly kind: AttentionKind;
  readonly id: string;
  readonly detail: string;
}

export interface TerminalSessionView {
  readonly id: string;
  readonly cursor: number;
  readonly status: "open" | "closed" | "lost";
  readonly lastCommandId: string | null;
  readonly ptyRef: string | null;
}

export interface ComputerControlView {
  readonly targetId: string;
  readonly holder: Controller;
  readonly generation: number;
  readonly takeoverPending: boolean;
  readonly activeToolCallId: string | null;
  readonly agentMayInput: boolean;
}

export interface RunView {
  readonly id: string;
  readonly status: string;
  readonly generation: number;
}

export interface LiveSession {
  readonly sessionId: string;
  readonly targetId: string;
  readonly runId: string | null;
  readonly controller: Controller;
  readonly controlSince: string | null;
  readonly status: SessionStatus;
  readonly currentUrl: string | null;
  readonly tabs: readonly Tab[];
  readonly screencastEnabled: boolean;
  readonly agentMayDrive: boolean;
  readonly run: RunView | null;
  readonly terminals: readonly TerminalSessionView[];
  readonly computerControl: ComputerControlView | null;
  readonly automation: {
    readonly paused: boolean;
    readonly reason: PauseReason | null;
    readonly agentInputFenced: boolean;
  };
  readonly guard: GuardView;
}

export interface TargetView {
  readonly id: string;
  readonly workspaceId: string;
  readonly substrate: string;
  readonly status: string;
  readonly browserSessions: readonly { readonly id: string; readonly status: SessionStatus; readonly controller: Controller; readonly currentUrl: string | null }[];
  readonly terminals: readonly TerminalSessionView[];
  readonly computerControl: ComputerControlView | null;
}

function record(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${what} is not an object`);
  }
  return value as Record<string, unknown>;
}

function text(source: Record<string, unknown>, key: string): string | null {
  const value = source[key];
  return typeof value === "string" ? value : null;
}

function requireText(source: Record<string, unknown>, key: string, what: string): string {
  const value = text(source, key);
  if (value === null) {
    throw new Error(`${what}.${key} is missing`);
  }
  return value;
}

function controller(value: unknown): Controller {
  return value === "user" || value === "none" ? value : "agent";
}

function sessionStatus(value: unknown): SessionStatus {
  return value === "paused_takeover" || value === "paused_policy" || value === "closed" ? value : "active";
}

function pauseReason(value: unknown): PauseReason | null {
  return value === "user_control" || value === "policy" || value === "closed" ? value : null;
}

function cursorOf(value: unknown): number {
  const cursor = typeof value === "number" ? value : typeof value === "string" ? Number(value) : 0;
  return Number.isFinite(cursor) && cursor >= 0 ? Math.floor(cursor) : 0;
}

export function decodeTerminal(raw: unknown): TerminalSessionView {
  const source = record(raw, "terminal session");
  const status = source["status"];
  return {
    id: requireText(source, "id", "terminal session"),
    cursor: cursorOf(source["cursor"]),
    status: status === "closed" || status === "lost" ? status : "open",
    lastCommandId: text(source, "last_command_id"),
    ptyRef: text(source, "pty_ref"),
  };
}

export function decodeComputerControl(raw: unknown): ComputerControlView | null {
  if (raw === null || raw === undefined) {
    return null;
  }
  const source = record(raw, "computer control");
  return {
    targetId: requireText(source, "target_id", "computer control"),
    holder: controller(source["holder"]),
    generation: typeof source["generation"] === "number" ? source["generation"] : 1,
    takeoverPending: source["takeover_pending"] === true,
    activeToolCallId: text(source, "active_tool_call_id"),
    agentMayInput: source["agent_may_input"] === true,
  };
}

function certificateStatus(value: unknown): GuardCertificateView["status"] {
  return value === "continued" || value === "cancelled" || value === "expired" ? value : "pending";
}

function downloadState(value: unknown): GuardDownloadView["state"] {
  return value === "completed" || value === "cancelled" ? value : "in_progress";
}

/** Decode the `guard` object of the session projection. A missing or malformed part reads as empty. */
export function decodeGuard(raw: unknown): GuardView {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return { reportedAt: null, tabs: [], inDoubt: [] };
  }
  const source = raw as Record<string, unknown>;
  const list = (key: string): unknown[] => (Array.isArray(source[key]) ? (source[key] as unknown[]) : []);
  const object = (value: unknown): Record<string, unknown> => (value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {});
  return {
    reportedAt: text(source, "reported_at"),
    tabs: list("tabs").map((entry) => {
      const tab = object(entry);
      const dialog = tab["dialog"] && typeof tab["dialog"] === "object" ? object(tab["dialog"]) : null;
      const items = (key: string): unknown[] => (Array.isArray(tab[key]) ? (tab[key] as unknown[]) : []);
      return {
        tabId: text(tab, "tab_id") ?? "",
        url: text(tab, "url") ?? "",
        documentRevision: text(tab, "document_revision") ?? "",
        dialog: dialog ? { id: text(dialog, "id") ?? "", kind: text(dialog, "kind") ?? "", message: text(dialog, "message") ?? "", url: text(dialog, "url") ?? "" } : null,
        certificates: items("certificates").map((item) => {
          const certificate = object(item);
          return {
            id: text(certificate, "id") ?? "",
            origin: text(certificate, "origin") ?? "",
            errorType: text(certificate, "error_type") ?? "",
            status: certificateStatus(certificate["status"]),
            certificateSha256: text(certificate, "certificate_sha256"),
            decidedBy: text(certificate, "decided_by"),
          };
        }),
        downloads: items("downloads").map((item) => {
          const download = object(item);
          return {
            id: text(download, "id") ?? "",
            suggestedFilename: text(download, "suggested_filename") ?? "",
            state: downloadState(download["state"]),
            receivedBytes: cursorOf(download["received_bytes"]),
            failure: text(download, "failure"),
          };
        }),
        popupsBlocked: cursorOf(tab["popups_blocked"]),
      };
    }),
    inDoubt: list("in_doubt").map((entry) => {
      const item = object(entry);
      return { callId: text(item, "call_id") ?? "", actionId: text(item, "action_id") ?? "", reason: text(item, "reason") ?? "" };
    }),
  };
}

/**
 * What on this session waits for a person: a dialog the page raised, a certificate error only the user
 * can decide, and browser actions whose outcome is in doubt. The page shows them; it does not decide
 * them, and none of them can be dismissed by the agent.
 */
export function attentionNeeded(session: LiveSession): readonly Attention[] {
  const items: Attention[] = [];
  for (const tab of session.guard.tabs) {
    if (tab.dialog) {
      items.push({ kind: "dialog", id: tab.dialog.id, detail: `${tab.dialog.kind}: ${tab.dialog.message}` });
    }
    for (const certificate of tab.certificates) {
      if (certificate.status === "pending") {
        items.push({ kind: "certificate", id: certificate.id, detail: `${certificate.origin} ${certificate.errorType}` });
      }
    }
  }
  for (const doubt of session.guard.inDoubt) {
    items.push({ kind: "in_doubt", id: doubt.actionId, detail: doubt.reason });
  }
  return items;
}

/** Decode `GET /v1/browser-sessions/{id}`. */
export function decodeLiveSession(raw: unknown): LiveSession {
  const view = record(raw, "live session");
  const session = record(view["session"], "live session.session");
  const automation = record(view["automation"], "live session.automation");
  const tabs = Array.isArray(session["tabs"]) ? session["tabs"] : [];
  const terminals = Array.isArray(view["terminal_sessions"]) ? view["terminal_sessions"] : [];
  const run = view["run"] && typeof view["run"] === "object" ? record(view["run"], "live session.run") : null;
  const screencast = session["screencast"] && typeof session["screencast"] === "object" ? (session["screencast"] as Record<string, unknown>) : {};
  return {
    sessionId: requireText(session, "id", "session"),
    targetId: requireText(session, "target_id", "session"),
    runId: text(session, "run_id"),
    controller: controller(session["control_holder"]),
    controlSince: text(session, "control_since"),
    status: sessionStatus(session["status"]),
    currentUrl: text(session, "current_url"),
    tabs: tabs.map((tab) => {
      const source = record(tab, "tab");
      return {
        targetId: text(source, "target_id") ?? "",
        url: text(source, "url") ?? "",
        title: text(source, "title") ?? "",
        documentRevision: text(source, "document_revision"),
      };
    }),
    screencastEnabled: screencast["enabled"] === true,
    agentMayDrive: session["agent_may_drive"] === true,
    run: run
      ? {
          id: requireText(run, "id", "run"),
          status: requireText(run, "status", "run"),
          generation: typeof run["generation"] === "number" ? run["generation"] : 1,
        }
      : null,
    terminals: terminals.map(decodeTerminal),
    computerControl: decodeComputerControl(view["computer_control"]),
    automation: {
      paused: automation["paused"] === true,
      reason: pauseReason(automation["reason"]),
      agentInputFenced: automation["agent_input_fenced"] === true,
    },
    guard: decodeGuard(session["guard"]),
  };
}

/** Decode `GET /v1/targets`. */
export function decodeTargets(raw: unknown): TargetView[] {
  const source = record(raw, "targets");
  const items = Array.isArray(source["items"]) ? source["items"] : [];
  return items.map((item) => {
    const target = record(item, "target");
    const sessions = Array.isArray(target["browser_sessions"]) ? target["browser_sessions"] : [];
    const terminals = Array.isArray(target["terminal_sessions"]) ? target["terminal_sessions"] : [];
    return {
      id: requireText(target, "id", "target"),
      workspaceId: text(target, "workspace_id") ?? "",
      substrate: text(target, "substrate") ?? "",
      status: text(target, "status") ?? "",
      browserSessions: sessions.map((session) => {
        const row = record(session, "browser session");
        return {
          id: requireText(row, "id", "browser session"),
          status: sessionStatus(row["status"]),
          controller: controller(row["control_holder"]),
          currentUrl: text(row, "current_url"),
        };
      }),
      terminals: terminals.map(decodeTerminal),
      computerControl: decodeComputerControl(target["computer_control"]),
    };
  });
}

// ---- the rules the page applies ----

/** Whether the agent may drive this session: what the server said, nothing the page decides. */
export function agentInputAllowed(session: LiveSession): boolean {
  return !session.automation.agentInputFenced && session.agentMayDrive;
}

/** Whether a takeover is offered from here: only an active, agent-held session. */
export function takeoverAllowed(session: LiveSession): boolean {
  return session.status === "active" && session.controller === "agent";
}

/** Whether a handback is offered: only what the user holds (a policy pause is not theirs). */
export function handbackAllowed(session: LiveSession): boolean {
  return session.status === "paused_takeover" && session.controller === "user";
}

/** Why automation is paused, as the page names it. */
export function pauseReasonOf(session: LiveSession): PauseReason | null {
  return session.automation.reason;
}

export function isSameSession(before: LiveSession, after: LiveSession): boolean {
  return before.sessionId === after.sessionId;
}

/** `RequestTakeover {session_id}` — the command, the renderer sends it through the bridge. */
export function requestTakeover(sessionId: string): { readonly name: "RequestTakeover"; readonly params: { readonly session_id: string } } {
  if (!sessionId.startsWith("bsn_")) {
    throw new Error("a takeover names a bsn_ session");
  }
  return { name: "RequestTakeover", params: { session_id: sessionId } };
}

/** `Handback {session_id}`. */
export function handback(sessionId: string): { readonly name: "Handback"; readonly params: { readonly session_id: string } } {
  if (!sessionId.startsWith("bsn_")) {
    throw new Error("a handback names a bsn_ session");
  }
  return { name: "Handback", params: { session_id: sessionId } };
}

/** `RequestTakeover {target_id}` / `Handback {target_id}` for the native computer. */
export function computerCommand(kind: "RequestTakeover" | "Handback", targetId: string): { readonly name: "RequestTakeover" | "Handback"; readonly params: { readonly target_id: string } } {
  if (!targetId.startsWith("tgt_")) {
    throw new Error("a native-computer control command names a tgt_ target");
  }
  return { name: kind, params: { target_id: targetId } };
}

/**
 * Apply one durable event to the view. Only an event that names this session (or its run,
 * or its target's computer control) changes anything; anything else — including a
 * `browser.control_changed` for another session — returns the same view, which is how a
 * replacement session cannot slip in.
 */
export function applyEvent(session: LiveSession, event: Record<string, unknown>): LiveSession {
  const type = typeof event["type"] === "string" ? event["type"] : "";
  const payload = event["payload"] && typeof event["payload"] === "object" ? (event["payload"] as Record<string, unknown>) : {};
  if (type === "browser.control_changed") {
    if (payload["session_id"] !== session.sessionId) {
      return session;
    }
    const holder = controller(payload["holder"]);
    const status = sessionStatus(payload["status"]);
    const reason: PauseReason | null = status === "paused_takeover" ? "user_control" : status === "paused_policy" ? "policy" : status === "closed" ? "closed" : null;
    return {
      ...session,
      controller: holder,
      status,
      controlSince: text(payload, "since") ?? session.controlSince,
      agentMayDrive: status === "active" && holder === "agent",
      automation: { paused: reason !== null, reason, agentInputFenced: !(status === "active" && holder === "agent") },
    };
  }
  if (type === "target.control_changed") {
    if (payload["target_id"] !== session.targetId || !session.computerControl) {
      return session;
    }
    const holder = controller(payload["holder"]);
    const takeoverPending = payload["takeover_pending"] === true;
    return {
      ...session,
      computerControl: {
        ...session.computerControl,
        holder,
        generation: typeof payload["generation"] === "number" ? payload["generation"] : session.computerControl.generation,
        takeoverPending,
        agentMayInput: holder === "agent" && !takeoverPending && session.computerControl.activeToolCallId === null,
      },
    };
  }
  if (type.startsWith("run.") && session.run && payload["run_id"] === session.run.id) {
    const to = text(payload, "to");
    if (!to) {
      return session;
    }
    const generation = typeof payload["generation"] === "number" ? payload["generation"] : session.run.generation;
    return {
      ...session,
      run: { ...session.run, status: to, generation },
      automation: {
        ...session.automation,
        agentInputFenced: session.automation.agentInputFenced || to === "WAITING_TAKEOVER",
      },
    };
  }
  return session;
}
