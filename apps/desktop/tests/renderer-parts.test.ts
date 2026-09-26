/**
 * APP-003/APP-004 renderer parts that are pure: message text parsing (content stays data — only
 * http(s) links become anchors), the palette's matching and the thread list's decoding.
 */

import { describe, expect, it } from "vitest";

import { decodeThreadSummaries } from "../src/renderer/App.js";
import { matchItems, type PaletteItem } from "../src/renderer/Palette.js";
import { parseBlocks, parseInline } from "../src/renderer/Prose.js";

describe("message text", () => {
  it("splits paragraphs, lists, headings, quotes and fenced code", () => {
    const blocks = parseBlocks("# Plan\n\nFirst line\nsecond line\n\n- one\n- two\n\n1. a\n2. b\n\n> quoted\n> more\n\n```\ncode <b>\n```");
    expect(blocks).toEqual([
      { kind: "heading", level: 1, text: "Plan" },
      { kind: "paragraph", text: "First line\nsecond line" },
      { kind: "list", ordered: false, items: ["one", "two"] },
      { kind: "list", ordered: true, items: ["a", "b"] },
      { kind: "quote", text: "quoted\nmore" },
      { kind: "code", text: "code <b>" },
    ]);
  });

  it("keeps an unclosed fence's lines as code rather than dropping them", () => {
    expect(parseBlocks("```\nlet x = 1;")).toEqual([{ kind: "code", text: "let x = 1;" }]);
  });

  it("finds inline code, bold and http(s) links, and nothing else", () => {
    expect(parseInline("run `ls` then **stop** at [docs](https://example.com/a) or https://example.org/b.")).toEqual([
      { kind: "text", text: "run " },
      { kind: "code", text: "ls" },
      { kind: "text", text: " then " },
      { kind: "strong", text: "stop" },
      { kind: "text", text: " at " },
      { kind: "link", text: "docs", href: "https://example.com/a" },
      { kind: "text", text: " or " },
      { kind: "link", text: "https://example.org/b", href: "https://example.org/b" },
      { kind: "text", text: "." },
    ]);
  });

  it("never turns a non-http scheme into a link", () => {
    const spans = parseInline("[click](javascript:alert(1)) and file:///etc/passwd and <script>x</script>");
    expect(spans.every((span) => span.kind === "text")).toBe(true);
    expect(spans.map((span) => span.text).join("")).toBe("[click](javascript:alert(1)) and file:///etc/passwd and <script>x</script>");
  });
});

describe("palette matching", () => {
  const item = (label: string, detail: string | null = null): PaletteItem => ({ key: label, group: "g", label, detail, icon: null, run: () => undefined });
  const items = [item("Approvals"), item("Launch checklist", "status page post"), item("Settings")];

  it("matches every typed word against label and detail, keeping order", () => {
    expect(matchItems(items, "").map((entry) => entry.label)).toEqual(["Approvals", "Launch checklist", "Settings"]);
    expect(matchItems(items, "APPR").map((entry) => entry.label)).toEqual(["Approvals"]);
    expect(matchItems(items, "launch post").map((entry) => entry.label)).toEqual(["Launch checklist"]);
    expect(matchItems(items, "launch nothing")).toEqual([]);
  });
});

describe("thread list decoding", () => {
  it("reads attention and activity, and skips rows without an id", () => {
    const decoded = decodeThreadSummaries({
      threads: [
        { id: "thr_1", title: "T", kind: "group", message_count: 4, last_activity_at: "2026-09-26T10:00:00Z", attention: { needs_you: true } },
        { id: "thr_2" },
        { title: "no id" },
      ],
    });
    expect(decoded).toEqual([
      { id: "thr_1", title: "T", kind: "group", messageCount: 4, lastActivityAt: "2026-09-26T10:00:00Z", needsYou: true },
      { id: "thr_2", title: null, kind: "direct", messageCount: 0, lastActivityAt: null, needsYou: false },
    ]);
  });
});
