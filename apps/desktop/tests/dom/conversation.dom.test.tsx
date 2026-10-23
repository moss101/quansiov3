// @vitest-environment jsdom
/** APP-004 on a page: the Conversation view (transcript, composer, questions, inspector) over the real preload API. */

import { act, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { t } from "../../src/i18n/strings.js";
import { Conversation } from "../../src/renderer/Conversation.js";
import { click, mount, text, type, type Rig } from "./rig.js";

const THREAD = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const TURN = "trn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const QUESTION = "q_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const ARTIFACT = "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

const roster = [
  { id: "tm_a", name: "Ada", agentThreadId: "ath_a" },
  { id: "tm_b", name: "Bo", agentThreadId: "ath_b" },
];

function message(id: string, seq: number, authorKind: string, authorId: string, body: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return { id, thread_id: THREAD, seq, author_kind: authorKind, author_id: authorId, content_blocks: [{ type: "text", text: body }], created_at: "2026-09-20T10:00:00Z", ...extra };
}

function question(kind: string, options: string[] = []): Record<string, unknown> {
  return { id: QUESTION, run_id: RUN, kind, prompt: "Which one?", options, required: true, status: "open", answer: null };
}

const timeline = {
  run: { id: RUN, work_node_id: "wn_1", status: "SUCCEEDED", trigger: { kind: "message" } },
  trigger_message: { message_id: "msg_1", attachments: [ARTIFACT] },
  turns: [
    {
      id: TURN, seq: 1, input: { kind: "message" }, status: "completed",
      context: { sources: [{ kind: "thread", source: THREAD, trust_level: 2 }] },
      steps: [
        { kind: "model_call", model_call: { route: { provider: "in-process", model_id: "stub", chosen_by: "seam" } } },
        { kind: "tool_call", tool_calls: [{ tool_name: "fs.read", status: "completed", effect_class: "read.internal", effect_status: "settled" }] },
        { kind: "wait", question: { id: QUESTION, prompt: "Which?", status: "answered", answer: { value: "alpha" } } },
      ],
    },
  ],
};

/** The same run while its turn is still going: the timeline is also the run's current state. */
const runningTimeline = { ...timeline, run: { ...timeline.run, status: "RUNNING" } };

interface Props { threadId: string | null; inspectorOpen: boolean; teammateName: string | null }
const spies = { onThreadChange: vi.fn(), onThreadsChanged: vi.fn(() => Promise.resolve()), onInspectorOpen: vi.fn() };

/** The shell's side of the contract: it follows the thread the page reports, as the real shell does. */
function Host({ initial, inspectorOpen, teammateName }: { initial: string | null; inspectorOpen: boolean; teammateName: string | null }): React.ReactElement {
  const [threadId, setThreadId] = useState<string | null>(initial);
  return (
    <Conversation
      workspaceId="ws_1" threadId={threadId} teammateId="tm_a" teammateName={teammateName} threadFace={null}
      roster={roster} personId="usr_me"
      onThreadChange={(next) => { spies.onThreadChange(next); setThreadId(next); }}
      onThreadsChanged={spies.onThreadsChanged}
      inspectorOpen={inspectorOpen} onInspectorOpen={spies.onInspectorOpen}
    />
  );
}

function view(overrides: Partial<Props> = {}): React.ReactElement {
  const props: Props = { threadId: THREAD, inspectorOpen: false, teammateName: null, ...overrides };
  return <Host initial={props.threadId} inspectorOpen={props.inspectorOpen} teammateName={props.teammateName} />;
}

const thread = {
  "threads.messages": {
    messages: [
      message("msg_1", 1, "user", "usr_me", "hello **there**\n- one\n- two", { run_id: RUN, attachments: [ARTIFACT] }),
      message("msg_2", 2, "agent", "ath_a", "reply with `code` and https://example.com/x", { run_id: RUN, turn_id: TURN }),
      message("msg_3", 3, "agent", "ath_b", "another voice"),
      message("msg_4", 4, "system", "sys", "run started", { run_id: RUN }),
    ],
  },
  "runs.timeline": timeline,
};

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
  vi.clearAllMocks();
});

function textarea(): HTMLTextAreaElement {
  return rig.container.querySelector("textarea") as HTMLTextAreaElement;
}

describe("the transcript", () => {
  it("lays out grouped bubbles, names for several voices, markdown, attachments and system lines", async () => {
    rig = await mount(view(), thread);
    expect(rig.calls.some((call) => call.channel === "threads.messages")).toBe(true);
    expect(spies.onThreadsChanged).toHaveBeenCalled();
    const page = text(rig);
    expect(page).toContain("hello");
    expect(rig.container.querySelector("strong")?.textContent).toBe("there");
    expect(rig.container.querySelectorAll(".prose li").length).toBe(2);
    expect(rig.container.querySelector("code")?.textContent).toBe("code");
    expect(rig.container.querySelector("a")?.getAttribute("href")).toBe("https://example.com/x");
    expect(rig.container.querySelector(".attachments")?.textContent).toBe(ARTIFACT);
    expect(rig.container.querySelector(".meta-event")?.textContent).toBe("run started");
    expect(rig.container.querySelector(".meta-time time")).not.toBeNull();
    // Two teammates speak, so their names are shown; the reader's own message is not labelled.
    const senders = [...rig.container.querySelectorAll(".sender")].map((node) => node.textContent);
    expect(senders).toEqual(["Ada", "Bo"]);
  });

  it("keeps a new thread empty, offers the addressee choice and routes the first post to the chosen teammate", async () => {
    rig = await mount(view({ threadId: null }));
    expect(text(rig)).toContain(t("emptyThreadTitle"));
    await click(rig, "Bo");
    expect(textarea().placeholder).toBe(`${t("messageTo")} Bo`);
    expect(rig.container.querySelector("button[aria-pressed=true]")?.textContent).toContain("Bo");
    await click(rig, t("anyTeammate"));
    expect(textarea().placeholder).toBe(t("composerPlaceholder"));
  });
});

describe("the composer", () => {
  it("posts on Enter (not Shift+Enter), clears, inspects the new run and reports the new thread", async () => {
    rig = await mount(view({ threadId: null }), {
      "messages.post": { thread_id: THREAD, run_id: RUN, message_id: "msg_9", run_status: "RUNNING" },
      "runs.timeline": timeline,
    });
    const send = rig.container.querySelector<HTMLButtonElement>("button.send");
    expect(send?.disabled).toBe(true);
    await click(rig, "Bo");
    await type(rig, "Message", "  ship it  ");
    expect(send?.disabled).toBe(false);
    const field = textarea();
    await act(async () => {
      field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true, bubbles: true, cancelable: true }));
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.calls.some((call) => call.channel === "messages.post")).toBe(false);
    await act(async () => {
      field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      await Promise.resolve();
    });
    await rig.flush();
    const posted = rig.calls.find((call) => call.channel === "messages.post");
    expect(posted).toBeDefined();
    expect(JSON.stringify(posted?.payload)).toContain("ship it");
    expect(JSON.stringify(posted?.payload)).toContain("tm_b");
    expect(textarea().value).toBe("");
    expect(spies.onThreadChange).toHaveBeenCalledWith(THREAD);
    expect(rig.calls.some((call) => call.channel === "runs.timeline")).toBe(true);
  });

  it("shows the refusal of a post that fails and keeps the typed text", async () => {
    rig = await mount(view(), { ...thread, "messages.post": {} });
    await type(rig, "Message", "hi");
    await click(rig, t("send"));
    // The answer has no thread/run/message identities, so the post is refused.
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("PostMessage did not return");
    expect(textarea().value).toBe("hi");
  });

  it("attaches files from disk, says what was skipped, and removes a chip", async () => {
    rig = await mount(view(), {
      ...thread,
      "files.pick": { ok: true, result: { grant: { token: "tok", files: [{ index: 0, name: "a.txt", sizeBytes: 3 }] } } },
      "files.grant.attach": { ok: true, result: { attached: [{ artifact_id: ARTIFACT, title: "a.txt", size_bytes: 3 }], skipped: [{ name: "b.bin", message: "too big" }] } },
    });
    await click(rig, t("attachFromDisk"));
    expect(rig.container.querySelector(".chip")?.textContent).toContain("a.txt");
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("b.bin: too big");
    expect(JSON.stringify(rig.calls.find((call) => call.channel === "files.grant.attach")?.payload)).toContain("tok");
    await click(rig, t("remove"));
    expect(rig.container.querySelector(".chip")).toBeNull();
    // The notice closes.
    await click(rig, t("close"));
    expect(rig.container.querySelector("[role=alert]")).toBeNull();
  });

  it("says why an attach was refused, and a cancelled dialog is silent", async () => {
    rig = await mount(view(), { ...thread, "files.pick": { ok: false, error: { code: "FILE_GRANT_INVALID", message: "nope", details: { reason: "expired" } } } });
    await click(rig, t("attachFromDisk"));
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain(t("attachGrantExpired"));
    rig.answer("files.pick", { ok: true, result: {} });
    await click(rig, t("close"));
    await click(rig, t("attachFromDisk"));
    expect(rig.container.querySelector("[role=alert]")).toBeNull();
  });

  it("attaches the grant a deep link pushed, and shows its failure", async () => {
    rig = await mount(view(), {
      ...thread,
      "files.grant.attach": { ok: true, result: { attached: [{ artifact_id: ARTIFACT, title: "pushed.txt", size_bytes: 1 }], skipped: [] } },
    });
    await rig.push("files.grant", { grant: { token: "t2", files: [] }, threadId: THREAD });
    expect(rig.container.querySelector(".chip")?.textContent).toContain("pushed.txt");
    await rig.push("files.grant", { error: { code: "FILE_GRANT_INVALID", message: "denied by main" } });
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("denied by main");
  });
});

describe("a turn in flight", () => {
  it("shows the working row, then the streamed reply, and cancels the turn", async () => {
    rig = await mount(view({ threadId: null }), { "messages.post": { thread_id: THREAD, run_id: RUN, message_id: "msg_9", run_status: "RUNNING" }, "runs.timeline": runningTimeline, "turns.cancel": { ok: true } });
    await type(rig, "Message", "go");
    await click(rig, t("send"));
    expect(rig.container.querySelector(".working")?.textContent).toContain(t("working"));
    await rig.push("stream.frame", { kind: "event", cursor: "c1", event: { type: "turn.started", aggregate_id: RUN, payload: { run_id: RUN, turn_id: TURN } } });
    await rig.push("stream.frame", { kind: "event", cursor: "c2", event: { type: "step.created", aggregate_id: RUN, payload: { run_id: RUN, turn_id: TURN, step_id: "stp_1", kind: "tool_call", status: "pending", ref: "fs.read" } } });
    expect(rig.container.querySelector(".working")?.textContent).toContain("fs.read");
    await rig.push("stream.frame", { kind: "live", channel: "workspace:ws_1", frame: { kind: "model.delta", run_id: RUN, turn_id: TURN, text: "partial answer" } });
    expect(rig.container.querySelector(".streaming")?.textContent).toContain("partial answer");
    await click(rig, t("cancelTurn"));
    expect(rig.calls.find((call) => call.channel === "turns.cancel")?.payload).toMatchObject({ runId: RUN });
  });

  it("reads a refused cancel as a notice", async () => {
    rig = await mount(view({ threadId: null }), { "messages.post": { thread_id: THREAD, run_id: RUN, message_id: "msg_9" }, "runs.timeline": runningTimeline, "turns.cancel": () => { throw new Error("cancel refused"); } });
    await type(rig, "Message", "go");
    await click(rig, t("send"));
    await click(rig, t("cancelTurn"));
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("cancel refused");
  });

  it("re-reads messages when the thread gains one on the stream", async () => {
    rig = await mount(view(), thread);
    const before = rig.calls.filter((call) => call.channel === "threads.messages").length;
    await rig.push("stream.frame", { kind: "event", cursor: "c9", event: { type: "thread.message_posted", aggregate_id: THREAD, payload: { thread_id: THREAD } } });
    expect(rig.calls.filter((call) => call.channel === "threads.messages").length).toBeGreaterThan(before);
  });
});

describe("agent questions", () => {
  async function open(kind: string, options: string[] = []): Promise<void> {
    rig = await mount(view(), { ...thread, "questions.list": { questions: [question(kind, options)] }, "questions.answer": { ok: true } });
    await rig.push("stream.frame", { kind: "event", cursor: "c3", event: { type: "question.asked", aggregate_id: RUN, payload: { run_id: RUN, question_id: QUESTION } } });
  }
  const answerPayload = (): unknown => rig.calls.find((call) => call.channel === "questions.answer")?.payload;

  it("answers a confirm card yes and no", async () => {
    await open("confirm");
    expect(rig.container.querySelector("[data-question]")?.textContent).toContain("Which one?");
    await click(rig, t("yes"));
    expect(answerPayload()).toMatchObject({ questionId: QUESTION, value: true });
    await click(rig, t("no"));
    expect(rig.calls.filter((call) => call.channel === "questions.answer").pop()?.payload).toMatchObject({ value: false });
  });

  it("sends a single choice only once one is chosen, and every box ticked for a multi choice", async () => {
    await open("single_choice", ["alpha", "beta"]);
    const answerButton = [...rig.container.querySelectorAll<HTMLButtonElement>("button.primary")].find((button) => button.textContent === t("answer"));
    expect(answerButton?.disabled).toBe(true);
    await click(rig, "beta");
    expect(answerButton?.disabled).toBe(false);
    await click(rig, t("answer"));
    expect(answerPayload()).toMatchObject({ value: "beta" });
    await rig.unmount();

    await open("multi_choice", ["x", "y", "z"]);
    await click(rig, "x");
    await click(rig, "z");
    await click(rig, "x");
    await click(rig, "y");
    await click(rig, t("answer"));
    expect(answerPayload()).toMatchObject({ value: ["z", "y"] });
  });

  it("sends free text trimmed and not an empty one", async () => {
    await open("free_text");
    const submit = [...rig.container.querySelectorAll<HTMLButtonElement>("button.primary")].find((button) => button.textContent === t("answer"));
    expect(submit?.disabled).toBe(true);
    await type(rig, "Which one?", "  blue  ");
    await click(rig, t("answer"));
    expect(answerPayload()).toMatchObject({ value: "blue" });
  });

  it("shows a failed answer as a notice", async () => {
    await open("confirm");
    rig.answer("questions.answer", () => { throw new Error("answer refused"); });
    await click(rig, t("yes"));
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("answer refused");
  });
});

describe("the details pane", () => {
  it("opens from a message, reads the run timeline and shows route, sources, tools and the question", async () => {
    rig = await mount(view({ inspectorOpen: true }), thread);
    expect(text(rig)).toContain(t("inspectorEmpty"));
    await click(rig, t("inspectTurn"));
    expect(spies.onInspectorOpen).toHaveBeenCalledWith(true);
    const pane = rig.container.querySelector(".inspection");
    expect(pane?.getAttribute("data-run")).toBe(RUN);
    const body = pane?.textContent ?? "";
    expect(body).toContain("in-process/stub (seam)");
    expect(body).toContain("thread");
    expect(body).toContain("fs.read · completed · read.internal (settled)");
    expect(body).toContain('"alpha"');
    expect(body).toContain(ARTIFACT);
    await click(rig, t("close"));
    expect(spies.onInspectorOpen).toHaveBeenCalledWith(false);
  });

  it("refuses a malformed timeline with a notice", async () => {
    rig = await mount(view({ inspectorOpen: true }), { ...thread, "runs.timeline": {} });
    await click(rig, t("inspectTurn"));
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("timeline has no run");
  });
});
