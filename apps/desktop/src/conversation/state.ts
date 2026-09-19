/**
 * Conversation state (APP-004): one thread's messages, its runs' progress, the questions
 * the agent asked, and the composer — a pure reducer over durable events, live frames and
 * projection loads. Nothing here is authoritative: every identity is the server's (`thr_`,
 * `msg_`, `run_`, `trn_`, `q_`, `art_`), and reload rebuilds the same state from the same
 * projections. The renderer dispatches; the bridge fetches; this module decides what the
 * page shows.
 *
 * Durable events (`event` frames) change what is known; live frames (`model.delta`,
 * `progress`) only change what is shown while a turn runs and are dropped once the durable
 * message for that turn arrives. A frame for a run this thread does not own is ignored.
 */

import type { EventFrame, LiveFrame, StreamFrame } from "../client/stream.js";

export type AuthorKind = "user" | "agent" | "system";

export interface ConversationMessage {
  readonly id: string;
  readonly threadId: string;
  readonly seq: number;
  readonly authorKind: AuthorKind;
  readonly authorId: string;
  readonly runId: string | null;
  readonly turnId: string | null;
  readonly text: string;
  readonly attachments: readonly string[];
}

export type QuestionStatus = "open" | "answered" | "expired" | "cancelled";

export interface ConversationQuestion {
  readonly id: string;
  readonly runId: string;
  readonly kind: string;
  readonly prompt: string;
  readonly options: readonly string[];
  readonly required: boolean;
  readonly status: QuestionStatus;
  readonly answer: unknown;
}

/** A step of the current turn, as the runtime reports it (a tool call, a wait, a model call). */
export interface ActivityCard {
  readonly stepId: string;
  readonly turnId: string;
  readonly kind: string;
  readonly status: string;
  readonly ref: string | null;
}

export interface RunActivity {
  readonly runId: string;
  /** The runtime's own spelling (`RUNNING`, `WAITING_QUESTION`, `CANCELLED`, …). */
  readonly status: string;
  readonly turnId: string | null;
  /** Assistant text streamed for the current turn; cleared when its durable message lands. */
  readonly streaming: string;
  readonly stage: string | null;
  readonly cards: readonly ActivityCard[];
}

export interface ComposerAttachment {
  readonly artifactId: string;
  readonly title: string;
  readonly sizeBytes: number;
}

export interface ComposerState {
  readonly text: string;
  readonly attachments: readonly ComposerAttachment[];
  readonly posting: boolean;
  readonly error: string | null;
}

export interface ConversationState {
  readonly workspaceId: string;
  readonly threadId: string | null;
  readonly messages: readonly ConversationMessage[];
  readonly questions: readonly ConversationQuestion[];
  readonly runs: Readonly<Record<string, RunActivity>>;
  /** The run the composer's last message started, or the latest run that is not finished. */
  readonly activeRunId: string | null;
  /** Set by an event: the projection must be fetched again. */
  readonly stale: { readonly messages: boolean; readonly questions: boolean };
  readonly composer: ComposerState;
  readonly inspectedRunId: string | null;
}

export type ConversationAction =
  | { readonly type: "thread.selected"; readonly threadId: string | null }
  | { readonly type: "messages.loaded"; readonly threadId: string; readonly messages: readonly ConversationMessage[] }
  | { readonly type: "questions.loaded"; readonly questions: readonly ConversationQuestion[] }
  | { readonly type: "run.loaded"; readonly runId: string; readonly status: string }
  | { readonly type: "frame"; readonly frame: StreamFrame }
  | { readonly type: "composer.text"; readonly text: string }
  | { readonly type: "composer.attach"; readonly attachment: ComposerAttachment }
  | { readonly type: "composer.detach"; readonly artifactId: string }
  | { readonly type: "composer.posting" }
  | { readonly type: "composer.posted"; readonly threadId: string; readonly runId: string; readonly messageId: string }
  | { readonly type: "composer.failed"; readonly error: string }
  | { readonly type: "inspect"; readonly runId: string | null };

const TERMINAL_RUN_STATES: ReadonlySet<string> = new Set(["SUCCEEDED", "FAILED", "CANCELLED"]);

export function initialConversation(workspaceId: string, threadId: string | null = null): ConversationState {
  return {
    workspaceId,
    threadId,
    messages: [],
    questions: [],
    runs: {},
    activeRunId: null,
    stale: { messages: threadId !== null, questions: threadId !== null },
    composer: { text: "", attachments: [], posting: false, error: null },
    inspectedRunId: null,
  };
}

/** True when the run is one this thread owns (a message or question of the thread names it). */
export function ownsRun(state: ConversationState, runId: string): boolean {
  return (
    runId in state.runs ||
    state.activeRunId === runId ||
    state.messages.some((message) => message.runId === runId) ||
    state.questions.some((question) => question.runId === runId)
  );
}

export function conversationReducer(state: ConversationState, action: ConversationAction): ConversationState {
  switch (action.type) {
    case "thread.selected":
      if (action.threadId === state.threadId) {
        return state;
      }
      return { ...initialConversation(state.workspaceId, action.threadId), composer: { ...state.composer, error: null } };
    case "messages.loaded": {
      if (action.threadId !== state.threadId) {
        return state;
      }
      // Once a turn's durable message exists, its streamed preview is redundant.
      const runs: Record<string, RunActivity> = { ...state.runs };
      for (const message of action.messages) {
        if (message.authorKind === "agent" && message.runId && message.turnId) {
          const run = runs[message.runId];
          if (run && run.turnId === message.turnId && run.streaming) {
            runs[message.runId] = { ...run, streaming: "" };
          }
        }
      }
      return { ...state, messages: action.messages, runs, stale: { ...state.stale, messages: false } };
    }
    case "questions.loaded":
      return { ...state, questions: action.questions, stale: { ...state.stale, questions: false } };
    case "run.loaded":
      return { ...state, runs: { ...state.runs, [action.runId]: withStatus(state.runs[action.runId], action.runId, action.status) } };
    case "frame":
      return action.frame.kind === "event" ? applyEvent(state, action.frame) : applyLive(state, action.frame);
    case "composer.text":
      return { ...state, composer: { ...state.composer, text: action.text, error: null } };
    case "composer.attach":
      if (!action.attachment.artifactId.startsWith("art_")) {
        return { ...state, composer: { ...state.composer, error: "attachments must be artifacts" } };
      }
      if (state.composer.attachments.some((existing) => existing.artifactId === action.attachment.artifactId)) {
        return state;
      }
      return { ...state, composer: { ...state.composer, attachments: [...state.composer.attachments, action.attachment], error: null } };
    case "composer.detach":
      return {
        ...state,
        composer: { ...state.composer, attachments: state.composer.attachments.filter((existing) => existing.artifactId !== action.artifactId) },
      };
    case "composer.posting":
      return { ...state, composer: { ...state.composer, posting: true, error: null } };
    case "composer.posted": {
      const base = action.threadId === state.threadId ? state : { ...initialConversation(state.workspaceId, action.threadId) };
      return {
        ...base,
        activeRunId: action.runId,
        runs: { ...base.runs, [action.runId]: withStatus(base.runs[action.runId], action.runId, "RUNNING") },
        stale: { messages: true, questions: true },
        composer: { text: "", attachments: [], posting: false, error: null },
      };
    }
    case "composer.failed":
      return { ...state, composer: { ...state.composer, posting: false, error: action.error } };
    case "inspect":
      return { ...state, inspectedRunId: action.runId };
    default:
      return state;
  }
}

function withStatus(existing: RunActivity | undefined, runId: string, status: string): RunActivity {
  return existing ? { ...existing, status } : { runId, status, turnId: null, streaming: "", stage: null, cards: [] };
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function applyEvent(state: ConversationState, frame: EventFrame): ConversationState {
  const event = frame.event;
  const type = text(event["type"]) ?? text(event["event_type"]) ?? "";
  const payload = (event["payload"] && typeof event["payload"] === "object" ? event["payload"] : {}) as Record<string, unknown>;
  const runId = text(payload["run_id"]) ?? (type.startsWith("run.") ? text(event["aggregate_id"]) : null);

  if (type === "thread.message_posted") {
    if (text(payload["thread_id"]) !== state.threadId) {
      return state;
    }
    return { ...state, stale: { ...state.stale, messages: true } };
  }
  if (type === "thread.created") {
    return state;
  }
  if (type.startsWith("question.")) {
    if (!runId || !ownsRun(state, runId)) {
      return state;
    }
    const runs = type === "question.asked" ? { ...state.runs, [runId]: withStatus(state.runs[runId], runId, "WAITING_QUESTION") } : state.runs;
    return { ...state, runs, stale: { ...state.stale, questions: true } };
  }
  if (!runId || !ownsRun(state, runId)) {
    return state;
  }
  if (type.startsWith("run.")) {
    const to = text(payload["to"]) ?? text(payload["status"]);
    if (!to) {
      return state;
    }
    const run = withStatus(state.runs[runId], runId, to);
    const finished = TERMINAL_RUN_STATES.has(to);
    return {
      ...state,
      runs: { ...state.runs, [runId]: finished ? { ...run, stage: null } : run },
      activeRunId: finished && state.activeRunId === runId ? null : state.activeRunId,
    };
  }
  if (type === "turn.started") {
    const run = withStatus(state.runs[runId], runId, state.runs[runId]?.status ?? "RUNNING");
    return { ...state, runs: { ...state.runs, [runId]: { ...run, turnId: text(payload["turn_id"]), streaming: "", stage: null, cards: [] } } };
  }
  if (type === "turn.completed" || type === "turn.aborted") {
    const run = state.runs[runId];
    if (!run) {
      return state;
    }
    return { ...state, runs: { ...state.runs, [runId]: { ...run, stage: null } }, stale: { ...state.stale, messages: true } };
  }
  if (type.startsWith("step.")) {
    const stepId = text(payload["step_id"]);
    const turnId = text(payload["turn_id"]);
    if (!stepId || !turnId) {
      return state;
    }
    const run = withStatus(state.runs[runId], runId, state.runs[runId]?.status ?? "RUNNING");
    const status = text(payload["status"]) ?? type.slice("step.".length);
    const card: ActivityCard = { stepId, turnId, kind: text(payload["kind"]) ?? "step", status, ref: text(payload["ref"]) };
    const index = run.cards.findIndex((existing) => existing.stepId === stepId);
    const cards = index >= 0 ? run.cards.map((existing, at) => (at === index ? { ...existing, ...card, kind: existing.kind === "step" ? card.kind : existing.kind } : existing)) : [...run.cards, card];
    return { ...state, runs: { ...state.runs, [runId]: { ...run, cards } } };
  }
  return state;
}

function applyLive(state: ConversationState, frame: LiveFrame): ConversationState {
  const live = frame.frame;
  const runId = text(live["run_id"]);
  if (!runId || !ownsRun(state, runId)) {
    return state;
  }
  const run = withStatus(state.runs[runId], runId, state.runs[runId]?.status ?? "RUNNING");
  const turnId = text(live["turn_id"]) ?? run.turnId;
  if (live["kind"] === "model.delta") {
    const delta = text(live["text"]) ?? "";
    // A delta for a new turn starts a new preview; `final` carries the whole text.
    const same = turnId === run.turnId;
    const streaming = live["final"] === true ? delta : (same ? run.streaming : "") + delta;
    return { ...state, runs: { ...state.runs, [runId]: { ...run, turnId, streaming } } };
  }
  if (live["kind"] === "progress") {
    return { ...state, runs: { ...state.runs, [runId]: { ...run, turnId, stage: text(live["stage"]) } } };
  }
  return state;
}

// ---- projection decoding: the wire shapes of GET /v1/messages and /v1/questions ----

/** Decode `GET /v1/messages`; blocks other than text are summarized by their type. */
export function decodeMessages(payload: unknown): ConversationMessage[] {
  const list = asList(payload, "messages");
  const messages: ConversationMessage[] = [];
  for (const raw of list) {
    const record = asRecord(raw);
    const id = text(record["id"]);
    const threadId = text(record["thread_id"]);
    if (!id || !threadId) {
      continue;
    }
    const authorKind = text(record["author_kind"]);
    const blocks = Array.isArray(record["content_blocks"]) ? (record["content_blocks"] as unknown[]) : [];
    const parts: string[] = [];
    for (const block of blocks) {
      const item = asRecord(block);
      const value = text(item["text"]);
      parts.push(value ?? `[${text(item["type"]) ?? "block"}]`);
    }
    messages.push({
      id,
      threadId,
      seq: typeof record["seq"] === "number" ? record["seq"] : messages.length + 1,
      authorKind: authorKind === "agent" || authorKind === "system" ? authorKind : "user",
      authorId: text(record["author_id"]) ?? "",
      runId: text(record["run_id"]),
      turnId: text(record["turn_id"]),
      text: parts.join("\n"),
      attachments: Array.isArray(record["attachments"]) ? (record["attachments"] as unknown[]).filter((item): item is string => typeof item === "string") : [],
    });
  }
  return messages;
}

/** Decode `GET /v1/questions`. */
export function decodeQuestions(payload: unknown): ConversationQuestion[] {
  const list = asList(payload, "questions");
  const questions: ConversationQuestion[] = [];
  for (const raw of list) {
    const record = asRecord(raw);
    const id = text(record["id"]);
    const runId = text(record["run_id"]);
    if (!id || !runId) {
      continue;
    }
    const status = text(record["status"]);
    questions.push({
      id,
      runId,
      kind: text(record["kind"]) ?? "free_text",
      prompt: text(record["prompt"]) ?? "",
      options: Array.isArray(record["options"]) ? (record["options"] as unknown[]).filter((item): item is string => typeof item === "string") : [],
      required: record["required"] === true,
      status: status === "answered" || status === "expired" || status === "cancelled" ? status : "open",
      answer: asRecord(record["answer"])["value"] ?? null,
    });
  }
  return questions;
}

function asList(payload: unknown, key: string): unknown[] {
  const record = asRecord(payload);
  return Array.isArray(record[key]) ? (record[key] as unknown[]) : [];
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

/** The questions still waiting for the user in this thread. */
export function openQuestions(state: ConversationState): readonly ConversationQuestion[] {
  return state.questions.filter((question) => question.status === "open");
}

/** The run whose turn can be cancelled right now, if any. */
export function cancellableRun(state: ConversationState): RunActivity | null {
  const active = state.activeRunId ? state.runs[state.activeRunId] : undefined;
  if (active && !TERMINAL_RUN_STATES.has(active.status)) {
    return active;
  }
  const candidates = Object.values(state.runs).filter((run) => !TERMINAL_RUN_STATES.has(run.status));
  return candidates[candidates.length - 1] ?? null;
}
