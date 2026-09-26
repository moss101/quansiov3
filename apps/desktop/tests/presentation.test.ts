/**
 * APP-003/APP-004: how a thread becomes a sidebar row and how a transcript is divided by time —
 * pure, from the projections the renderer already reads.
 */

import { describe, expect, it } from "vitest";

import {
  DIVIDER_GAP_MS,
  byAgentThread,
  dividerDay,
  faceLetter,
  faceTone,
  initials,
  sidebarRow,
  transcript,
  turnInFlight,
  type RosterEntry,
  type ThreadSummary,
} from "../src/conversation/presentation.js";
import type { ConversationMessage } from "../src/conversation/state.js";

const THREAD: ThreadSummary = { id: "thr_1", title: null, kind: "direct", messageCount: 3, lastActivityAt: null, needsYou: false };
const ROSTER: RosterEntry[] = [
  { id: "agt_writer", name: "Writer", agentThreadId: "ath_writer" },
  { id: "agt_scout", name: "scout", agentThreadId: "ath_scout" },
  { id: "agt_idle", name: "Idle", agentThreadId: null },
];

let seq = 0;
function message(authorKind: ConversationMessage["authorKind"], authorId: string, text: string, createdAt: string | null = null): ConversationMessage {
  seq += 1;
  return { id: `msg_${String(seq)}`, threadId: "thr_1", seq, authorKind, authorId, runId: null, turnId: null, text, attachments: [], createdAt };
}

describe("sidebar rows", () => {
  const roster = byAgentThread(ROSTER);

  it("names a thread by its title, else the teammate replying, else how the person began", () => {
    const messages = [message("user", "usr_1", "Draft   the\nlaunch notes"), message("agent", "ath_writer", "On it.")];
    expect(sidebarRow({ ...THREAD, title: "Launch" }, messages, roster, undefined).name).toBe("Launch");
    expect(sidebarRow(THREAD, messages, roster, undefined).name).toBe("Writer");
    expect(sidebarRow(THREAD, messages.slice(0, 1), roster, undefined).name).toBe("Draft the launch notes");
    expect(sidebarRow(THREAD, null, roster, undefined).name).toBeNull();
  });

  it("previews the latest message on one line and truncates a long one", () => {
    const long = "x".repeat(400);
    const row = sidebarRow(THREAD, [message("user", "usr_1", "first"), message("agent", "ath_writer", `a\n\nb ${long}`)], roster, undefined);
    expect(row.preview?.startsWith("a b x")).toBe(true);
    expect(row.preview?.length).toBe(140);
    expect(row.preview?.endsWith("…")).toBe(true);
  });

  it("shows the teammates replying, most recent first, and the latest one receives new messages", () => {
    const messages = [message("agent", "ath_writer", "one"), message("agent", "ath_scout", "two"), message("agent", "ath_writer", "three"), message("agent", "ath_unknown", "four")];
    const row = sidebarRow(THREAD, messages, roster, undefined);
    expect(row.faces.map((face) => face.key)).toEqual(["agt_writer", "agt_scout"]);
    expect(row.faces.map((face) => face.letter)).toEqual(["W", "S"]);
    expect(row.teammateId).toBe("agt_writer");
    expect(row.more).toBe(0);
  });

  it("counts teammates beyond three as more", () => {
    const many: RosterEntry[] = ["a", "b", "c", "d", "e"].map((name) => ({ id: `agt_${name}`, name, agentThreadId: `ath_${name}` }));
    const row = sidebarRow(THREAD, many.map((entry) => message("agent", entry.agentThreadId ?? "", "hi")), byAgentThread(many), undefined);
    expect(row.faces).toHaveLength(3);
    expect(row.more).toBe(2);
  });

  it("is waiting only when the server's attention says so, and unread only after new messages", () => {
    expect(sidebarRow({ ...THREAD, needsYou: true }, null, roster, undefined).waiting).toBe(true);
    expect(sidebarRow(THREAD, null, roster, undefined).waiting).toBe(false);
    expect(sidebarRow(THREAD, null, roster, undefined).unread).toBe(false);
    expect(sidebarRow(THREAD, null, roster, 3).unread).toBe(false);
    expect(sidebarRow(THREAD, null, roster, 2).unread).toBe(true);
  });
});

describe("faces and initials", () => {
  it("keeps a tone per identity and picks letters from names", () => {
    expect(faceTone("agt_writer")).toBe(faceTone("agt_writer"));
    expect(new Set(["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].map(faceTone)).size).toBeGreaterThan(3);
    expect(faceLetter("  ghost")).toBe("G");
    expect(faceLetter("—")).toBe("?");
    expect(initials("Mohsin  Mustafa", "m@example.com")).toBe("MM");
    expect(initials("Ada", null)).toBe("A");
    expect(initials(null, "zed@example.com")).toBe("Z");
    expect(initials(null, null)).toBe("?");
  });
});

describe("transcript time dividers", () => {
  it("divides before the first message and after a long pause, and groups the same author", () => {
    const start = Date.parse("2026-09-26T09:00:00Z");
    const at = (ms: number): string => new Date(start + ms).toISOString();
    const messages = [
      message("user", "usr_1", "a", at(0)),
      message("user", "usr_1", "b", at(60_000)),
      message("agent", "ath_writer", "c", at(120_000)),
      message("agent", "ath_writer", "d", at(120_000 + DIVIDER_GAP_MS)),
      message("agent", "ath_writer", "e", at(120_000 + DIVIDER_GAP_MS + 60_000)),
    ];
    const items = transcript(messages);
    expect(items.map((item) => (item.kind === "divider" ? "|" : `${item.message.text}${item.continues ? "+" : ""}`))).toEqual(["|", "a", "b+", "c", "|", "d", "e+"]);
  });

  it("divides when the day changes, however short the pause", () => {
    const late = new Date(2026, 8, 25, 23, 59).toISOString();
    const early = new Date(2026, 8, 26, 0, 1).toISOString();
    const items = transcript([message("user", "usr_1", "a", late), message("user", "usr_1", "b", early)]);
    expect(items.map((item) => item.kind)).toEqual(["divider", "message", "divider", "message"]);
  });

  it("shows no divider for messages without a time", () => {
    const items = transcript([message("user", "usr_1", "a"), message("user", "usr_1", "b")]);
    expect(items.map((item) => item.kind)).toEqual(["message", "message"]);
  });

  it("names the day relative to now", () => {
    const now = new Date(2026, 8, 26, 18, 0);
    expect(dividerDay(new Date(2026, 8, 26, 1, 0), now)).toBe("today");
    expect(dividerDay(new Date(2026, 8, 25, 23, 0), now)).toBe("yesterday");
    expect(dividerDay(new Date(2026, 8, 21, 12, 0), now)).toBe("week");
    expect(dividerDay(new Date(2026, 8, 1, 12, 0), now)).toBe("date");
  });
});

describe("a turn in flight", () => {
  const run = { runId: "run_1", status: "RUNNING", turnId: "turn_1", streaming: "", stage: null, cards: [] as { stepId: string; turnId: string; kind: string; status: string; ref: string | null }[] };
  const asked = [message("user", "usr_1", "go")];
  const answered = [...asked, message("agent", "ath_writer", "done")];

  it("is in flight until the reply lands, even though the run stays open", () => {
    expect(turnInFlight(run, asked)).toBe(true);
    expect(turnInFlight(run, answered)).toBe(false);
  });

  it("is in flight while streaming, staged or with an unfinished step", () => {
    expect(turnInFlight({ ...run, streaming: "Hel" }, answered)).toBe(true);
    expect(turnInFlight({ ...run, stage: "model_call" }, answered)).toBe(true);
    expect(turnInFlight({ ...run, cards: [{ stepId: "s", turnId: "t", kind: "tool_call", status: "started", ref: "fs.read" }] }, answered)).toBe(true);
    expect(turnInFlight({ ...run, cards: [{ stepId: "s", turnId: "t", kind: "tool_call", status: "completed", ref: "fs.read" }] }, answered)).toBe(false);
  });

  it("is not work while the teammate waits on the person's answer", () => {
    expect(turnInFlight({ ...run, status: "WAITING_QUESTION" }, asked)).toBe(false);
  });
});
