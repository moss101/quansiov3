/**
 * Collaboration on a canonical thread (APP-009, DOMAIN.md §3.1): the client half.
 *
 * The server decides who is in a thread, who may read it and who may manage it; this
 * module decodes what it said (`GET /v1/threads/{id}`), keeps a thread's messages in the
 * one order the server gave them, applies the durable `thread.*` events to that view, and
 * builds the commands the page sends. Two rules the page relies on:
 *
 * * **order is `seq`, identity is `id`.** Messages arrive from a projection read and from
 *   the stream, possibly both, possibly out of order; `mergeMessages` keeps exactly one copy
 *   of each id sorted by `seq`, so two clients show the same conversation.
 * * **membership is the server's.** A removal event for the viewer ends the view: the page
 *   drops the messages it holds rather than waiting for a refusal.
 */

export type ParticipantKind = "user" | "teammate" | "worker";
export type ParticipantRole = "owner" | "member";

export interface ParticipantView {
  readonly kind: ParticipantKind;
  readonly id: string;
  readonly role: ParticipantRole;
  readonly joinedAt: string | null;
  readonly leftAt: string | null;
  readonly active: boolean;
}

export interface AttentionView {
  readonly kind: ParticipantKind;
  readonly id: string;
  readonly mentionsPending: number;
  readonly openQuestions: number;
  readonly agentStatus: string | null;
  readonly run: { readonly id: string; readonly status: string } | null;
  readonly needsHuman: boolean;
}

export interface ThreadView {
  readonly id: string;
  readonly workspaceId: string;
  readonly kind: "direct" | "group" | "objective";
  readonly title: string | null;
  readonly protected: boolean;
  readonly participants: readonly ParticipantView[];
  readonly attention: readonly AttentionView[];
  readonly responsibleTeammateId: string | null;
  readonly myRole: ParticipantRole | null;
}

export interface Reaction {
  readonly emoji: string;
  readonly userId: string;
}

export interface Mention {
  readonly kind: "user" | "teammate";
  readonly id: string;
}

export interface ThreadMessage {
  readonly id: string;
  readonly threadId: string;
  readonly seq: number;
  readonly authorKind: "user" | "agent" | "system";
  readonly authorId: string;
  readonly text: string;
  readonly notice: Record<string, unknown> | null;
  readonly mentions: readonly Mention[];
  readonly reactions: readonly Reaction[];
  readonly runId: string | null;
}

export interface ThreadListEntry {
  readonly id: string;
  readonly kind: string;
  readonly title: string | null;
  readonly protected: boolean;
  readonly messageCount: number;
  readonly mentionsPending: number;
  readonly openQuestions: number;
  readonly needsYou: boolean;
}

function record(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${what} is not an object`);
  }
  return value as Record<string, unknown>;
}

function str(source: Record<string, unknown>, key: string): string | null {
  const value = source[key];
  return typeof value === "string" ? value : null;
}

function num(source: Record<string, unknown>, key: string): number {
  const value = source[key];
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function kindOf(value: unknown): ParticipantKind {
  return value === "teammate" || value === "worker" ? value : "user";
}

function mentionOf(value: unknown): Mention | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const source = value as Record<string, unknown>;
  const id = str(source, "id");
  const kind = source["kind"];
  if (!id || (kind !== "user" && kind !== "teammate")) {
    return null;
  }
  return { kind, id };
}

function reactionOf(value: unknown): Reaction | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const source = value as Record<string, unknown>;
  const emoji = str(source, "emoji");
  const userId = str(source, "user_id");
  return emoji && userId ? { emoji, userId } : null;
}

/** Decode `GET /v1/threads/{id}`. */
export function decodeThreadView(raw: unknown): ThreadView {
  const view = record(raw, "thread");
  const participants = Array.isArray(view["participants"]) ? view["participants"] : [];
  const attention = Array.isArray(view["attention"]) ? view["attention"] : [];
  const kind = view["kind"];
  const myRole = view["my_role"];
  return {
    id: str(view, "id") ?? "",
    workspaceId: str(view, "workspace_id") ?? "",
    kind: kind === "group" || kind === "objective" ? kind : "direct",
    title: str(view, "title"),
    protected: view["protected"] === true,
    participants: participants.map((entry) => {
      const source = record(entry, "participant");
      return {
        kind: kindOf(source["kind"]),
        id: str(source, "id") ?? "",
        role: source["role"] === "owner" ? "owner" : "member",
        joinedAt: str(source, "joined_at"),
        leftAt: str(source, "left_at"),
        active: source["active"] === true,
      };
    }),
    attention: attention.map((entry) => {
      const source = record(entry, "attention");
      const run = source["run"] && typeof source["run"] === "object" ? (source["run"] as Record<string, unknown>) : null;
      return {
        kind: kindOf(source["kind"]),
        id: str(source, "id") ?? "",
        mentionsPending: num(source, "mentions_pending"),
        openQuestions: num(source, "open_questions"),
        agentStatus: str(source, "agent_status"),
        run: run ? { id: str(run, "id") ?? "", status: str(run, "status") ?? "" } : null,
        needsHuman: source["needs_human"] === true,
      };
    }),
    responsibleTeammateId: str(view, "responsible_teammate_id"),
    myRole: myRole === "owner" || myRole === "member" ? myRole : null,
  };
}

/** Decode `GET /v1/messages?thread_id=` into thread messages, in `seq` order. */
export function decodeThreadMessages(raw: unknown): ThreadMessage[] {
  const source = record(raw, "messages");
  const items = Array.isArray(source["messages"]) ? source["messages"] : [];
  return mergeMessages(
    [],
    items.map((item) => {
      const message = record(item, "message");
      const blocks = Array.isArray(message["content_blocks"]) ? (message["content_blocks"] as unknown[]) : [];
      const first = blocks[0] && typeof blocks[0] === "object" ? (blocks[0] as Record<string, unknown>) : {};
      const notice = first["type"] === "system_notice" && first["notice"] && typeof first["notice"] === "object" ? (first["notice"] as Record<string, unknown>) : null;
      const authorKind = message["author_kind"];
      return {
        id: str(message, "id") ?? "",
        threadId: str(message, "thread_id") ?? "",
        seq: num(message, "seq"),
        authorKind: authorKind === "agent" || authorKind === "system" ? authorKind : "user",
        authorId: str(message, "author_id") ?? "",
        text: blocks
          .map((block) => (block && typeof block === "object" && typeof (block as Record<string, unknown>)["text"] === "string" ? ((block as Record<string, unknown>)["text"] as string) : ""))
          .join("\n"),
        notice,
        mentions: (Array.isArray(message["mentions"]) ? message["mentions"] : []).map(mentionOf).filter((mention): mention is Mention => mention !== null),
        reactions: (Array.isArray(message["reactions"]) ? message["reactions"] : []).map(reactionOf).filter((reaction): reaction is Reaction => reaction !== null),
        runId: str(message, "run_id"),
      };
    }),
  );
}

/** Decode `GET /v1/threads?workspace_id=`. */
export function decodeThreadList(raw: unknown): ThreadListEntry[] {
  const source = record(raw, "threads");
  const items = Array.isArray(source["threads"]) ? source["threads"] : [];
  return items.map((item) => {
    const thread = record(item, "thread");
    const attention = thread["attention"] && typeof thread["attention"] === "object" ? (thread["attention"] as Record<string, unknown>) : {};
    return {
      id: str(thread, "id") ?? "",
      kind: str(thread, "kind") ?? "direct",
      title: str(thread, "title"),
      protected: thread["protected"] === true,
      messageCount: num(thread, "message_count"),
      mentionsPending: num(attention, "mentions_pending"),
      openQuestions: num(attention, "open_questions"),
      needsYou: attention["needs_you"] === true,
    };
  });
}

/**
 * One copy of each message, in `seq` order. A later copy of the same id replaces the
 * earlier one (a projection read is fresher than a stream echo); `id` breaks a `seq` tie so
 * the order is total even if two copies disagree while a read is in flight.
 */
export function mergeMessages(current: readonly ThreadMessage[], incoming: readonly ThreadMessage[]): ThreadMessage[] {
  const byId = new Map<string, ThreadMessage>();
  for (const message of [...current, ...incoming]) {
    byId.set(message.id, message);
  }
  return [...byId.values()].sort((a, b) => a.seq - b.seq || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

/** Whether a `seq` sequence has a hole the page should fill by re-reading. */
export function hasGap(messages: readonly ThreadMessage[]): boolean {
  return messages.some((message, index) => message.seq !== index + 1);
}

/** Who is looking, from `GET /v1/me`: their user id and whether they administer the workspace. */
export function viewerFrom(me: unknown, workspaceId: string): { readonly userId: string | null; readonly workspaceAdmin: boolean } {
  const source = me && typeof me === "object" ? (me as Record<string, unknown>) : {};
  const user = source["user"] && typeof source["user"] === "object" ? (source["user"] as Record<string, unknown>) : {};
  const workspaces = Array.isArray(source["workspaces"]) ? (source["workspaces"] as unknown[]) : [];
  const workspace = workspaces.find((entry) => entry && typeof entry === "object" && (entry as Record<string, unknown>)["id"] === workspaceId) as Record<string, unknown> | undefined;
  return { userId: typeof user["id"] === "string" ? user["id"] : null, workspaceAdmin: workspace?.["role"] === "admin" };
}

// ---- membership and permissions (what to offer; the server decides) ----

export function activeParticipants(view: ThreadView): readonly ParticipantView[] {
  return view.participants.filter((participant) => participant.active);
}

/** Whether the viewer may manage others' membership: an owner of the thread or a workspace admin. */
export function mayManage(view: ThreadView, workspaceAdmin: boolean): boolean {
  return view.myRole === "owner" || workspaceAdmin;
}

/** Whether the viewer may leave: the last owner may not leave people behind. */
export function mayLeave(view: ThreadView, me: string): boolean {
  const mine = view.participants.find((participant) => participant.active && participant.kind === "user" && participant.id === me);
  if (!mine) {
    return false;
  }
  if (mine.role !== "owner") {
    return true;
  }
  const others = activeParticipants(view).filter((participant) => participant.kind === "user" && participant.id !== me);
  return others.length === 0 || others.some((participant) => participant.role === "owner");
}

/** The viewer's attention in the thread, if they are in it. */
export function attentionFor(view: ThreadView, kind: ParticipantKind, id: string): AttentionView | null {
  return view.attention.find((entry) => entry.kind === kind && entry.id === id) ?? null;
}

/** Teammates the thread can be handed to: active, available, not already responsible. */
export function handoffTargets<T extends { readonly id: string; readonly status: string; readonly availability: string }>(view: ThreadView, roster: readonly T[]): readonly T[] {
  if (!view.responsibleTeammateId) {
    return [];
  }
  return roster.filter((teammate) => teammate.status === "active" && teammate.availability === "available" && teammate.id !== view.responsibleTeammateId);
}

// ---- reactions ----

export interface ReactionSummary {
  readonly emoji: string;
  readonly count: number;
  readonly mine: boolean;
}

/** Reactions grouped by emoji, in first-reaction order, with whether the viewer is among them. */
export function summarizeReactions(reactions: readonly Reaction[], me: string | null): ReactionSummary[] {
  const order: string[] = [];
  const counts = new Map<string, { count: number; mine: boolean }>();
  for (const reaction of reactions) {
    const current = counts.get(reaction.emoji);
    if (!current) {
      order.push(reaction.emoji);
      counts.set(reaction.emoji, { count: 1, mine: reaction.userId === me });
    } else {
      counts.set(reaction.emoji, { count: current.count + 1, mine: current.mine || reaction.userId === me });
    }
  }
  return order.map((emoji) => ({ emoji, count: counts.get(emoji)?.count ?? 0, mine: counts.get(emoji)?.mine ?? false }));
}

/** The `ReactToMessage` params a click means: add when the viewer has not reacted, else remove. */
export function reactionToggle(message: ThreadMessage, me: string, emoji: string): { readonly message_id: string; readonly emoji: string; readonly remove: boolean } {
  const mine = message.reactions.some((reaction) => reaction.emoji === emoji && reaction.userId === me);
  return { message_id: message.id, emoji, remove: mine };
}

// ---- mentions ----

export interface Mentionable {
  readonly kind: "user" | "teammate";
  readonly id: string;
  readonly handle: string;
}

/**
 * The mentions a composer text makes: every `@handle` that names someone mentionable
 * (case-insensitive, whole handle), each once, in order. An `@word` that names nobody is
 * left as text — the page never invents a principal.
 */
export function mentionsIn(text: string, people: readonly Mentionable[]): Mention[] {
  const found: Mention[] = [];
  for (const match of text.matchAll(/(^|[\s(])@([\p{L}\p{N}_.-]+)/gu)) {
    const handle = (match[2] ?? "").toLowerCase();
    const who = people.find((person) => person.handle.toLowerCase() === handle);
    if (who && !found.some((mention) => mention.kind === who.kind && mention.id === who.id)) {
      found.push({ kind: who.kind, id: who.id });
    }
  }
  return found;
}

/** Who a partial `@pre` could complete to. */
export function mentionCandidates(prefix: string, people: readonly Mentionable[]): readonly Mentionable[] {
  const wanted = prefix.replace(/^@/, "").toLowerCase();
  return people.filter((person) => person.handle.toLowerCase().startsWith(wanted));
}

// ---- durable events ----

export interface ThreadState {
  readonly view: ThreadView;
  readonly messages: readonly ThreadMessage[];
  /** The viewer was removed: the page shows nothing more of this thread. */
  readonly revoked: boolean;
  /** A `thread.message_posted` arrived whose body the page has not read yet. */
  readonly stale: boolean;
}

function principalOf(payload: Record<string, unknown>): { kind: ParticipantKind; id: string } | null {
  const principal = payload["principal"];
  if (!principal || typeof principal !== "object") {
    return null;
  }
  const source = principal as Record<string, unknown>;
  const id = str(source, "id");
  return id ? { kind: kindOf(source["kind"]), id } : null;
}

/**
 * Apply one durable event to a thread's state. Events for other threads change nothing.
 * A reaction updates the message it names; a message post marks the state stale (the body
 * is read from the projection); a membership change updates the participant list, and the
 * viewer's own removal revokes the view.
 */
export function applyThreadEvent(state: ThreadState, event: Record<string, unknown>, me: string | null): ThreadState {
  const type = typeof event["type"] === "string" ? event["type"] : "";
  const payload = event["payload"] && typeof event["payload"] === "object" ? (event["payload"] as Record<string, unknown>) : {};
  const threadId = event["aggregate_type"] === "thread" ? event["aggregate_id"] : payload["thread_id"];
  if (threadId !== state.view.id || !type.startsWith("thread.")) {
    return state;
  }
  if (type === "thread.message_posted") {
    const known = state.messages.some((message) => message.id === payload["message_id"]);
    return known ? state : { ...state, stale: true };
  }
  if (type === "thread.reaction_added" || type === "thread.reaction_removed") {
    const emoji = str(payload, "emoji");
    const userId = str(payload, "user_id");
    if (!emoji || !userId) {
      return state;
    }
    return {
      ...state,
      messages: state.messages.map((message) => {
        if (message.id !== payload["message_id"]) {
          return message;
        }
        const others = message.reactions.filter((reaction) => !(reaction.emoji === emoji && reaction.userId === userId));
        return { ...message, reactions: type === "thread.reaction_added" ? [...others, { emoji, userId }] : others };
      }),
    };
  }
  if (type === "thread.participant_added" || type === "thread.participant_removed") {
    const principal = principalOf(payload);
    if (!principal) {
      return state;
    }
    const added = type === "thread.participant_added";
    const role: ParticipantRole = payload["role"] === "owner" ? "owner" : "member";
    const existing = state.view.participants.find((participant) => participant.kind === principal.kind && participant.id === principal.id);
    const updated: ParticipantView = {
      kind: principal.kind,
      id: principal.id,
      role,
      joinedAt: existing?.joinedAt ?? null,
      leftAt: added ? null : new Date().toISOString(),
      active: added,
    };
    const participants = existing
      ? state.view.participants.map((participant) => (participant === existing ? updated : participant))
      : [...state.view.participants, updated];
    const revoked = !added && principal.kind === "user" && principal.id === me;
    return {
      ...state,
      view: { ...state.view, participants, myRole: principal.kind === "user" && principal.id === me ? (added ? role : null) : state.view.myRole },
      messages: revoked ? [] : state.messages,
      revoked: revoked || (added && principal.kind === "user" && principal.id === me ? false : state.revoked),
    };
  }
  if (type === "thread.handed_off") {
    const to = payload["to"] && typeof payload["to"] === "object" ? str(payload["to"] as Record<string, unknown>, "id") : null;
    return to ? { ...state, view: { ...state.view, responsibleTeammateId: to }, stale: true } : state;
  }
  if (type === "thread.attention_changed") {
    return { ...state, stale: true };
  }
  return state;
}

// ---- commands (names and params the server validates again) ----

export function createThreadParams(workspaceId: string, kind: "group" | "direct", title: string, participants: readonly { kind: "user" | "teammate"; id: string; role?: ParticipantRole }[]): Record<string, unknown> {
  const trimmed = title.trim();
  if (kind === "group" && !trimmed) {
    throw new Error("a group thread needs a title");
  }
  for (const participant of participants) {
    const prefix = participant.kind === "user" ? "usr_" : "agt_";
    if (!participant.id.startsWith(prefix)) {
      throw new Error(`a ${participant.kind} participant is a ${prefix} id`);
    }
  }
  return { workspace_id: workspaceId, kind, title: trimmed || null, participants: participants.map((participant) => ({ kind: participant.kind, id: participant.id, role: participant.role ?? "member" })) };
}

export function participantParams(threadId: string, kind: "user" | "teammate", id: string, role?: ParticipantRole): Record<string, unknown> {
  if (!threadId.startsWith("thr_")) {
    throw new Error("a participant change names a thr_ thread");
  }
  const prefix = kind === "user" ? "usr_" : "agt_";
  if (!id.startsWith(prefix)) {
    throw new Error(`a ${kind} participant is a ${prefix} id`);
  }
  return role ? { thread_id: threadId, kind, id, role } : { thread_id: threadId, kind, id };
}

export function handOffParams(threadId: string, toTeammateId: string): Record<string, unknown> {
  if (!threadId.startsWith("thr_") || !toTeammateId.startsWith("agt_")) {
    throw new Error("a handoff names a thr_ thread and an agt_ teammate");
  }
  return { thread_id: threadId, to_teammate_id: toTeammateId };
}
