// @vitest-environment jsdom
/** APP-008 on a page: the Artifacts workspace over the real preload API, answering from a table of projections. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { Artifacts } from "../../src/renderer/Artifacts.js";
import { click, mount, text, type, type Rig } from "./rig.js";

const ART = "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const PAGES = "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAB";
const V1 = "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const V2 = "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";

function b64(value: string): string {
  return Buffer.from(value, "utf8").toString("base64");
}

function version(id: string, seq: number, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, seq, content_digest: `digest-${id}`, size_bytes: 20, media_type: "text/markdown",
    produced_by: { run_id: null, step_id: null, user_id: "usr_1" }, evidence_ids: [], parent_version_id: null,
    format_check: { ok: true, rule: "artifact.source", detail: "" }, created_at: "2026-09-20T00:00:00Z", ...extra,
  };
}

const quality = {
  record_version: 1, ok: true, adapter: "docx", adapter_version: "1", trust: "UNTRUSTED_EXTERNAL",
  inputs: [], checks: [{ rule: "render", ok: true, detail: "fine", subject: null }], environment: {}, evidence_ids: ["ev_q"],
};

const history = {
  artifact: { id: ART, title: "Plan", kind: "document", origin: { kind: "run", ref: "run_1" }, current_version_id: V2 },
  versions: [
    version(V1, 1),
    version(V2, 2, { parent_version_id: V1, produced_by: { run_id: "run_9", step_id: null, user_id: null }, evidence_ids: ["ev_1"], quality_record: quality }),
  ],
};

const contents: Record<string, unknown> = {
  [V1]: { bytesBase64: b64("alpha\nbeta"), mediaType: "text/markdown", sizeBytes: 10, plan: { mode: "text", reason: "" } },
  [V2]: { bytesBase64: b64("alpha\ngamma"), mediaType: "text/markdown", sizeBytes: 11, plan: { mode: "text", reason: "" } },
};

function base(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    "artifacts.list": {
      artifacts: [
        { id: ART, kind: "document", title: "Plan", version_count: 2 },
        { id: PAGES, kind: "data", title: "Pages", role: `deck_pages:${ART}`, version_count: 1 },
        { title: "no id" },
      ],
    },
    "artifacts.versions": history,
    "artifacts.content": (payload: { versionId: string }) => contents[payload.versionId],
    "evidence.get": (payload: { evidenceId: string }) => ({ id: payload.evidenceId, kind: "tool_output", run_id: "run_9", inline_summary: "ran fine" }),
    "artifacts.save": { path: "/tmp/plan.md" },
    "artifacts.open": {},
    "artifacts.upload": { version_id: "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAA3" },
    "search.query": {
      program: { q: "plan", channels: ["artifacts", "threads"] },
      hits: [
        { channel: "artifacts", id: ART, title: "Plan hit", snippet: "the plan", provenance: { locator: "loc:1" } },
        { channel: "threads", id: "thr_1", title: "Thread hit", snippet: "talk", ref: { run_id: "run_5" }, provenance: { locator: "loc:2" } },
        { channel: "knowledge", id: "kn_1", title: "Fact hit", snippet: "fact", provenance: {} },
      ],
      truncated: true,
    },
    ...overrides,
  };
}

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

describe("the Artifacts view", () => {
  it("lists artifacts, opens one, shows its history, preview, evidence and quality, and sends open/save", async () => {
    const inspected: string[] = [];
    rig = await mount(<Artifacts workspaceId="ws_1" onInspectRun={(id) => { inspected.push(id); }} />, base());
    expect(text(rig)).toContain("Plan");
    expect(text(rig)).toContain("Pages");
    await click(rig, /Plan/);
    expect(rig.calls.find((call) => call.channel === "artifacts.versions")?.payload).toEqual({ artifactId: ART });
    const page = text(rig);
    expect(page).toContain("gamma");
    expect(page).toContain("ran fine");
    expect(page).toContain("valid source");
    expect(rig.container.querySelector(".quality-label")?.getAttribute("data-ok")).toBe("true");
    expect(rig.container.querySelector("[data-evidence=ev_1]")?.textContent).toContain("tool_output");
    // The produced-by run and the evidence's run both jump to the timeline.
    await click(rig, /Run run_9/);
    await click(rig, /^Run$/);
    expect(inspected).toEqual(["run_9", "run_9"]);
    await click(rig, /Save/);
    expect(text(rig)).toContain("/tmp/plan.md");
    await click(rig, /Open/);
    expect(rig.calls.some((call) => call.channel === "artifacts.open")).toBe(true);
  });

  it("shows a diff against the parent version and clears it", async () => {
    rig = await mount(<Artifacts workspaceId="ws_1" />, base());
    await click(rig, /Plan/);
    expect(rig.container.querySelector(".diff")?.getAttribute("data-compare")).toBe(V1);
    await click(rig, /Compare/);
    const diff = rig.container.querySelector(".diff");
    expect(diff?.querySelector(".line-removed")?.textContent).toContain("beta");
    expect(diff?.querySelector(".line-added")?.textContent).toContain("gamma");
    // The newest version is diffed against its parent by default, so the diff is shown before any explicit compare.
    await click(rig, /Clear/i);
    expect(rig.container.querySelector(".diff")?.getAttribute("data-compare")).toBe(V1);
  });

  it("edits a text source and saves it as the next version", async () => {
    rig = await mount(<Artifacts workspaceId="ws_1" />, base());
    await click(rig, /Plan/);
    await click(rig, /Edit/);
    await type(rig, "Edit", "alpha\ndelta");
    await click(rig, /new version/i);
    const upload = rig.calls.find((call) => call.channel === "artifacts.upload")?.payload as { artifactId: string; bytesBase64: string; name: string };
    expect(upload.artifactId).toBe(ART);
    expect(upload.name).toBe("Plan");
    expect(Buffer.from(upload.bytesBase64, "base64").toString("utf8")).toBe("alpha\ndelta");
    expect(rig.container.querySelector(".edit-source")).toBeNull();
  });

  it("cancels an edit without uploading", async () => {
    rig = await mount(<Artifacts workspaceId="ws_1" />, base());
    await click(rig, /Plan/);
    await click(rig, /Edit/);
    await click(rig, /Cancel/i);
    expect(rig.container.querySelector(".edit-source")).toBeNull();
    expect(rig.calls.some((call) => call.channel === "artifacts.upload")).toBe(false);
  });

  it("searches with the chosen channels and opens hits", async () => {
    const inspected: string[] = [];
    rig = await mount(<Artifacts workspaceId="ws_1" onInspectRun={(id) => { inspected.push(id); }} />, base());
    const submit = rig.container.querySelector<HTMLButtonElement>("form.search button[type=submit]");
    expect(submit?.disabled).toBe(true);
    await type(rig, "earch", "plan");
    // Unchecking a channel narrows the program.
    const boxes = rig.container.querySelectorAll<HTMLInputElement>(".channels input");
    boxes[3]?.click();
    await rig.flush();
    (rig.container.querySelector("form.search") as HTMLFormElement).requestSubmit();
    await rig.flush();
    expect(rig.calls.find((call) => call.channel === "search.query")?.payload).toEqual({ q: "plan", channels: ["artifacts", "threads", "knowledge"] });
    expect(rig.container.querySelector(".results")?.getAttribute("data-truncated")).toBe("true");
    expect(text(rig)).toContain("(3+)");
    await click(rig, "Thread hit");
    expect(inspected).toEqual(["run_5"]);
    await click(rig, "Plan hit");
    expect(rig.calls.some((call) => call.channel === "artifacts.versions")).toBe(true);
    await click(rig, /^Clear$/);
    expect(rig.container.querySelector(".results")).toBeNull();
  });

  it("filters by kind, and refreshes on an artifact event", async () => {
    rig = await mount(<Artifacts workspaceId="ws_1" />, base());
    const select = rig.container.querySelector("select") as HTMLSelectElement;
    const { set } = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value") as { set: (v: string) => void };
    await act(async () => {
      set.call(select, "data");
      select.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
    });
    await rig.flush();
    const lists = rig.calls.filter((call) => call.channel === "artifacts.list");
    expect(lists[lists.length - 1]?.payload).toEqual({ kind: "data", runId: null });
    await rig.push("stream.frame", { event: { type: "artifact.version_created", aggregate_id: ART } });
    expect(rig.calls.filter((call) => call.channel === "artifacts.list").length).toBeGreaterThan(lists.length);
    await rig.push("stream.frame", "not an object");
  });

  it("shows refusals as a notice and download-only plans with their reason", async () => {
    rig = await mount(<Artifacts workspaceId="ws_1" />, base({
      "artifacts.content": { bytesBase64: "", mediaType: "application/x-sh", sizeBytes: 1, plan: { mode: "download-only", reason: "programs are not previewed" } },
      "artifacts.save": () => { throw new Error("disk full"); },
    }));
    await click(rig, /Plan/);
    expect(rig.container.querySelector(".download-only")?.textContent).toContain("programs are not previewed");
    await click(rig, /Save/);
    expect(text(rig)).toContain("disk full");
  });

  it("previews a raster image in an img with a data URL, and a pages artifact as a deck link", async () => {
    const png = Buffer.from([137, 80, 78, 71]).toString("base64");
    rig = await mount(<Artifacts workspaceId="ws_1" />, base({
      "artifacts.versions": { artifact: { id: ART, title: "Deck", kind: "presentation", origin: { kind: "run", ref: "r" }, current_version_id: V1 }, versions: [version(V1, 1, { media_type: "image/png" })] },
      "artifacts.content": { bytesBase64: png, mediaType: "image/png", sizeBytes: 4, plan: { mode: "image", reason: "" } },
    }));
    await click(rig, /Plan/);
    expect(rig.container.querySelector("img")?.getAttribute("src")).toMatch(/^data:image\/png;base64,/);
    // The deck's pages artifact is reachable from the deck.
    await click(rig, /page/i);
    expect(rig.calls.filter((call) => call.channel === "artifacts.versions").length).toBeGreaterThan(1);
  });

  it("summarizes a transcript output the authority made", async () => {
    const transcript = JSON.stringify({ language: "en", duration_ms: 65000, trust: "UNTRUSTED_EXTERNAL", segments: [{ start_ms: 0, text: "hello world" }] });
    rig = await mount(<Artifacts workspaceId="ws_1" />, base({
      "artifacts.versions": { artifact: { id: ART, title: "Talk", kind: "other", origin: {}, current_version_id: V1 }, versions: [version(V1, 1, { media_type: "application/vnd.quansio.transcript+json", format_check: null })] },
      "artifacts.content": { bytesBase64: b64(transcript), mediaType: "application/vnd.quansio.transcript+json", sizeBytes: 100, plan: { mode: "text", reason: "" } },
    }));
    await click(rig, /Plan/);
    expect(text(rig)).toContain("hello world");
    expect(text(rig)).toContain("not checked");
  });
});
