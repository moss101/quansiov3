/**
 * APP-009 unit tests: one stable order for messages from any number of sources, the thread
 * view the server gave (membership, attention, the responsible teammate), reactions and
 * mentions as the page builds them, and the durable events applied to the view — including
 * the viewer's own removal, which ends the view.
 */

import { describe, expect, it } from "vitest";

import {
  applyThreadEvent,
  attentionFor,
  createThreadParams,
  decodeThreadList,
  decodeThreadMessages,
  decodeThreadView,
  handOffParams,
  handoffTargets,
  hasGap,
  mayLeave,
  mayManage,
  mentionCandidates,
  mentionsIn,
  mergeMessages,
  participantParams,
  reactionToggle,
  summarizeReactions,
  type ThreadMessage,
  type ThreadState,
} from "../src/index.js";

const THREAD = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const EDITOR = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";
const ATLAS = "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const NOVA = "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";

function view(overrides: Record<string, unknown> = {}): unknown {
  return {
    id: THREAD,
    workspace_id: "ws_x",
    kind: "group",
    title: "launch",
    protected: true,
    participants: [
      { kind: "user", id: OWNER, role: "owner", joined_at: "2026-09-23T10:00:00Z", left_at: null, active: true },
      { kind: "user", id: EDITOR, role: "member", joined_at: "2026-09-23T10:00:01Z", left_at: null, active: true },
      { kind: "teammate", id: ATLAS, role: "member", joined_at: "2026-09-23T10:00:02Z", left_at: null, active: true },
    ],
    attention: [
      { kind: "user", id: EDITOR, mentions_pending: 1, open_questions: 0, needs_human: true },
      { kind: "teammate", id: ATLAS, agent_status: "ACTIVE", run: { id: "run_1", status: "RUNNING" }, needs_human: false },
    ],
    responsible_teammate_id: ATLAS,
    my_role: "owner",
    ...overrides,
  };
}

function message(seq: number, id: string, extra: Partial<ThreadMessage> = {}): ThreadMessage {
  return { id, threadId: THREAD, seq, authorKind: "user", authorId: OWNER, text: `m${String(seq)}`, notice: null, mentions: [], reactions: [], runId: null, ...extra };
}

describe("message order", () => {
  it("keeps one copy of each id in seq order whatever the arrival order", () => {
    const fromStream = [message(3, "msg_c"), message(1, "msg_a")];
    const fromRead = [message(2, "msg_b"), message(1, "msg_a", { text: "fresh" }), message(3, "msg_c")];
    const merged = mergeMessages(fromStream, fromRead);
    expect(merged.map((m) => m.seq)).toEqual([1, 2, 3]);
    expect(merged.find((m) => m.id === "msg_a")?.text).toBe("fresh");
    expect(mergeMessages(fromRead, fromStream).map((m) => m.id)).toEqual(merged.map((m) => m.id));
    expect(hasGap(merged)).toBe(false);
    expect(hasGap(fromStream)).toBe(true);
  });

  it("decodes messages with mentions, reactions and a handoff notice", () => {
    const messages = decodeThreadMessages({
      messages: [
        { id: "msg_2", thread_id: THREAD, seq: 2, author_kind: "system", author_id: "system", content_blocks: [{ type: "system_notice", text: "Atlas handed this thread to Nova.", notice: { kind: "handoff", to: { kind: "teammate", id: NOVA } } }], mentions: [], reactions: [] },
        { id: "msg_1", thread_id: THREAD, seq: 1, author_kind: "user", author_id: OWNER, content_blocks: [{ type: "text", text: "@editor look" }], mentions: [{ kind: "user", id: EDITOR }, { kind: "robot", id: "x" }], reactions: [{ emoji: "👍", user_id: EDITOR }] },
      ],
    });
    expect(messages.map((m) => m.id)).toEqual(["msg_1", "msg_2"]);
    expect(messages[0]?.mentions).toEqual([{ kind: "user", id: EDITOR }]);
    expect(messages[0]?.reactions).toEqual([{ emoji: "👍", userId: EDITOR }]);
    expect(messages[1]?.notice?.["kind"]).toBe("handoff");
  });
});

describe("thread view and membership", () => {
  it("decodes membership, attention and the responsible teammate", () => {
    const decoded = decodeThreadView(view());
    expect(decoded.protected).toBe(true);
    expect(decoded.participants.map((p) => p.id)).toEqual([OWNER, EDITOR, ATLAS]);
    expect(attentionFor(decoded, "user", EDITOR)?.mentionsPending).toBe(1);
    expect(attentionFor(decoded, "teammate", ATLAS)?.run?.status).toBe("RUNNING");
    expect(decoded.responsibleTeammateId).toBe(ATLAS);
  });

  it("offers management to an owner or an admin, and leaving unless it strands people", () => {
    const owned = decodeThreadView(view());
    expect(mayManage(owned, false)).toBe(true);
    expect(mayManage(decodeThreadView(view({ my_role: "member" })), false)).toBe(false);
    expect(mayManage(decodeThreadView(view({ my_role: null })), true)).toBe(true);
    expect(mayLeave(owned, OWNER)).toBe(false);
    expect(mayLeave(owned, EDITOR)).toBe(true);
    expect(mayLeave(owned, "usr_nobody")).toBe(false);
  });

  it("offers a handoff only to other active, available teammates", () => {
    const roster = [
      { id: ATLAS, status: "active", availability: "available" },
      { id: NOVA, status: "active", availability: "available" },
      { id: "agt_away", status: "active", availability: "away" },
      { id: "agt_gone", status: "archived", availability: "available" },
    ];
    expect(handoffTargets(decodeThreadView(view()), roster).map((t) => t.id)).toEqual([NOVA]);
    expect(handoffTargets(decodeThreadView(view({ responsible_teammate_id: null })), roster)).toEqual([]);
  });

  it("decodes the thread list with the reader's attention", () => {
    const list = decodeThreadList({ threads: [{ id: THREAD, kind: "group", title: "launch", protected: true, message_count: 4, attention: { mentions_pending: 2, open_questions: 1, needs_you: true } }] });
    expect(list[0]).toMatchObject({ id: THREAD, protected: true, mentionsPending: 2, openQuestions: 1, needsYou: true });
  });
});

describe("reactions and mentions", () => {
  it("groups reactions and toggles the viewer's own", () => {
    const reactions = [{ emoji: "👍", userId: OWNER }, { emoji: "🎉", userId: EDITOR }, { emoji: "👍", userId: EDITOR }];
    expect(summarizeReactions(reactions, EDITOR)).toEqual([
      { emoji: "👍", count: 2, mine: true },
      { emoji: "🎉", count: 1, mine: true },
    ]);
    const target = message(1, "msg_a", { reactions });
    expect(reactionToggle(target, OWNER, "👍")).toEqual({ message_id: "msg_a", emoji: "👍", remove: true });
    expect(reactionToggle(target, OWNER, "🎉")).toEqual({ message_id: "msg_a", emoji: "🎉", remove: false });
  });

  it("turns @handles into mentions of known people only", () => {
    const people = [
      { kind: "user" as const, id: EDITOR, handle: "editor" },
      { kind: "teammate" as const, id: ATLAS, handle: "Atlas" },
    ];
    expect(mentionsIn("@Editor and @atlas, and @nobody, again @editor", people)).toEqual([
      { kind: "user", id: EDITOR },
      { kind: "teammate", id: ATLAS },
    ]);
    expect(mentionsIn("mail me at a@editor.com", people)).toEqual([]);
    expect(mentionCandidates("@at", people).map((p) => p.id)).toEqual([ATLAS]);
  });

  it("builds the commands with the ids the server expects and refuses others", () => {
    expect(createThreadParams("ws_x", "group", " launch ", [{ kind: "user", id: EDITOR }])).toEqual({ workspace_id: "ws_x", kind: "group", title: "launch", participants: [{ kind: "user", id: EDITOR, role: "member" }] });
    expect(() => createThreadParams("ws_x", "group", "  ", [])).toThrow(/title/);
    expect(() => createThreadParams("ws_x", "group", "t", [{ kind: "teammate", id: EDITOR }])).toThrow(/agt_/);
    expect(participantParams(THREAD, "user", EDITOR, "owner")).toEqual({ thread_id: THREAD, kind: "user", id: EDITOR, role: "owner" });
    expect(() => participantParams("x", "user", EDITOR)).toThrow(/thr_/);
    expect(handOffParams(THREAD, NOVA)).toEqual({ thread_id: THREAD, to_teammate_id: NOVA });
    expect(() => handOffParams(THREAD, EDITOR)).toThrow();
  });
});

describe("durable events on the view", () => {
  function state(): ThreadState {
    return { view: decodeThreadView(view()), messages: [message(1, "msg_a")], revoked: false, stale: false };
  }

  it("applies reactions, flags new posts and ignores other threads", () => {
    const reacted = applyThreadEvent(state(), { type: "thread.reaction_added", aggregate_type: "thread", aggregate_id: THREAD, payload: { message_id: "msg_a", emoji: "👀", user_id: EDITOR } }, OWNER);
    expect(reacted.messages[0]?.reactions).toEqual([{ emoji: "👀", userId: EDITOR }]);
    const unreacted = applyThreadEvent(reacted, { type: "thread.reaction_removed", aggregate_type: "thread", aggregate_id: THREAD, payload: { message_id: "msg_a", emoji: "👀", user_id: EDITOR } }, OWNER);
    expect(unreacted.messages[0]?.reactions).toEqual([]);
    expect(applyThreadEvent(state(), { type: "thread.message_posted", aggregate_type: "thread", aggregate_id: THREAD, payload: { message_id: "msg_b", seq: 2 } }, OWNER).stale).toBe(true);
    expect(applyThreadEvent(state(), { type: "thread.message_posted", aggregate_type: "thread", aggregate_id: THREAD, payload: { message_id: "msg_a", seq: 1 } }, OWNER).stale).toBe(false);
    const other = state();
    expect(applyThreadEvent(other, { type: "thread.message_posted", aggregate_type: "thread", aggregate_id: "thr_other", payload: {} }, OWNER)).toBe(other);
  });

  it("ends the view when the viewer is removed, and follows a handoff", () => {
    const removed = applyThreadEvent(state(), { type: "thread.participant_removed", aggregate_type: "thread", aggregate_id: THREAD, payload: { principal: { kind: "user", id: EDITOR }, role: "member" } }, EDITOR);
    expect(removed.revoked).toBe(true);
    expect(removed.messages).toEqual([]);
    expect(removed.view.participants.find((p) => p.id === EDITOR)?.active).toBe(false);
    const someoneElse = applyThreadEvent(state(), { type: "thread.participant_removed", aggregate_type: "thread", aggregate_id: THREAD, payload: { principal: { kind: "user", id: EDITOR }, role: "member" } }, OWNER);
    expect(someoneElse.revoked).toBe(false);
    expect(someoneElse.messages).toHaveLength(1);
    const handed = applyThreadEvent(state(), { type: "thread.handed_off", aggregate_type: "thread", aggregate_id: THREAD, payload: { from: { kind: "teammate", id: ATLAS }, to: { kind: "teammate", id: NOVA } } }, OWNER);
    expect(handed.view.responsibleTeammateId).toBe(NOVA);
  });
});
