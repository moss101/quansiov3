/**
 * APP-008: the artifact workspace's preview safety, version history and diff, decoders and
 * the typed search program — pure, no server.
 */

import { describe, expect, it } from "vitest";

import {
  MAX_IMAGE_PREVIEW_BYTES,
  MAX_TEXT_PREVIEW_BYTES,
  artifactWorkspaceReducer,
  comparedVersion,
  decodeArtifacts,
  decodeEvidence,
  decodeHistory,
  decodeSearch,
  diffApplicable,
  diffLines,
  imageDataUrl,
  initialArtifactWorkspace,
  isTextLike,
  openAllowed,
  previewExecutesWithHostPrivileges,
  previewIsSafe,
  previewPlan,
  searchQueryString,
  searchRequest,
  selectedVersion,
  suggestedFilename,
  textPreview,
  type ArtifactWorkspaceAction,
  type ArtifactWorkspaceState,
} from "../src/index.js";
import { previewFromContent } from "../src/renderer/Artifacts.js";

const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const ART = "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const V1 = "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const V2 = "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";

function reduce(state: ArtifactWorkspaceState, ...actions: ArtifactWorkspaceAction[]): ArtifactWorkspaceState {
  return actions.reduce(artifactWorkspaceReducer, state);
}

describe("malicious preview", () => {
  it("never renders active content: HTML, SVG, XHTML, XML and JavaScript are text", () => {
    for (const media of ["text/html", "text/html; charset=utf-8", "image/svg+xml", "application/xhtml+xml", "application/javascript", "text/javascript", "application/xml", "text/xml"]) {
      expect(previewPlan(media, 100).mode, media).toBe("text");
      expect(openAllowed(media), media).toBe(false);
      expect(previewIsSafe(media), media).toBe(true);
      expect(previewExecutesWithHostPrivileges(media), media).toBe(false);
    }
    // A page with a script arrives from main as text and stays a string: what the page
    // shows is the literal markup, never a document that could run it.
    const page = "<!doctype html><html><body><script>fetch('http://evil.example')</script><img src=x onerror=alert(1)></body></html>";
    const bytes = new TextEncoder().encode(page);
    const preview = previewFromContent(V1, { bytesBase64: Buffer.from(bytes).toString("base64"), mediaType: "text/plain; charset=utf-8", sizeBytes: bytes.length, plan: previewPlan("text/html", bytes.length) });
    expect(preview.plan.mode).toBe("text");
    expect(preview.text).toBe(page);
    expect(preview.imageUrl).toBeNull();
    // An SVG can carry script; it is never an image here, whatever it is labelled.
    expect(() => imageDataUrl(new TextEncoder().encode("<svg onload=alert(1)/>"), "image/svg+xml")).toThrow(/raster/);
    expect(previewPlan("image/svg+xml", 10).mode).toBe("text");
  });

  it("never opens programs or unlabelled bytes and only decodes raster images in-process", () => {
    for (const media of ["application/x-msdownload", "application/x-sh", "text/x-shellscript", "application/x-mach-binary", "application/java-archive", "application/x-apple-diskimage"]) {
      expect(previewPlan(media, 10).mode, media).toBe("download-only");
      expect(openAllowed(media), media).toBe(false);
    }
    expect(openAllowed("application/octet-stream")).toBe(false);
    expect(openAllowed("")).toBe(false);
    expect(openAllowed("application/pdf")).toBe(false);
    expect(openAllowed("image/png")).toBe(true);
    expect(openAllowed("text/csv")).toBe(true);
    expect(previewPlan("image/png", 10).mode).toBe("image");
    expect(previewPlan("image/png", MAX_IMAGE_PREVIEW_BYTES + 1).mode).toBe("download-only");
    expect(previewPlan("text/plain", MAX_TEXT_PREVIEW_BYTES + 1).mode).toBe("download-only");
    expect(previewPlan("application/pdf", 10).mode).toBe("download-only");
    expect(previewPlan("audio/mpeg", 10).mode).toBe("download-only");
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
    expect(imageDataUrl(png, "image/png")).toBe("data:image/png;base64,iVBORw==");
    const preview = previewFromContent(V1, { bytesBase64: Buffer.from(png).toString("base64"), mediaType: "image/png", sizeBytes: 4, plan: previewPlan("image/png", 4) });
    expect(preview.imageUrl).toMatch(/^data:image\/png;base64,/);
    expect(preview.text).toBeNull();
  });

  it("decodes text with replacement and marks truncation", () => {
    const { text, truncated } = textPreview(new Uint8Array([0x68, 0x69, 0xff, 0xfe]));
    expect(text.startsWith("hi")).toBe(true);
    expect(truncated).toBe(false);
    const big = textPreview(new Uint8Array(300_000).fill(0x61));
    expect(big.truncated).toBe(true);
    expect(big.text.endsWith("…[truncated]")).toBe(true);
    expect(isTextLike("text/markdown")).toBe(true);
    expect(isTextLike("application/json")).toBe(true);
    expect(isTextLike("image/png")).toBe(false);
  });

  it("suggests OS-safe filenames that carry the version", () => {
    expect(suggestedFilename("report.md", V1)).toBe(`report-${V1.slice(-8)}.md`);
    expect(suggestedFilename("../../etc/passwd", V1)).toBe(`_.._etc_passwd-${V1.slice(-8)}`);
    expect(suggestedFilename(".bashrc", V1)).toBe(`bashrc-${V1.slice(-8)}`);
    expect(suggestedFilename("a/b\\c.tar.gz", V1)).toBe(`a_b_c.tar-${V1.slice(-8)}.gz`);
    expect(suggestedFilename("", V1)).toBe(`artifact-${V1.slice(-8)}`);
  });
});

describe("version history", () => {
  const history = {
    artifact: { id: ART, title: "report.md", kind: "document", origin: { kind: "upload", ref: "usr_1" }, current_version_id: V2 },
    versions: [
      { id: V2, seq: 2, content_digest: "sha256:b", size_bytes: 20, media_type: "text/markdown", produced_by: { run_id: "run_1", step_id: "stp_1" }, evidence_ids: ["evd_1"], parent_version_id: V1, created_at: "2026-09-20T00:00:01Z" },
      { id: V1, seq: 1, content_digest: "sha256:a", size_bytes: 10, media_type: "text/markdown", produced_by: { user_id: "usr_1" }, evidence_ids: [], parent_version_id: null, created_at: "2026-09-20T00:00:00Z" },
    ],
  };

  it("selects the current version, compares with the parent by default and keeps links", () => {
    let state = reduce(initialArtifactWorkspace(WORKSPACE), { type: "select", artifactId: ART });
    expect(state.stale.history).toBe(true);
    state = reduce(state, { type: "history.loaded", history: decodeHistory(history) });
    expect(state.history?.versions.map((version) => version.seq)).toEqual([1, 2]);
    expect(selectedVersion(state)?.id).toBe(V2);
    expect(comparedVersion(state)?.id).toBe(V1);
    expect(selectedVersion(state)?.producedBy.runId).toBe("run_1");
    expect(selectedVersion(state)?.evidenceIds).toEqual(["evd_1"]);
    state = reduce(state, { type: "version.select", versionId: V1 });
    expect(selectedVersion(state)?.id).toBe(V1);
    expect(comparedVersion(state)).toBeNull();
    state = reduce(state, { type: "version.compare", versionId: V2 });
    expect(comparedVersion(state)?.id).toBe(V2);
    // Unknown versions are ignored; a history for another artifact is ignored.
    expect(reduce(state, { type: "version.select", versionId: "artv_nope" })).toBe(state);
    expect(reduce(state, { type: "history.loaded", history: { ...decodeHistory(history), artifactId: "art_other" } })).toBe(state);
    state = reduce(state, { type: "evidence.loaded", evidence: decodeEvidence({ id: "evd_1", kind: "tool_output", run_id: "run_1", inline_summary: "grep output", captured_at: "2026-09-20T00:00:00Z" }) });
    expect(state.evidence["evd_1"]?.summary).toBe("grep output");
    // A change event marks the list and this artifact's history stale.
    state = reduce(state, { type: "artifact.changed", artifactId: ART });
    expect(state.stale).toEqual({ list: true, history: true });
  });

  it("diffs text versions line by line and refuses when either side is not text", () => {
    const lines = diffLines("a\nb\nc\n", "a\nc\nd\n");
    expect(lines?.map((line) => `${line.kind[0] ?? ""}:${line.text}`)).toEqual(["s:a", "r:b", "s:c", "a:d", "s:"]);
    expect(diffLines("same", "same")?.every((line) => line.kind === "same")).toBe(true);
    const decoded = decodeHistory(history);
    const [v1, v2] = decoded.versions;
    if (!v1 || !v2) {
      throw new Error("two versions expected");
    }
    expect(diffApplicable(v1, v2, isTextLike)).toBe(true);
    expect(diffApplicable({ ...v1, mediaType: "image/png" }, v2, isTextLike)).toBe(false);
    const huge = Array.from({ length: 5000 }, (_, index) => String(index)).join("\n");
    expect(diffLines(huge, huge)).toBeNull();
  });

  it("decodes the artifact list with its kinds, metadata and produced-by", () => {
    const rows = decodeArtifacts({
      artifacts: [
        { id: ART, kind: "spreadsheet", title: "q3.xlsx", origin: { kind: "tool", ref: "tc_1" }, current_version_id: V1, version_count: 3, media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", size_bytes: 1234, content_digest: "sha256:x", produced_by: { run_id: "run_1" }, updated_at: "2026-09-20T00:00:00Z" },
        { id: "art_2", kind: "hologram", title: "?", produced_by: {} },
        { title: "no id" },
      ],
    });
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({ kind: "spreadsheet", versionCount: 3, sizeBytes: 1234, producedBy: { runId: "run_1", stepId: null, userId: null } });
    expect(rows[1]?.kind).toBe("other");
  });
});

describe("global search", () => {
  it("builds a typed program and refuses anything outside it", () => {
    const request = searchRequest("  quokka  ", ["artifacts", "threads", "artifacts"], 10);
    expect(request).toEqual({ q: "quokka", channels: ["artifacts", "threads"], maxResults: 10 });
    expect(searchQueryString(WORKSPACE, request)).toBe(`workspace_id=${WORKSPACE}&q=quokka&channels=artifacts%2Cthreads&max_results=10`);
    expect(() => searchRequest("")).toThrow(/1\.\.=200/);
    expect(() => searchRequest("x".repeat(201))).toThrow(/1\.\.=200/);
    expect(() => searchRequest("q", ["memory"])).toThrow(/not a search channel/);
    expect(() => searchRequest("q", [])).toThrow(/channel/);
    expect(() => searchRequest("q", undefined, 0)).toThrow(/maxResults/);
    expect(() => searchRequest("q", undefined, 51)).toThrow(/maxResults/);
    expect(searchQueryString(WORKSPACE, searchRequest("a'b%c&d=e"))).toContain("q=a%27b%25c%26d%3De");
  });

  it("decodes hits with provenance and drops unknown channels", () => {
    const results = decodeSearch({
      program: { q: "quokka", channels: ["artifacts", "threads"] },
      hits: [
        { channel: "artifacts", id: ART, title: "notes.md", snippet: "the quokka", matched: "content", score: 2.5, ref: { artifact_id: ART, version_id: V1 }, provenance: { source_id: ART, locator: `artifact_version:${V1}`, snapshot: "3:abc" } },
        { channel: "threads", id: "msg_1", title: "thr_1", snippet: "did the quokka", ref: { thread_id: "thr_1", run_id: null } },
        { channel: "memory", id: "mem_1" },
      ],
      truncated: true,
      engines: [{ channel: "artifacts", engine: "quansio-indexer" }],
    });
    expect(results.hits.map((hit) => hit.channel)).toEqual(["artifacts", "threads"]);
    expect(results.hits[0]?.provenance).toEqual({ sourceId: ART, locator: `artifact_version:${V1}`, snapshot: "3:abc" });
    expect(results.hits[1]?.ref).toEqual({ thread_id: "thr_1", run_id: null });
    expect(results.truncated).toBe(true);
    expect(results.engines[0]?.engine).toBe("quansio-indexer");
  });
});
