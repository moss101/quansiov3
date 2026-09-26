/**
 * APP-004: the conversation reducer over durable events and live frames, the composer's
 * commands, and the inspector's decoding — pure, no server.
 */

import { describe, expect, it } from "vitest";

import type { StreamFrame } from "../src/client/stream.js";
import {
  ANSWER_COMMAND,
  CANCEL_COMMAND,
  MAX_UPLOAD_BYTES,
  answerParams,
  attachmentFromUpload,
  cancelParams,
  cancellableRun,
  conversationReducer,
  decodeMessages,
  decodeQuestions,
  decodeTimeline,
  initialConversation,
  openQuestions,
  postParams,
  postedIdentities,
  postedRunStatus,
  uploadParams,
  type ConversationAction,
  type ConversationState,
} from "../src/index.js";

const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const THREAD = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const OTHER_RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";
const TURN = "trn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const QUESTION = "q_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const ARTIFACT = "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

function event(type: string, payload: Record<string, unknown>, cursor = "c1"): StreamFrame {
  const aggregate = payload["run_id"] ?? payload["thread_id"];
  return { kind: "event", cursor, event: { type, aggregate_id: typeof aggregate === "string" ? aggregate : "", payload } };
}

function live(frame: Record<string, unknown>): StreamFrame {
  return { kind: "live", channel: `workspace:${WORKSPACE}`, frame: { kind: "model.delta", ...frame } };
}

function reduce(state: ConversationState, ...actions: ConversationAction[]): ConversationState {
  return actions.reduce(conversationReducer, state);
}

function posted(): ConversationState {
  return reduce(
    initialConversation(WORKSPACE),
    { type: "composer.text", text: "hello" },
    { type: "composer.posting" },
    { type: "composer.posted", threadId: THREAD, runId: RUN, messageId: "msg_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" },
  );
}

describe("streaming UI", () => {
  it("shows model deltas of its own run while the turn runs and drops them once the durable reply lands", () => {
    let state = posted();
    expect(state.threadId).toBe(THREAD);
    expect(state.activeRunId).toBe(RUN);
    expect(state.composer.text).toBe("");

    state = reduce(
      state,
      { type: "frame", frame: event("turn.started", { run_id: RUN, turn_id: TURN }) },
      { type: "frame", frame: { kind: "live", channel: `workspace:${WORKSPACE}`, frame: { kind: "progress", run_id: RUN, turn_id: TURN, stage: "model_call" } } },
      { type: "frame", frame: live({ run_id: RUN, turn_id: TURN, text: "wor" }) },
      { type: "frame", frame: live({ run_id: RUN, turn_id: TURN, text: "king" }) },
    );
    expect(state.runs[RUN]?.streaming).toBe("working");
    expect(state.runs[RUN]?.stage).toBe("model_call");

    // A final delta carries the whole text, not another piece.
    state = reduce(state, { type: "frame", frame: live({ run_id: RUN, turn_id: TURN, text: "working on it", final: true }) });
    expect(state.runs[RUN]?.streaming).toBe("working on it");

    // A frame for a run this thread does not own changes nothing.
    const before = state;
    state = reduce(state, { type: "frame", frame: live({ run_id: OTHER_RUN, turn_id: "trn_x", text: "leak" }) });
    expect(state).toBe(before);

    state = reduce(state, { type: "frame", frame: event("turn.completed", { run_id: RUN, turn_id: TURN }) });
    expect(state.stale.messages).toBe(true);
    expect(state.runs[RUN]?.stage).toBeNull();

    state = reduce(state, {
      type: "messages.loaded",
      threadId: THREAD,
      messages: [
        { id: "msg_1", threadId: THREAD, seq: 1, authorKind: "user", authorId: "usr_1", runId: null, turnId: null, text: "hello", attachments: [], createdAt: null },
        { id: "msg_2", threadId: THREAD, seq: 2, authorKind: "agent", authorId: "ath_1", runId: RUN, turnId: TURN, text: "working on it", attachments: [], createdAt: null },
      ],
    });
    expect(state.stale.messages).toBe(false);
    expect(state.runs[RUN]?.streaming).toBe("");
    expect(state.messages.map((message) => message.authorKind)).toEqual(["user", "agent"]);
  });

  it("keeps tool and step cards for the current turn and forgets them when the next turn starts", () => {
    let state = posted();
    state = reduce(
      state,
      { type: "frame", frame: event("turn.started", { run_id: RUN, turn_id: TURN }) },
      { type: "frame", frame: event("step.created", { run_id: RUN, turn_id: TURN, step_id: "stp_1", kind: "model_call", status: "pending" }) },
      { type: "frame", frame: event("step.dispatched", { run_id: RUN, turn_id: TURN, step_id: "stp_1", kind: "model_call", status: "dispatched" }) },
      { type: "frame", frame: event("step.created", { run_id: RUN, turn_id: TURN, step_id: "stp_2", kind: "tool_call", status: "pending", ref: "fs.read" }) },
      { type: "frame", frame: event("step.completed", { run_id: RUN, turn_id: TURN, step_id: "stp_2", kind: "tool_call", status: "completed", ref: "fs.read" }) },
    );
    expect(state.runs[RUN]?.cards.map((card) => [card.kind, card.status, card.ref])).toEqual([
      ["model_call", "dispatched", null],
      ["tool_call", "completed", "fs.read"],
    ]);
    state = reduce(state, { type: "frame", frame: event("turn.started", { run_id: RUN, turn_id: "trn_2" }) });
    expect(state.runs[RUN]?.cards).toEqual([]);
    expect(state.runs[RUN]?.turnId).toBe("trn_2");
  });
});

describe("questions and cancel", () => {
  it("parks on a question, answers it in the thread and resumes", () => {
    let state = posted();
    state = reduce(state, { type: "frame", frame: event("question.asked", { run_id: RUN, question_id: QUESTION }) });
    expect(state.runs[RUN]?.status).toBe("WAITING_QUESTION");
    expect(state.stale.questions).toBe(true);
    state = reduce(state, {
      type: "questions.loaded",
      questions: decodeQuestions({
        questions: [{ id: QUESTION, run_id: RUN, kind: "single_choice", prompt: "Which?", options: ["alpha", "beta"], required: true, status: "open", answer: null }],
      }),
    });
    expect(openQuestions(state).map((question) => question.id)).toEqual([QUESTION]);
    expect(answerParams(QUESTION, "alpha")).toEqual({ question_id: QUESTION, value: "alpha" });
    expect(ANSWER_COMMAND).toBe("AnswerQuestion");

    state = reduce(
      state,
      { type: "frame", frame: event("question.answered", { run_id: RUN, question_id: QUESTION, value: "alpha" }) },
      { type: "frame", frame: event("run.resumed", { run_id: RUN, from: "WAITING_QUESTION", to: "RUNNING" }) },
    );
    expect(state.runs[RUN]?.status).toBe("RUNNING");
    expect(state.stale.questions).toBe(true);
    state = reduce(state, {
      type: "questions.loaded",
      questions: decodeQuestions({
        questions: [{ id: QUESTION, run_id: RUN, kind: "single_choice", prompt: "Which?", options: ["alpha", "beta"], required: true, status: "answered", answer: { value: "alpha" } }],
      }),
    });
    expect(openQuestions(state)).toEqual([]);
    expect(state.questions[0]?.answer).toBe("alpha");
  });

  it("offers cancel for the live run and stops offering it once the run is cancelled", () => {
    let state = posted();
    expect(cancellableRun(state)?.runId).toBe(RUN);
    expect(cancelParams(RUN)).toEqual({ run_id: RUN, generation: 1 });
    expect(CANCEL_COMMAND).toBe("CancelTurn");
    state = reduce(state, { type: "frame", frame: event("run.cancelled", { run_id: RUN, from: "RUNNING", to: "CANCELLED" }) });
    expect(state.runs[RUN]?.status).toBe("CANCELLED");
    expect(state.activeRunId).toBeNull();
    expect(cancellableRun(state)).toBeNull();
    expect(() => cancelParams("ui-run")).toThrow(/run_/);
  });
});

describe("reload", () => {
  it("rebuilds the same thread from the projections alone", () => {
    const wire = {
      messages: [
        { id: "msg_1", thread_id: THREAD, seq: 1, author_kind: "user", author_id: "usr_1", run_id: null, turn_id: null, content_blocks: [{ type: "text", text: "hello" }], attachments: [ARTIFACT] },
        { id: "msg_2", thread_id: THREAD, seq: 2, author_kind: "agent", author_id: "ath_1", run_id: RUN, turn_id: TURN, content_blocks: [{ type: "text", text: "done" }], attachments: [] },
      ],
    };
    const fresh = reduce(
      initialConversation(WORKSPACE, THREAD),
      { type: "messages.loaded", threadId: THREAD, messages: decodeMessages(wire) },
      { type: "run.loaded", runId: RUN, status: "SUCCEEDED" },
    );
    expect(fresh.messages[0]?.attachments).toEqual([ARTIFACT]);
    expect(fresh.messages[1]?.runId).toBe(RUN);
    expect(fresh.runs[RUN]?.status).toBe("SUCCEEDED");
    // Frames for the run are accepted because the thread's messages name it.
    const withFrame = reduce(fresh, { type: "frame", frame: live({ run_id: RUN, turn_id: "trn_3", text: "more" }) });
    expect(withFrame.runs[RUN]?.streaming).toBe("more");
    // Selecting another thread starts over; selecting the same one is a no-op.
    expect(reduce(fresh, { type: "thread.selected", threadId: THREAD })).toBe(fresh);
    expect(reduce(fresh, { type: "thread.selected", threadId: "thr_other" }).messages).toEqual([]);
  });
});

describe("composer", () => {
  it("attaches artifacts by identity and sends them with the message", () => {
    let state = reduce(initialConversation(WORKSPACE, THREAD), { type: "composer.text", text: "  look  " });
    state = reduce(state, { type: "composer.attach", attachment: attachmentFromUpload({ artifact_id: ARTIFACT, title: "brief.pdf", size_bytes: 12 }) });
    state = reduce(state, { type: "composer.attach", attachment: { artifactId: ARTIFACT, title: "dup", sizeBytes: 12 } });
    expect(state.composer.attachments).toHaveLength(1);
    expect(postParams(state)).toEqual({ workspace_id: WORKSPACE, content: "look", thread_id: THREAD, attachments: [ARTIFACT] });
    state = reduce(state, { type: "composer.attach", attachment: { artifactId: "local-file", title: "x", sizeBytes: 1 } });
    expect(state.composer.error).toMatch(/artifacts/);
    expect(state.composer.attachments).toHaveLength(1);
    state = reduce(state, { type: "composer.detach", artifactId: ARTIFACT });
    expect(state.composer.attachments).toEqual([]);
    expect(() => attachmentFromUpload({ title: "no id" })).toThrow(/artifact id/);
    expect(postedIdentities({ thread_id: THREAD, run_id: RUN, message_id: "msg_1" })).toEqual({ threadId: THREAD, runId: RUN, messageId: "msg_1" });
  });

  it("encodes an upload within the bound and refuses one outside it", () => {
    const bytes = new Uint8Array([0, 255, 1, 254]);
    const params = uploadParams(WORKSPACE, { name: " sample.bin ", mediaType: "", bytes }, THREAD);
    expect(params).toEqual({ workspace_id: WORKSPACE, title: "sample.bin", media_type: "application/octet-stream", bytes_base64: "AP8B/g==", thread_id: THREAD });
    expect(() => uploadParams(WORKSPACE, { name: "empty", mediaType: "text/plain", bytes: new Uint8Array(0) })).toThrow(/bytes/);
    expect(() => uploadParams(WORKSPACE, { name: "big", mediaType: "text/plain", bytes: new Uint8Array(MAX_UPLOAD_BYTES + 1) })).toThrow(/bytes/);
  });

  it("records a failed post and keeps the draft", () => {
    let state = reduce(initialConversation(WORKSPACE), { type: "composer.text", text: "keep me" }, { type: "composer.posting" });
    expect(state.composer.posting).toBe(true);
    state = reduce(state, { type: "composer.failed", error: "VALIDATION_BOUNDS" });
    expect(state.composer).toMatchObject({ text: "keep me", posting: false, error: "VALIDATION_BOUNDS" });
  });
});

describe("inspector", () => {
  it("decodes the model route, context sources with trust labels, tools with effect status and the question", () => {
    const inspection = decodeTimeline({
      run: { id: RUN, work_node_id: "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", status: "SUCCEEDED", trigger: { kind: "message", ref: "msg_1" } },
      trigger_message: { message_id: "msg_1", attachments: [ARTIFACT] },
      turns: [
        {
          id: TURN,
          seq: 1,
          input: { kind: "message" },
          status: "completed",
          context: { sources: [{ kind: "thread", source: "thr_1", trust_level: 2, tokens: 120 }, { kind: "artifact", source: ARTIFACT, trust_level: "TRUSTED_USER" }] },
          steps: [
            { kind: "model_call", model_call: { stop_reason: "tool_use", latency_ms: 12, route: { provider: "in-process", model_id: "conformance-stub", chosen_by: "seam:conformance" } } },
            { kind: "tool_call", tool_calls: [{ tool_name: "fs.read", status: "completed", effect_id: "eff_1", effect_class: "read.internal", effect_status: "settled" }] },
            { kind: "wait", question: { id: QUESTION, prompt: "Which?", status: "answered", answer: { value: "alpha" } } },
          ],
        },
      ],
    });
    expect(inspection.workNodeId).toBe("wn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(inspection.attachments).toEqual([ARTIFACT]);
    const [turn] = inspection.turns;
    expect(turn?.modelRoute).toBe("in-process/conformance-stub");
    expect(turn?.chosenBy).toBe("seam:conformance");
    expect(turn?.sources.map((source) => `${source.kind}:${source.trustLevel}`)).toEqual(["thread:2", "artifact:TRUSTED_USER"]);
    expect(turn?.tools).toEqual([{ toolName: "fs.read", status: "completed", effectId: "eff_1", effectClass: "read.internal", effectStatus: "settled" }]);
    expect(turn?.question).toEqual({ id: QUESTION, prompt: "Which?", status: "answered", answer: "alpha" });
    expect(() => decodeTimeline({})).toThrow(/run/);
  });
});

describe("a run that finishes inside the post", () => {
  it("is settled by the post's run status even when its terminal event came first", () => {
    const thread = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
    const run = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
    let state = initialConversation(WORKSPACE, thread);
    // The terminal event arrives before the renderer knows the run: it is not this thread's yet.
    state = conversationReducer(state, { type: "frame", frame: { kind: "event", cursor: "c1", event: { type: "run.state_changed", aggregate_id: run, payload: { run_id: run, to: "SUCCEEDED" } } } as never });
    state = conversationReducer(state, { type: "composer.posted", threadId: thread, runId: run, messageId: "msg_1" });
    expect(cancellableRun(state)?.runId).toBe(run);
    const status = postedRunStatus({ thread_id: thread, run_id: run, message_id: "msg_1", run_status: "SUCCEEDED" });
    expect(status).toBe("SUCCEEDED");
    state = conversationReducer(state, { type: "run.loaded", runId: run, status: status ?? "" });
    expect(cancellableRun(state)).toBeNull();
    expect(postedRunStatus({ run_status: "" })).toBeNull();
    expect(postedRunStatus(null)).toBeNull();
  });
});
