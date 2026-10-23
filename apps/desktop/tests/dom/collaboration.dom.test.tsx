// @vitest-environment jsdom
/** APP-009 on a page: the Collaboration view over the real preload API, answering from thread projections. */

import { afterEach, describe, expect, it } from "vitest";

import { Collaboration } from "../../src/renderer/Collaboration.js";
import { click, mount, text, type, type Rig } from "./rig.js";

const THREAD = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const EDITOR = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";
const ATLAS = "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const NOVA = "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";

function threadView(extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: THREAD, workspace_id: "ws_x", kind: "group", title: "launch", protected: true,
    participants: [
      { kind: "user", id: OWNER, role: "owner", joined_at: "2026-09-23T10:00:00Z", left_at: null, active: true },
      { kind: "user", id: EDITOR, role: "member", joined_at: "2026-09-23T10:00:01Z", left_at: null, active: true },
      { kind: "teammate", id: ATLAS, role: "member", joined_at: "2026-09-23T10:00:02Z", left_at: null, active: true },
      { kind: "user", id: "usr_gone", role: "member", joined_at: "2026-09-23T10:00:02Z", left_at: "2026-09-24T10:00:02Z", active: false },
    ],
    attention: [{ kind: "user", id: EDITOR, mentions_pending: 1, open_questions: 0, needs_human: true }],
    responsible_teammate_id: ATLAS,
    my_role: "owner",
    ...extra,
  };
}

function teammate(id: string, name: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return { id, workspace_id: "ws_x", name, status: "active", availability: "available", ...extra };
}

const messages = {
  messages: [
    { id: "msg_2", thread_id: THREAD, seq: 2, author_kind: "system", author_id: "system", content_blocks: [{ type: "system_notice", text: "Atlas took this thread.", notice: { kind: "handoff" } }], mentions: [], reactions: [] },
    { id: "msg_1", thread_id: THREAD, seq: 1, author_kind: "user", author_id: EDITOR, content_blocks: [{ type: "text", text: "ship it" }], mentions: [], reactions: [{ emoji: "👍", user_id: OWNER }] },
  ],
};

const ok = { ok: true, result: {} };
const answers = {
  "threads.list": { threads: [{ id: THREAD, kind: "group", title: "launch", protected: true, message_count: 2, attention: { mentions_pending: 1, open_questions: 1, needs_you: true } }] },
  "threads.view": { ok: true, result: threadView() },
  "threads.messages": messages,
  "teammates.list": { workspace_id: "ws_x", teammates: [teammate(ATLAS, "Atlas"), teammate(NOVA, "Nova"), teammate("agt_old", "Old", { status: "archived" })] },
  "session.me": { user: { id: OWNER }, workspaces: [{ id: "ws_x", role: "member" }] },
  "participants.add": ok, "participants.remove": ok, "messages.react": ok, "threads.handoff": ok, "threads.create": { ok: true, result: { thread_id: THREAD } },
} as const;

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

async function open(extra: Record<string, unknown> = {}): Promise<void> {
  rig = await mount(<Collaboration workspaceId="ws_x" />, { ...answers, ...extra });
  await click(rig, "launch");
}

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

describe("the Collaboration view", () => {
  it("lists threads with attention, and prompts to pick one", async () => {
    rig = await mount(<Collaboration workspaceId="ws_x" />, answers);
    expect(rig.container.querySelector(".collab-empty")?.textContent).toBe("Pick a thread.");
    const row = rig.container.querySelector(".collab-threads li")?.textContent ?? "";
    expect(row).toContain("members only");
    expect(row).toContain("needs you (2)");
  });

  it("opens a thread: messages in seq order, a notice marked as such, members, and the responsible teammate", async () => {
    await open();
    const items = [...rig.container.querySelectorAll(".messages li")];
    expect(items.map((item) => item.getAttribute("data-seq"))).toEqual(["1", "2"]);
    expect(items[1]?.querySelector(".author")?.textContent).toBe("Notice");
    const members = rig.container.querySelector(".collab-members")?.textContent ?? "";
    expect(members).toContain("Atlas");
    expect(members).toContain("responsible");
    expect(members).toContain("left");
    expect(members).toContain("needs you");
  });

  it("shows the server's refusal to open a thread", async () => {
    rig = await mount(<Collaboration workspaceId="ws_x" />, { ...answers, "threads.view": { ok: false, error: { code: "PERMISSION_DENIED", message: "not a member" } } });
    await click(rig, "launch");
    expect(text(rig)).toContain("Refused — PERMISSION_DENIED: not a member");
    expect(rig.container.querySelector(".collab-thread")).toBeNull();
  });

  it("toggles reactions: removes mine, adds a quick one", async () => {
    await open();
    await click(rig, "👍 1");
    expect(sent("messages.react")[0]?.payload).toEqual({ message_id: "msg_1", emoji: "👍", remove: true });
    await click(rig, "🎉");
    expect(sent("messages.react")[1]?.payload).toEqual({ message_id: "msg_1", emoji: "🎉", remove: false });
  });

  it("posts a message with mentions of known members, offering @ completions", async () => {
    await open({ "messages.post": { ok: true } });
    await type(rig, "Message the thread", "hey @At");
    const candidate = rig.container.querySelector(".composer .cards button");
    expect(candidate?.textContent).toBe("@Atlas");
    await click(rig, "@Atlas");
    expect((rig.container.querySelector("textarea") as HTMLTextAreaElement).value).toBe("hey @Atlas ");
    await click(rig, "Send");
    expect(sent("messages.post")[0]?.payload).toMatchObject({ threadId: THREAD, content: "hey @Atlas ", mentions: [{ kind: "teammate", id: ATLAS }] });
  });

  it("does not send an empty draft, and shows a failed send as a notice", async () => {
    await open({ "messages.post": () => Promise.reject(new Error("offline")) });
    await click(rig, "Send");
    expect(sent("messages.post")).toHaveLength(0);
    await type(rig, "Message the thread", "hello");
    await click(rig, "Send");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("offline");
  });

  it("manages members: removes another, adds a teammate or a user, leaves", async () => {
    await open();
    const removeButtons = [...rig.container.querySelectorAll<HTMLButtonElement>(".collab-members li button")];
    expect(removeButtons.length).toBe(2);
    removeButtons[0]?.click();
    await rig.flush();
    expect(sent("participants.remove")[0]?.payload).toMatchObject({ thread_id: THREAD, kind: "user", id: EDITOR });
    await type(rig, "or agt_", NOVA);
    await click(rig, "Add");
    expect(sent("participants.add")[0]?.payload).toMatchObject({ kind: "teammate", id: NOVA });
    await type(rig, "or agt_", "usr_new");
    await click(rig, "Add");
    expect(sent("participants.add")[1]?.payload).toMatchObject({ kind: "user", id: "usr_new" });
  });

  it("shows a notice, sends nothing and keeps the field when the id to add has the wrong prefix", async () => {
    await open();
    await type(rig, "or agt_", "bob");
    await click(rig, "Add");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("usr_");
    expect(sent("participants.add")).toHaveLength(0);
    expect(rig.container.querySelector<HTMLInputElement>(".collab-members input")?.value).toBe("bob");
  });

  it("hides member management from a plain member, who may leave", async () => {
    rig = await mount(<Collaboration workspaceId="ws_x" />, { ...answers, "session.me": { user: { id: EDITOR }, workspaces: [{ id: "ws_x", role: "member" }] }, "threads.view": { ok: true, result: threadView({ my_role: "member" }) } });
    await click(rig, "launch");
    expect([...rig.container.querySelectorAll(".collab-members li button")]).toHaveLength(0);
    await click(rig, "Leave thread");
    expect(sent("participants.remove")[0]?.payload).toMatchObject({ kind: "user", id: EDITOR });
  });

  it("hands the thread off to another available teammate", async () => {
    await open();
    expect(rig.container.querySelector(".collab-handoff")?.textContent).not.toContain("Atlas");
    await click(rig, "Hand to Nova");
    expect(sent("threads.handoff")[0]?.payload).toEqual({ thread_id: THREAD, to_teammate_id: NOVA });
  });

  it("shows a refusal from a command", async () => {
    await open({ "threads.handoff": { ok: false, error: { code: "CONFLICT_STATE", message: "busy" } } });
    await click(rig, "Hand to Nova");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("CONFLICT_STATE: busy");
  });

  it("creates a group thread from a title, people and a picked teammate, then opens it", async () => {
    rig = await mount(<Collaboration workspaceId="ws_x" />, answers);
    await type(rig, "Title", "launch plan");
    await type(rig, "People", `${EDITOR}, `);
    await click(rig, "Nova");
    await click(rig, "Create");
    expect(sent("threads.create")[0]?.payload).toEqual({
      workspace_id: "ws_x", kind: "group", title: "launch plan",
      participants: [{ kind: "user", id: EDITOR, role: "member" }, { kind: "teammate", id: NOVA, role: "member" }],
    });
    expect(rig.container.querySelector(".collab-thread")).not.toBeNull();
    expect((rig.container.querySelector("input[placeholder='Title']") as HTMLInputElement).value).toBe("");
  });

  it("refuses to create a group without a title", async () => {
    rig = await mount(<Collaboration workspaceId="ws_x" />, answers);
    await click(rig, "Create");
    expect(sent("threads.create")).toHaveLength(0);
  });

  it("follows the stream: a new post re-reads the thread, a reaction applies in place, removal closes the view", async () => {
    await open();
    const reads = (): number => sent("threads.messages").length;
    const before = reads();
    await rig.push("stream.frame", { event: { type: "thread.message_posted", aggregate_type: "thread", aggregate_id: THREAD, payload: { message_id: "msg_9", seq: 3 } } });
    expect(reads()).toBeGreaterThan(before);
    await rig.push("stream.frame", { event: { type: "thread.reaction_added", aggregate_type: "thread", aggregate_id: THREAD, payload: { message_id: "msg_1", emoji: "🎉", user_id: EDITOR } } });
    expect(text(rig)).toContain("🎉 1");
    await rig.push("stream.frame", { event: { type: "work.node_changed" } });
    await rig.push("stream.frame", null);
    await rig.push("stream.frame", { event: { type: "thread.participant_removed", aggregate_type: "thread", aggregate_id: THREAD, payload: { principal: { kind: "user", id: OWNER } } } });
    expect(rig.container.querySelector(".collab-thread")).toBeNull();
    expect(text(rig)).toContain("You were removed from this thread");
  });

  it("survives an unreadable roster and identity", async () => {
    rig = await mount(<Collaboration workspaceId="ws_x" />, { ...answers, "teammates.list": () => Promise.reject(new Error("x")), "session.me": () => Promise.reject(new Error("y")), "threads.list": () => Promise.reject(new Error("list down")) });
    expect(text(rig)).toContain("list down");
  });
});
