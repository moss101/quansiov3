/**
 * ADP-008: capability packs and their install reports on the Knowledge surface — pure, no server. The page decodes
 * `GET /v1/packs` strictly, offers a rollback only where a retained revision still holds its content, and shows an
 * install report's state, phase and outcome. The vocabularies are pinned to the Rust owner's source.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { INSTALL_PHASES, ROLLBACK_COMMAND, decodePacks, retained, rollback, shortRevision, summarizeReport } from "../src/index.js";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const R1 = `sha256:${"a".repeat(64)}`;
const R2 = `sha256:${"b".repeat(64)}`;
const R3 = `sha256:${"c".repeat(64)}`;

function only<T>(items: readonly T[] | null | undefined): T {
  const item = items?.[0];
  if (item === undefined) {
    throw new Error("expected one item");
  }
  return item;
}

function revision(hash: string, status: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return { version_id: `pckv_${hash.slice(7, 20)}`, revision: hash, semver: "1.0.0", status, has_content: true, activated_at: "2026-10-01T00:00:00Z", retired_at: null, ...extra };
}

function report(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: "cmd_01J8Z3K6F1N8VQ2X5W9YP00001",
    pack: "coding",
    pack_id: "pck_1",
    revision: R2,
    previous_revision: R1,
    state: "failed",
    phase: "compile",
    outcome: "retained_previous",
    diagnostics: [{ phase: "compile", rule: "pack.tool.host_only", severity: "error", message: "not model-visible", pointer: "/contents/tool_names/1" }],
    diagnostics_omitted: 2,
    started_at: "2026-10-07T10:00:00Z",
    finished_at: "2026-10-07T10:00:01Z",
    expires_at: "2026-11-06T10:00:00Z",
    ...overrides,
  };
}

describe("packs", () => {
  it("decodes the packs, their active revision and the revisions kept for rollback", () => {
    const view = decodePacks({
      packs: [{ pack_id: "pck_1", name: "coding", active: revision(R2, "published"), revisions: [revision(R1, "deprecated"), revision(R3, "withdrawn", { has_content: false })] }],
      install_reports: [],
    });
    expect(view.packs).toHaveLength(1);
    expect(view.packs[0]?.active?.revision).toBe(R2);
    expect(retained(only(view.packs)).map((kept) => kept.revision)).toEqual([R1]);
    expect(view.reports).toEqual([]);
  });

  it("drops rows the server could not have sent instead of guessing", () => {
    const view = decodePacks({
      packs: [{ name: "no id" }, { pack_id: "pck_2", name: "ok", active: { revision: "no version id" }, revisions: [{}] }],
      install_reports: [report({ state: "exploded" }), report({ phase: "later" }), report({ request_id: null }), report()],
    });
    expect(view.packs.map((pack) => pack.name)).toEqual(["ok"]);
    expect(view.packs[0]?.active).toBeNull();
    expect(view.packs[0]?.revisions).toEqual([]);
    expect(view.reports).toHaveLength(1);
  });

  it("says when the caller may not read reports: null, not an empty list", () => {
    expect(decodePacks({ packs: [], install_reports: null }).reports).toBeNull();
    expect(decodePacks({ packs: [] }).reports).toBeNull();
    expect(decodePacks(undefined)).toEqual({ packs: [], reports: null });
  });
});

describe("install reports", () => {
  it("show state, phase and outcome and what is still active", () => {
    const view = only(decodePacks({ packs: [], install_reports: [report()] }).reports);
    expect([view.state, view.phase, view.outcome]).toEqual(["failed", "compile", "retained_previous"]);
    expect(view.diagnostics).toEqual([{ phase: "compile", rule: "pack.tool.host_only", severity: "error", message: "not model-visible", pointer: "/contents/tool_names/1" }]);
    expect(view.diagnosticsOmitted).toBe(2);
    expect(summarizeReport(view)).toBe(`failed at compile — the previous revision ${shortRevision(R1)} is still active`);
    const first = only(decodePacks({ packs: [], install_reports: [report({ outcome: "failed", previous_revision: null })] }).reports);
    expect(summarizeReport(first)).toBe("failed at compile — nothing was activated");
    const done = only(decodePacks({ packs: [], install_reports: [report({ state: "succeeded", phase: "done", outcome: "activated", diagnostics: [] })] }).reports);
    expect(summarizeReport(done)).toBe(`activated ${shortRevision(R2)}`);
    const running = only(decodePacks({ packs: [], install_reports: [report({ state: "running", phase: "verify", outcome: null, finished_at: null })] }).reports);
    expect(summarizeReport(running)).toBe("installing — at verify");
  });

  it("keeps a finding's text as text: nothing here parses or runs it", () => {
    const hostile = "<img src=x onerror=alert(1)> javascript:alert(2)";
    const view = only(decodePacks({ packs: [], install_reports: [report({ diagnostics: [{ phase: "verify", rule: "pack.schema", severity: "error", message: hostile }] })] }).reports);
    expect(view.diagnostics[0]?.message).toBe(hostile);
    const rendered = readFileSync(join(REPO, "apps", "desktop", "src", "renderer", "Knowledge.tsx"), "utf8");
    expect(rendered).not.toContain("dangerouslySetInnerHTML");
  });

  it("abbreviate a revision to twelve digits", () => {
    expect(shortRevision(R1)).toBe("a".repeat(12));
    expect(shortRevision(null)).toBe("—");
  });
});

describe("rollback", () => {
  const pack = (revisions: Record<string, unknown>[], active: Record<string, unknown> | null = revision(R2, "published")) => only(decodePacks({ packs: [{ pack_id: "pck_1", name: "coding", active, revisions }] }).packs);

  it("is offered only when a retired revision still holds its content", () => {
    expect(rollback(pack([revision(R1, "deprecated")]))).toEqual({ ok: true, params: { pack: "coding" } });
    const withdrawn = rollback(pack([revision(R1, "withdrawn", { has_content: false })]));
    expect(withdrawn.ok).toBe(false);
    expect(!withdrawn.ok && withdrawn.code).toBe("CONFLICT_STATE");
    expect(rollback(pack([], revision(R2, "published"))).ok).toBe(false);
    expect(rollback(pack([revision(R1, "deprecated")], null)).ok).toBe(false);
    expect(ROLLBACK_COMMAND).toBe("RollbackPack");
  });
});

describe("the vocabularies are the server's", () => {
  const rust = readFileSync(join(REPO, "crates", "server", "src", "control", "packs", "report.rs"), "utf8");

  it("phases, states and outcomes match the report owner", () => {
    for (const phase of INSTALL_PHASES) {
      expect(rust).toContain(`Self::${phase.charAt(0).toUpperCase()}${phase.slice(1)} => "${phase}"`);
    }
    for (const word of ["running", "succeeded", "failed", "activated", "retained_previous"]) {
      expect(rust).toContain(`"${word}"`);
    }
  });

  it("the rollback command is a catalog command", () => {
    const commands = readFileSync(join(REPO, "schemas", "catalog", "commands.yaml"), "utf8");
    expect(commands).toContain(ROLLBACK_COMMAND);
  });
});
