// @vitest-environment jsdom
/** APP-011 on a page: the Knowledge view over the real preload API, answering from a table of projections. */

import { afterEach, describe, expect, it } from "vitest";

import { Knowledge } from "../../src/renderer/Knowledge.js";
import { click, mount, text, type, type Rig } from "./rig.js";

const KN_A = "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const KN_B = "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAB";

function knowledgeRow(id: string, status: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, workspace_id: "ws_1", scope: "workspace", kind: "fact", content_ref: null, content: { claim: `claim ${id}` },
    provenance: [{ source_kind: "approved_source", ref: "doc:checklist", digest: "0".repeat(64), retrieved_at: "2026-09-20T00:00:00Z" }],
    confidence: 0.9, version: 1, status, retrievable: status === "active", superseded_by: null,
    created_at: "2026-09-20T00:00:00Z", updated_at: "2026-09-20T00:00:00Z", ...extra,
  };
}

function memoryRow(id: string, status: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, workspace_id: "ws_1", scope: "user", subject_ref: "usr_1", content: "prefers bundles summarised first",
    provenance: { kind: "explicit_user", ref: "msg_1" }, confidence: 0.8, status, retrievable: status === "active",
    last_used_at: null, expires_at: null, created_at: "2026-09-20T00:00:00Z", updated_at: "2026-09-20T00:00:00Z", ...extra,
  };
}

function versionRow(id: string, status: string, promotableTo: string[], extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, semver: "1.0.0",
    manifest: { instructions: "summarise", tool_needs: ["fs.read"], capability_needs: [{ effect_class: "fs.write.workspace", resource: { kind: "fs", selector: "**" } }] },
    provenance: "authored:test", status, resolves: false, promotable_to: promotableTo, created_at: "2026-09-20T00:00:00Z", ...extra,
  };
}

const ok = { ok: true, result: {} };
const answers = {
  "knowledge.list": { knowledge: [knowledgeRow(KN_A, "active"), knowledgeRow(KN_B, "candidate")] },
  "memory.list": { memory: [memoryRow("mem_1", "active"), memoryRow("mem_2", "active", { checkpoint_id: "chk_1" })] },
  "skills.list": {
    skills: [{ id: "skl_1", name: "release-notes", scope: "workspace", owner: "usr_1", current_active_version_id: "sklv_active",
      versions: [versionRow("sklv_eval", "evaluating", ["approved"]), versionRow("sklv_appr", "approved", ["active"]), versionRow("sklv_active", "active", ["deprecated"], { resolves: true })] }],
  },
  "packs.list": {
    packs: [{ pack_id: "pk_1", name: "coding", active: { version_id: "v2", revision: "b".repeat(64), semver: "2.0.0", status: "active", has_content: true },
      revisions: [{ version_id: "v1", revision: "a".repeat(64), semver: "1.0.0", status: "deprecated", has_content: true }] }],
    install_reports: [{ request_id: "req_1", pack: "coding", state: "failed", phase: "verify", outcome: "failed", diagnostics_omitted: 2,
      diagnostics: [{ phase: "validate", rule: "no-egress", severity: "error", message: "refused", pointer: "/tools/0" }] }],
  },
  "runs.capabilities": {
    id: "run_1",
    capability_projection: { id: "cap_1", subject: { kind: "run", id: "run_1" },
      inputs: [{ layer: "tenant_policy", ref: "pol_1@1", digest: "a".repeat(64) }, { layer: "active_skills", ref: "sklv_1", digest: "b".repeat(64) }],
      grants: [{ effect_class: "read.internal", resource: { kind: "fs", selector: "**" }, constraints: {} }],
      inputs_digest: "d".repeat(64), computed_at: "2026-09-20T00:00:00Z", expires_at: "2999-01-01T00:00:00Z" },
  },
  "knowledge.delete": ok, "knowledge.supersede": ok, "memory.delete": ok, "skills.promote": ok, "skills.disable": ok, "skills.install": ok, "packs.rollback": ok,
} as const;

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

describe("the Knowledge view", () => {
  it("lays out entries, memory (refusing recovery state), skills, packs, reports and the capability resolution", async () => {
    rig = await mount(<Knowledge workspaceId="ws_1" runId="run_1" />, answers);
    const page = text(rig);
    expect(page).toContain(KN_A);
    expect(page).toContain("prefers bundles summarised first");
    expect(rig.container.querySelectorAll("[data-entry^=mem_]").length).toBe(1);
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("1");
    expect(rig.container.querySelector("[data-skill=skl_1]")?.getAttribute("data-active-version")).toBe("sklv_active");
    expect(rig.container.querySelector("[data-report=req_1]")?.getAttribute("data-outcome")).toBe("failed");
    expect(rig.container.querySelector("[data-layer=tenant_policy]")?.className).toBe("present");
    expect(rig.container.querySelector("[data-grant='read.internal']")).not.toBeNull();
  });

  it("sends each lifecycle command only for an edge the ladder has, and reloads", async () => {
    rig = await mount(<Knowledge workspaceId="ws_1" runId={null} />, answers);
    await click(rig, "Forget");
    expect(rig.calls.some((call) => call.channel === "knowledge.delete")).toBe(true);
    await click(rig, "Supersede");
    await click(rig, new RegExp(KN_B));
    expect(rig.calls.some((call) => call.channel === "knowledge.supersede")).toBe(true);
    rig.container.querySelector<HTMLButtonElement>("[data-entry^=mem_] .forget")?.click();
    await rig.flush();
    expect(rig.calls.some((call) => call.channel === "memory.delete")).toBe(true);
    await click(rig, /Activate/);
    expect(rig.calls.some((call) => call.channel === "skills.promote")).toBe(true);
    await click(rig, /Disable/);
    expect(rig.calls.some((call) => call.channel === "skills.disable")).toBe(true);
    await click(rig, /Roll back/i);
    expect(rig.calls.some((call) => call.channel === "packs.rollback")).toBe(true);
  });

  it("installs a draft skill, and shows the server's refusal as a notice", async () => {
    rig = await mount(<Knowledge workspaceId="ws_1" />, { ...answers, "skills.install": { ok: false, error: { code: "VALIDATION_SCHEMA", message: "bad" } } });
    await type(rig, "name", "release-notes");
    await type(rig, "nstructions", "summarise");
    const form = rig.container.querySelector("form.install") as HTMLFormElement;
    form.requestSubmit();
    await rig.flush();
    expect(rig.calls.some((call) => call.channel === "skills.install")).toBe(true);
    expect(text(rig)).toContain("VALIDATION_SCHEMA");
  });

  it("reloads when a knowledge event arrives on the stream", async () => {
    rig = await mount(<Knowledge workspaceId="ws_1" />, answers);
    const before = rig.calls.filter((call) => call.channel === "knowledge.list").length;
    await rig.push("stream.frame", { event: { type: "knowledge.changed" } });
    expect(rig.calls.filter((call) => call.channel === "knowledge.list").length).toBeGreaterThan(before);
  });
});
