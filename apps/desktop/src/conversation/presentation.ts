/**
 * How conversations are presented (APP-003 shell, APP-004 conversation): the sidebar row a
 * thread becomes and the time dividers a transcript shows. Pure: every input is a projection
 * the renderer already read (the thread list, the teammate roster, a thread's messages), and
 * nothing here decides anything the server owns — attention, authorship and order come from
 * the projections as they are.
 */

import type { ConversationMessage, RunActivity } from "./state.js";

/** A thread as `GET /v1/threads` lists it. */
export interface ThreadSummary {
  readonly id: string;
  readonly title: string | null;
  readonly kind: string;
  readonly messageCount: number;
  readonly lastActivityAt: string | null;
  /** The server's attention projection: a mention or an open question waits on this person. */
  readonly needsYou: boolean;
}

/** A teammate as the sidebar needs it: who it is and the AgentThread its replies come from. */
export interface RosterEntry {
  readonly id: string;
  readonly name: string;
  readonly agentThreadId: string | null;
}

/** A small round identity mark: a letter on a tone chosen from the identity. */
export interface Face {
  readonly key: string;
  readonly letter: string;
  readonly tone: number;
}

export interface SidebarRow {
  readonly id: string;
  /** The thread's title, else the teammate it is with, else how it began. */
  readonly name: string | null;
  /** The latest message, on one line. */
  readonly preview: string | null;
  /** A teammate is waiting on this person (question or mention). */
  readonly waiting: boolean;
  /** Messages arrived since this person last had the thread open in this session. */
  readonly unread: boolean;
  /** Who is in the thread, most recent teammate first (at most three). */
  readonly faces: readonly Face[];
  /** Teammates beyond the faces shown. */
  readonly more: number;
  /** The teammate replies come from, when there is one: new messages go to them. */
  readonly teammateId: string | null;
}

/** Tones for faces; the stylesheet names each `tone-N`. */
export const FACE_TONES = 8;

const MAX_FACES = 3;
const PREVIEW_LENGTH = 140;

/** A stable tone for an identity, so a teammate keeps its colour everywhere and every session. */
export function faceTone(id: string): number {
  let hash = 0;
  for (const char of id) {
    hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  }
  return hash % FACE_TONES;
}

/** The letter a face shows: the first letter or digit of a name, upper-cased. */
export function faceLetter(name: string): string {
  const match = /[\p{L}\p{N}]/u.exec(name);
  return match ? match[0].toUpperCase() : "?";
}

/** Initials for a person: first and last word of a display name, else the email's first letter. */
export function initials(displayName: string | null, email: string | null): string {
  const words = (displayName ?? "").trim().split(/\s+/).filter(Boolean);
  if (words.length > 0) {
    const first = faceLetter(words[0] ?? "");
    const last = words.length > 1 ? faceLetter(words[words.length - 1] ?? "") : "";
    return `${first}${last}`;
  }
  return email ? faceLetter(email) : "?";
}

function oneLine(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > PREVIEW_LENGTH ? `${flat.slice(0, PREVIEW_LENGTH - 1)}…` : flat;
}

/** The roster indexed by the AgentThread its teammates reply from. */
export function byAgentThread(roster: readonly RosterEntry[]): ReadonlyMap<string, RosterEntry> {
  const index = new Map<string, RosterEntry>();
  for (const entry of roster) {
    if (entry.agentThreadId) {
      index.set(entry.agentThreadId, entry);
    }
  }
  return index;
}

/**
 * The sidebar row for a thread. `messages` is the thread's last-read projection (null when it
 * has not been read yet); `seen` is how many messages the person had seen when they last had
 * the thread open (undefined when they have not opened it this session).
 */
export function sidebarRow(
  thread: ThreadSummary,
  messages: readonly ConversationMessage[] | null,
  roster: ReadonlyMap<string, RosterEntry>,
  seen: number | undefined,
): SidebarRow {
  const teammates: RosterEntry[] = [];
  for (const message of [...(messages ?? [])].reverse()) {
    const teammate = message.authorKind === "agent" ? roster.get(message.authorId) : undefined;
    if (teammate && !teammates.some((known) => known.id === teammate.id)) {
      teammates.push(teammate);
    }
  }
  const latest = messages && messages.length > 0 ? messages[messages.length - 1] : undefined;
  const firstFromPerson = messages?.find((message) => message.authorKind === "user");
  const title = thread.title?.trim() ? thread.title.trim() : null;
  const name = title ?? teammates[0]?.name ?? (firstFromPerson ? oneLine(firstFromPerson.text) : null);
  return {
    id: thread.id,
    name,
    preview: latest ? oneLine(latest.text) : null,
    waiting: thread.needsYou,
    unread: seen !== undefined && thread.messageCount > seen,
    faces: teammates.slice(0, MAX_FACES).map((teammate) => ({ key: teammate.id, letter: faceLetter(teammate.name), tone: faceTone(teammate.id) })),
    more: Math.max(0, teammates.length - MAX_FACES),
    teammateId: teammates[0]?.id ?? null,
  };
}

/** A transcript entry: a message, preceded by a time divider when the conversation paused. */
export type TranscriptItem =
  | { readonly kind: "divider"; readonly key: string; readonly at: Date }
  | { readonly kind: "message"; readonly key: string; readonly message: ConversationMessage; readonly continues: boolean };

/** A pause this long (or a new calendar day) starts a new stretch, marked with its time. */
export const DIVIDER_GAP_MS = 15 * 60 * 1000;

function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

function timeOf(message: ConversationMessage): Date | null {
  if (!message.createdAt) {
    return null;
  }
  const at = new Date(message.createdAt);
  return Number.isNaN(at.getTime()) ? null : at;
}

/**
 * The transcript as shown: a divider before the first timed message, after every pause of at
 * least {@link DIVIDER_GAP_MS} and when the day changes; `continues` marks a message from the same author as the one
 * before it with no divider between, so the two sit closer together.
 */
export function transcript(messages: readonly ConversationMessage[]): readonly TranscriptItem[] {
  const items: TranscriptItem[] = [];
  let previous: ConversationMessage | null = null;
  let previousAt: Date | null = null;
  for (const message of messages) {
    const at = timeOf(message);
    const divided = at !== null && (previousAt === null || at.getTime() - previousAt.getTime() >= DIVIDER_GAP_MS || !sameDay(at, previousAt));
    if (divided) {
      items.push({ kind: "divider", key: `at-${message.id}`, at });
    }
    const continues = !divided && previous !== null && previous.authorKind === message.authorKind && previous.authorId === message.authorId;
    items.push({ kind: "message", key: message.id, message, continues });
    previous = message;
    previousAt = at ?? previousAt;
  }
  return items;
}

/** Which day a divider names, relative to `now`: today, yesterday, this week, or a date. */
export function dividerDay(at: Date, now: Date): "today" | "yesterday" | "week" | "date" {
  const day = (value: Date): number => new Date(value.getFullYear(), value.getMonth(), value.getDate()).getTime();
  const days = Math.round((day(now) - day(at)) / 86_400_000);
  if (days <= 0) {
    return "today";
  }
  if (days === 1) {
    return "yesterday";
  }
  return days < 7 ? "week" : "date";
}

const STEP_DONE: ReadonlySet<string> = new Set(["completed", "succeeded", "failed", "cancelled", "denied", "skipped"]);

/**
 * Whether a run's turn is in flight — what the "working" row and the stop control show. A
 * conversation's run stays open between turns, so an open run alone is not work: a turn is in
 * flight while it streams, names a stage or has an unfinished step, or while the person's
 * message is the thread's latest and no reply has landed.
 */
export function turnInFlight(run: RunActivity, messages: readonly ConversationMessage[]): boolean {
  if (run.status === "WAITING_QUESTION") {
    return false;
  }
  if (run.streaming || run.stage || run.cards.some((card) => !STEP_DONE.has(card.status.toLowerCase()))) {
    return true;
  }
  const latest = messages[messages.length - 1];
  return latest === undefined || latest.authorKind === "user";
}
