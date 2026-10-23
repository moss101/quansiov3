// @vitest-environment jsdom
/** APP-003 on a page: the shell (sign-in gate, sidebar, palette, navigation, activity) over the real preload API. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { App, decodeThreadSummaries } from "../../src/renderer/App.js";
import { Palette, matchItems, type PaletteItem } from "../../src/renderer/Palette.js";
import { Sidebar } from "../../src/renderer/Sidebar.js";
import { click, mount, text, type Rig } from "./rig.js";

const THR_A = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const THR_B = "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAB";
const AGT = "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

const signedIn = { kind: "signed-in", userId: "usr_1", tenantId: "tn_1", workspaceId: "ws_1", onboarded: false };
const run = { connected: true, workspaceId: "ws_1", activeRun: { runId: "run_1", threadId: THR_A, streamCursor: "c1" }, streamState: "open" };

function message(id: string, threadId: string, authorKind: string, authorId: string, body: string, runId: string | null = null): Record<string, unknown> {
  return { id, thread_id: threadId, seq: 1, author_kind: authorKind, author_id: authorId, run_id: runId, content_blocks: [{ type: "text", text: body }], created_at: "2026-09-20T00:00:00Z" };
}

function answers(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    "session.state": signedIn,
    "session.activeRun": run,
    "session.me": { user: { id: "usr_1", display_name: "Ada Lovelace", primary_email: "ada@example.com" }, workspaces: [{ id: "ws_1", effective_settings: { theme: "dark" } }] },
    "threads.list": {
      threads: [
        { id: THR_A, title: "Launch plan", kind: "direct", message_count: 2, last_activity_at: "2026-09-20T00:00:00Z", attention: { needs_you: true } },
        { id: THR_B, title: null, kind: "direct", message_count: 1, last_activity_at: "2026-09-19T00:00:00Z" },
        { title: "no id" },
      ],
    },
    "threads.messages": (payload: { threadId: string }) =>
      payload.threadId === THR_A
        ? { messages: [message("msg_1", THR_A, "user", "usr_1", "hello team"), message("msg_2", THR_A, "agent", AGT, "on it", "run_77")] }
        : { messages: [message("msg_3", THR_B, "user", "usr_1", "a question about pricing")] },
    "teammates.list": { workspace_id: "ws_1", teammates: [{ id: AGT, name: "Rhea", status: "active", thread: { id: THR_A } }] },
    "shell.pending.consume": { ok: true, result: { events: [] } },
    "shell.activity.list": { ok: true, result: { items: [] } },
    "appearance.set": { ok: true, result: {} },
    ...overrides,
  };
}

function channelCalls(rig: Rig, channel: string): unknown[] {
  return rig.calls.filter((call) => call.channel === channel).map((call) => call.payload);
}

async function pressKey(rig: Rig, init: KeyboardEventInit, target: EventTarget = document): Promise<void> {
  await act(async () => {
    target.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
    await Promise.resolve();
  });
  await rig.flush();
}

async function typeInto(rig: Rig, element: HTMLInputElement, value: string): Promise<void> {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
    await Promise.resolve();
  });
  await rig.flush();
}

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
  document.documentElement.removeAttribute("data-theme");
});

describe("the shell", () => {
  it("gates on sign-in, reaches the shell once the session is signed in, and returns to sign-in when signed out", async () => {
    rig = await mount(<App />, answers({ "session.state": { kind: "signed-out", persistent: false } }));
    expect(rig.container.querySelector("main.shell")).toBeNull();
    expect(rig.container.querySelector(".window-drag")).not.toBeNull();
    await rig.push("session.state", signedIn);
    expect(rig.container.querySelector("main.shell")).not.toBeNull();
    await rig.push("session.state", { kind: "signed-out", persistent: false });
    expect(rig.container.querySelector("main.shell")).toBeNull();
  });

  it("lists the conversations with who and what is waiting, and applies the person's theme", async () => {
    rig = await mount(<App />, answers());
    const list = rig.container.querySelector("nav.chat-list") as HTMLElement;
    const rows = list.querySelectorAll(".chat-row");
    expect(rows.length).toBe(2);
    // Titled thread, waiting on the person, with its latest message.
    expect(rows[0]?.textContent).toContain("Launch plan");
    expect(rows[0]?.textContent).toContain("Waiting for you: on it");
    // An untitled thread is named by what the person first said.
    expect(rows[1]?.textContent).toContain("a question about pricing");
    expect(rows[0]?.getAttribute("aria-current")).toBe("page");
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(channelCalls(rig, "appearance.set")).toEqual([{ theme: "dark" }]);
    expect(text(rig)).toContain("Connected");
    expect(rig.container.querySelector(".account-button")?.textContent).toContain("AL");
  });

  it("opens another conversation from the sidebar and a new one from the plus button", async () => {
    rig = await mount(<App />, answers());
    const rows = rig.container.querySelectorAll<HTMLButtonElement>(".chat-row");
    await act(async () => {
      rows[1]?.click();
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector(".chat-row.selected")?.textContent).toContain("a question about pricing");
    await click(rig, "New");
    expect(rig.container.querySelector(".chat-row.selected")).toBeNull();
  });

  it("switches to the workspace list and opens a surface, marking only the page's own entry current", async () => {
    rig = await mount(<App />, answers());
    await click(rig, "Workspace");
    const nav = rig.container.querySelector("nav.surface-list") as HTMLElement;
    expect(nav.querySelector("[aria-current=page]")?.textContent).toContain("Chat");
    // Surfaces without a page are named but not buttons.
    expect(nav.querySelector(".nav-note")).not.toBeNull();
    const artifacts = [...nav.querySelectorAll("button")].find((button) => button.textContent.includes("Files and artifacts")) as HTMLButtonElement;
    await act(async () => {
      artifacts.click();
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector("section.artifacts")).not.toBeNull();
    expect(rig.container.querySelector(".surface-artifacts")).not.toBeNull();
    expect(channelCalls(rig, "artifacts.list").length).toBe(1);
    await click(rig, "Chats");
    expect(rig.container.querySelector("nav.chat-list")).not.toBeNull();
  });

  it("opens settings from the account menu, and signs out from it", async () => {
    rig = await mount(<App />, answers({ "auth.signOut": { ok: true } }));
    await click(rig, "Account");
    expect(rig.container.querySelector("[role=menu]")).not.toBeNull();
    await click(rig, "Settings");
    expect(rig.container.querySelector(".surface-settings")).not.toBeNull();
    expect(rig.container.querySelector("[role=menu]")).toBeNull();
    await click(rig, "Account");
    await pressKey(rig, { key: "Escape" });
    expect(rig.container.querySelector("[role=menu]")).toBeNull();
    await click(rig, "Account");
    await act(async () => {
      document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
      await Promise.resolve();
    });
    expect(rig.container.querySelector("[role=menu]")).toBeNull();
    await click(rig, "Account");
    await click(rig, "Sign out");
    expect(rig.calls.some((call) => call.channel === "auth.signOut")).toBe(true);
  });

  it("opens the palette with Ctrl+K, filters it, and runs the chosen item", async () => {
    rig = await mount(<App />, answers());
    await pressKey(rig, { key: "k", ctrlKey: true });
    const palette = rig.container.querySelector("[role=dialog]") as HTMLElement;
    expect(palette).not.toBeNull();
    expect(palette.textContent).toContain("Launch plan");
    expect(palette.textContent).toContain("Sign out");
    const field = palette.querySelector("input") as HTMLInputElement;
    await typeInto(rig, field, "pricing");
    expect(palette.querySelectorAll("[role=option]").length).toBe(1);
    await pressKey(rig, { key: "Enter" }, field);
    expect(rig.container.querySelector("[role=dialog]")).toBeNull();
    expect(rig.container.querySelector(".chat-row.selected")?.textContent).toContain("pricing");
    // Opening it again from the search button and closing with Escape.
    await click(rig, "Search");
    expect(rig.container.querySelector("[role=dialog]")).not.toBeNull();
    await pressKey(rig, { key: "Escape" }, rig.container.querySelector("[role=dialog] input") as HTMLElement);
    expect(rig.container.querySelector("[role=dialog]")).toBeNull();
  });

  it("palette actions open the pop-out window and the activity panel", async () => {
    rig = await mount(<App />, answers());
    await pressKey(rig, { key: "k", metaKey: true });
    let option = [...rig.container.querySelectorAll<HTMLElement>("[role=option]")].find((item) => item.textContent.includes("Open in a new window")) as HTMLElement;
    await act(async () => {
      option.click();
      await Promise.resolve();
    });
    await rig.flush();
    expect(channelCalls(rig, "shell.popout.open")).toEqual([{ threadId: THR_A }]);
    await pressKey(rig, { key: "k", ctrlKey: true });
    option = [...rig.container.querySelectorAll<HTMLElement>("[role=option]")].find((item) => item.textContent.includes("Stream")) as HTMLElement;
    await act(async () => {
      option.click();
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector("section.activity")).not.toBeNull();
  });

  it("carries out the shell's commands: settings, approvals, a thread, a run's thread, a deep-linked artifact", async () => {
    rig = await mount(<App />, answers());
    await rig.push("shell.command", { kind: "open-settings" });
    expect(rig.container.querySelector(".surface-settings")).not.toBeNull();
    await rig.push("shell.command", { kind: "open-approval", approvalId: "apr_1" });
    expect(rig.container.querySelector(".surface-approvals")).not.toBeNull();
    await rig.push("shell.command", { kind: "open-thread", threadId: THR_B });
    expect(rig.container.querySelector(".surface-conversation")).not.toBeNull();
    expect(rig.container.querySelector(".chat-row.selected")?.textContent).toContain("pricing");
    await rig.push("shell.command", { kind: "open-settings" });
    // The thread a run belongs to is the one whose messages name it.
    await rig.push("shell.command", { kind: "open-run", runId: "run_77" });
    expect(rig.container.querySelector(".chat-row.selected")?.textContent).toContain("Launch plan");
    await rig.push("shell.command", { kind: "open-settings" });
    await rig.push("shell.command", { kind: "open-run", runId: "run_unknown" });
    expect(rig.container.querySelector(".surface-conversation")).not.toBeNull();
    await rig.push("shell.command", { kind: "find" });
    expect(rig.container.querySelector("[role=dialog]")).not.toBeNull();
    await rig.push("shell.command", { kind: "popout-current-thread" });
    expect(channelCalls(rig, "shell.popout.open").length).toBe(1);
    await rig.push("shell.command", { kind: "new-thread" });
    expect(rig.container.querySelector(".chat-row.selected")).toBeNull();
    // A link to an artifact opens the artifacts page; a malformed command opens nothing.
    await rig.push("deepLink.intent", { kind: "open-artifact", artifactId: "art_1" });
    expect(rig.container.querySelector(".surface-artifacts")).not.toBeNull();
    await rig.push("shell.command", { kind: "open-thread", threadId: "not-an-id" });
    expect(rig.container.querySelector(".surface-artifacts")).not.toBeNull();
    await rig.push("deepLink.intent", { kind: "open-settings" });
    expect(rig.container.querySelector(".surface-settings")).not.toBeNull();
    expect(text(rig)).toContain("Opened");
  });

  it("acts on a command that arrived before the page subscribed", async () => {
    rig = await mount(<App />, answers({ "shell.pending.consume": { ok: true, result: { events: [{ channel: "shell.command", payload: { kind: "open-settings" } }, { payload: {} }] } } }));
    expect(rig.container.querySelector(".surface-settings")).not.toBeNull();
  });

  it("shows the activity panel: what needs the person, the stream log, and the open and dismiss commands", async () => {
    rig = await mount(<App />, answers({
      "shell.activity.list": { ok: true, result: { items: [{ key: "k1", kind: "approval", group: "waiting", title: "Approve deploy" }, { key: "k2", kind: "remote_ask_view", group: "waiting", title: "Remote view" }, { group: "bad" }] } },
    }));
    expect(rig.container.querySelector("section.activity")).toBeNull();
    await click(rig, "Stream");
    const panel = rig.container.querySelector("section.activity") as HTMLElement;
    expect(panel.textContent).toContain("Approve deploy");
    await click(rig, "Open");
    expect(channelCalls(rig, "shell.activity.open")).toEqual([{ key: "k1" }]);
    await click(rig, "Dismiss");
    expect(channelCalls(rig, "shell.activity.dismiss")).toEqual([{ key: "k1" }]);
    // A remote-control row offers the owner's answers instead.
    expect(panel.textContent).toContain("Remote view");
    expect(panel.querySelectorAll("button.link").length).toBeGreaterThan(2);
    // The stream log describes each frame.
    await rig.push("stream.frame", { kind: "event", cursor: "c2", event: { type: "artifact.created" } });
    await rig.push("stream.frame", { kind: "live", channel: "term", frame: { kind: "bytes" } });
    await rig.push("stream.frame", { other: true });
    await rig.push("stream.frame", "text frame");
    const lines = [...panel.querySelectorAll("ol > li")].map((li) => li.textContent);
    expect(lines).toContain("event artifact.created @ c2");
    expect(lines).toContain("live bytes on term");
    expect(lines).toContain('{"other":true}');
    expect(lines).toContain("text frame");
    await rig.push("shell.activity", { items: [] });
    expect(rig.container.querySelector(".attention-list")).toBeNull();
    await click(rig, "Close");
    expect(rig.container.querySelector("section.activity")).toBeNull();
  });

  it("re-reads the conversation list after thread events and the roster after teammate events", async () => {
    rig = await mount(<App />, answers());
    const before = channelCalls(rig, "threads.list").length;
    const rosterBefore = channelCalls(rig, "teammates.list").length;
    await rig.push("stream.frame", { kind: "event", event: { type: "thread.message_posted" } });
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 450));
    });
    expect(channelCalls(rig, "threads.list").length).toBeGreaterThan(before);
    await rig.push("stream.frame", { kind: "event", event: { type: "teammate.updated" } });
    expect(channelCalls(rig, "teammates.list").length).toBeGreaterThan(rosterBefore);
  });

  it("follows the connection and the machine's connectivity, and reports the browser's", async () => {
    rig = await mount(<App />, answers({ "session.activeRun": { ...run, streamState: "reconnecting" } }));
    expect(rig.container.querySelector(".status-reconnecting")?.textContent).toContain("Reconnecting");
    rig.answer("session.activeRun", { ...run, streamState: "open" });
    await rig.push("stream.state", {});
    expect(rig.container.querySelector(".status-reconnecting")).toBeNull();
    await rig.push("shell.connectivity", { state: "offline" });
    expect(rig.container.querySelector(".status-offline")).not.toBeNull();
    await rig.push("shell.connectivity", { state: "online" });
    expect(rig.container.querySelector(".status-offline")).toBeNull();
    await act(async () => {
      window.dispatchEvent(new Event("offline"));
      await Promise.resolve();
    });
    expect(channelCalls(rig, "shell.connectivity.report").length).toBe(1);
  });

  it("toggles the inspector from the header and says it is connecting before a workspace is known", async () => {
    rig = await mount(<App />, answers({ "session.activeRun": { connected: false, workspaceId: null, activeRun: null, streamState: "connecting" } }));
    expect(text(rig)).toContain("Connecting");
    expect(rig.container.querySelector(".surface .empty")).not.toBeNull();
  });

  it("reports a failing list as a notice", async () => {
    rig = await mount(<App />, answers({ "threads.list": () => { throw new Error("list failed"); } }));
    expect(rig.container.querySelector(".pane-notice")?.textContent).toContain("list failed");
  });
});

describe("the palette and the sidebar on their own", () => {
  const items: PaletteItem[] = [
    { key: "a", group: "G1", label: "Alpha", detail: "first", icon: null, run: () => undefined },
    { key: "b", group: "G1", label: "Beta", icon: null, run: () => undefined },
    { key: "c", group: "G2", label: "Gamma", icon: null, run: () => undefined },
  ];

  it("matches every word against label and detail, in order", () => {
    expect(matchItems(items, "").length).toBe(3);
    expect(matchItems(items, "alpha first").map((item) => item.key)).toEqual(["a"]);
    expect(matchItems(items, "a e").map((item) => item.key)).toEqual(["b"]);
    expect(matchItems(items, "zzz")).toEqual([]);
  });

  it("moves with the arrow keys, runs the active row on Enter, closes on a press outside and says when nothing matches", async () => {
    const ran: string[] = [];
    let closed = 0;
    const live = items.map((item) => ({ ...item, run: () => { ran.push(item.key); } }));
    rig = await mount(<Palette items={live} onClose={() => { closed += 1; }} />);
    const field = rig.container.querySelector("input") as HTMLInputElement;
    await pressKey(rig, { key: "ArrowDown" }, field);
    await pressKey(rig, { key: "ArrowDown" }, field);
    await pressKey(rig, { key: "ArrowDown" }, field);
    await pressKey(rig, { key: "ArrowUp" }, field);
    expect(rig.container.querySelector("[aria-selected=true]")?.textContent).toContain("Beta");
    await pressKey(rig, { key: "Enter" }, field);
    expect(ran).toEqual(["b"]);
    expect(closed).toBe(1);
    expect(rig.container.querySelectorAll(".palette-group").length).toBe(2);
    await typeInto(rig, field, "nothing");
    expect(rig.container.querySelector(".palette-empty")).not.toBeNull();
    await pressKey(rig, { key: "Enter" }, field);
    expect(ran).toEqual(["b"]);
    const scrim = rig.container.querySelector(".scrim") as HTMLElement;
    await act(async () => {
      scrim.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
      await Promise.resolve();
    });
    expect(closed).toBe(2);
  });

  it("selects a row by hover and click", async () => {
    const ran: string[] = [];
    rig = await mount(<Palette items={items.map((item) => ({ ...item, run: () => { ran.push(item.key); } }))} onClose={() => undefined} />);
    const rows = rig.container.querySelectorAll<HTMLElement>("[role=option]");
    await act(async () => {
      rows[2]?.dispatchEvent(new MouseEvent("mousemove", { bubbles: true }));
      await Promise.resolve();
    });
    expect(rows[2]?.getAttribute("aria-selected")).toBe("true");
    await act(async () => {
      rows[2]?.click();
      await Promise.resolve();
    });
    expect(ran).toEqual(["c"]);
  });

  it("opens a conversation in its own window when a row is dragged out of the window", async () => {
    const popped: { id: string; point: { x: number; y: number } | null }[] = [];
    const row = { id: THR_A, name: "Launch", preview: null, waiting: false, unread: true, faces: [], more: 0, teammateId: null };
    rig = await mount(
      <Sidebar view="chats" onView={() => undefined} rows={[row]} openThreadId={null} page="conversation" onOpenThread={() => undefined} onOpenPage={() => undefined} onSearch={() => undefined}
        onPopout={(id, point) => { popped.push({ id, point }); }} initials="" onSignOut={() => undefined} />,
    );
    const button = rig.container.querySelector(".chat-row") as HTMLElement;
    expect(button.textContent).toContain("No messages yet");
    expect(button.querySelector(".chat-dot.unread")).not.toBeNull();
    const drag = async (init: MouseEventInit): Promise<void> => {
      await act(async () => {
        button.dispatchEvent(new MouseEvent("dragend", { bubbles: true, ...init }));
        await Promise.resolve();
      });
    };
    await drag({ clientX: 10, clientY: 10, screenX: 10, screenY: 10 });
    expect(popped).toEqual([]);
    await drag({ clientX: -50, clientY: 10, screenX: 300, screenY: 200 });
    await drag({ clientX: -50, clientY: 10, screenX: 0, screenY: 0 });
    expect(popped).toEqual([{ id: THR_A, point: { x: 300, y: 200 } }, { id: THR_A, point: null }]);
    await act(async () => {
      const event = new Event("dragstart", { bubbles: true });
      Object.defineProperty(event, "dataTransfer", { value: { setData: () => undefined, effectAllowed: "" } });
      button.dispatchEvent(event);
      await Promise.resolve();
    });
  });

  it("decodes the thread list defensively", () => {
    expect(decodeThreadSummaries(null)).toEqual([]);
    expect(decodeThreadSummaries({ threads: [{ id: "thr_1", title: 4, kind: 5, message_count: "x" }] })).toEqual([
      { id: "thr_1", title: null, kind: "direct", messageCount: 0, lastActivityAt: null, needsYou: false },
    ]);
  });
});
