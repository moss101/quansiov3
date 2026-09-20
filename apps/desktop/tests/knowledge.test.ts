/**
 * APP-011: knowledge and memory with provenance and lifecycle, the controls the ladder
 * allows, memory that is never recovery, skills on the §11.5 ladder with activation refused
 * for anything but APPROVED, and a run's capability resolution laid out by layer — pure,
 * no server. The edge tables and the ladder are pinned to the Rust owners' source.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  KNOWLEDGE_DELETE_FROM,
  KNOWLEDGE_SUPERSEDE_FROM,
  LAYERS,
  MEMORY_DELETE_FROM,
  MEMORY_FIELDS,
  SKILL_LADDER,
  activation,
  activeVersions,
  decodeCapabilityResolution,
  decodeKnowledge,
  decodeMemory,
  decodeSkills,
  deleteKnowledge,
  deleteMemory,
  describeProvenance,
  disable,
  install,
  isCurrent,
  isRecoveryField,
  layersInOrder,
  promotion,
  retrievable,
  skillInputs,
  successorsFor,
  supersedeKnowledge,
  tombstones,
  type KnowledgeView,
  type SkillVersionView,
} from "../src/index.js";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const KN_A = "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const KN_B = "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAB";
const KN_C = "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAC";

function knowledgeRow(id: string, status: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id,
    workspace_id: "ws_1",
    scope: "workspace",
    kind: "fact",
    content_ref: null,
    content: { claim: `claim ${id}` },
    provenance: [{ source_kind: "approved_source", ref: "doc:checklist", digest: "0".repeat(64), retrieved_at: "2026-09-20T00:00:00Z" }],
    confidence: 0.9,
    version: 1,
    status,
    retrievable: status === "active",
    superseded_by: null,
    created_at: "2026-09-20T00:00:00Z",
    updated_at: "2026-09-20T00:00:00Z",
    ...extra,
  };
}

function memoryRow(id: string, status: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id,
    workspace_id: "ws_1",
    scope: "user",
    subject_ref: "usr_1",
    content: "prefers bundles summarised first",
    provenance: { kind: "explicit_user", ref: "msg_1" },
    confidence: 0.8,
    status,
    retrievable: status === "active",
    last_used_at: null,
    expires_at: null,
    created_at: "2026-09-20T00:00:00Z",
    updated_at: "2026-09-20T00:00:00Z",
    ...extra,
  };
}

describe("knowledge and memory", () => {
  it("decodes provenance, scope and lifecycle, and only the server's word decides retrievability", () => {
    const entries = decodeKnowledge({
      knowledge: [knowledgeRow(KN_A, "active"), knowledgeRow(KN_B, "verified"), knowledgeRow(KN_C, "superseded", { superseded_by: KN_A, retrievable: false }), { id: "kn_bad", status: "whatever" }, { id: "art_x", status: "active" }],
    });
    expect(entries.map((entry) => entry.id)).toEqual([KN_A, KN_B, KN_C]);
    expect(entries[0]?.provenance).toEqual([{ sourceKind: "approved_source", ref: "doc:checklist", digest: "0".repeat(64), retrievedAt: "2026-09-20T00:00:00Z" }]);
    expect(describeProvenance(entries[0] as KnowledgeView)).toBe("approved_source:doc:checklist");
    expect(retrievable(entries).map((entry) => entry.id)).toEqual([KN_A]);
    expect(tombstones(entries).map((entry) => entry.id)).toEqual([KN_C]);
    expect(entries[2]?.supersededBy).toBe(KN_A);
    // Status alone never makes something retrievable on the page: the server's flag does.
    const [flagged] = decodeKnowledge({ knowledge: [knowledgeRow(KN_A, "active", { retrievable: false })] });
    expect(flagged?.retrievable).toBe(false);
  });

  it("refuses a row as memory when it carries anything that is not memory", () => {
    const decoded = decodeMemory({
      memory: [
        memoryRow("mem_1", "active"),
        memoryRow("mem_2", "active", { checkpoint_id: "chk_1" }),
        memoryRow("mem_3", "candidate", { protocol_state: {} }),
        memoryRow("mem_4", "active", { colour: "blue" }),
        memoryRow("mem_5", "deleted", { retrievable: false }),
      ],
    });
    expect(decoded.entries.map((entry) => entry.id)).toEqual(["mem_1", "mem_5"]);
    expect(decoded.refused).toEqual([
      { id: "mem_2", fields: ["checkpoint_id"] },
      { id: "mem_3", fields: ["protocol_state"] },
      { id: "mem_4", fields: ["colour"] },
    ]);
    expect(decoded.entries[0]?.provenance).toEqual({ kind: "explicit_user", ref: "msg_1" });
    expect(retrievable(decoded.entries).map((entry) => entry.id)).toEqual(["mem_1"]);
    for (const field of ["checkpoint", "stream_cursor", "generation", "lease_id", "protocol_state", "resume_token", "attempt", "effect_id"]) {
      expect(isRecoveryField(field)).toBe(true);
    }
    for (const field of MEMORY_FIELDS) {
      expect(isRecoveryField(field)).toBe(false);
    }
  });

  it("offers only the edges the §11.4 ladder has, before any command is sent", () => {
    const [active, verified, candidate, deleted] = decodeKnowledge({
      knowledge: [knowledgeRow(KN_A, "active"), knowledgeRow(KN_B, "verified"), knowledgeRow(KN_C, "candidate"), knowledgeRow("kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAD", "deleted")],
    }) as [KnowledgeView, KnowledgeView, KnowledgeView, KnowledgeView];
    expect(deleteKnowledge(active)).toEqual({ ok: true, command: "DeleteKnowledge", params: { knowledge_id: KN_A } });
    expect(deleteKnowledge(candidate).ok).toBe(true);
    expect(deleteKnowledge(deleted)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(supersedeKnowledge(active, candidate)).toEqual({ ok: true, command: "SupersedeKnowledge", params: { knowledge_id: KN_A, superseded_by: KN_C } });
    expect(supersedeKnowledge(verified, active).ok).toBe(true);
    expect(supersedeKnowledge(candidate, active)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(supersedeKnowledge(active, active)).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(supersedeKnowledge(active, deleted)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(successorsFor(active, [active, verified, candidate, deleted]).map((entry) => entry.id)).toEqual([KN_B, KN_C]);
    const memory = decodeMemory({ memory: [memoryRow("mem_1", "active"), memoryRow("mem_2", "deleted", { retrievable: false })] }).entries;
    expect(deleteMemory(memory[0] as never)).toEqual({ ok: true, command: "DeleteMemory", params: { memory_id: "mem_1" } });
    expect(deleteMemory(memory[1] as never)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
  });

  it("pins the page's edge tables to the Rust data-lifecycle control", () => {
    const rust = readFileSync(join(REPO, "crates", "server", "src", "control", "data_lifecycle", "intelligence.rs"), "utf8");
    const table = (name: string): string[] => {
      const match = new RegExp(`pub const ${name}: \\[&str; \\d+\\] =\\s*\\[([^\\]]*)\\];`).exec(rust);
      if (!match) {
        throw new Error(`${name} not found in the Rust source`);
      }
      return [...(match[1] ?? "").matchAll(/"([a-z_]+)"/g)].map((found) => found[1] ?? "").sort();
    };
    expect([...KNOWLEDGE_DELETE_FROM].sort()).toEqual(table("KNOWLEDGE_DELETE_FROM"));
    expect([...KNOWLEDGE_SUPERSEDE_FROM].sort()).toEqual(table("KNOWLEDGE_SUPERSEDE_FROM"));
    expect([...MEMORY_DELETE_FROM].sort()).toEqual(table("MEMORY_DELETE_FROM"));
  });
});

function versionRow(id: string, status: string, promotableTo: string[], extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id,
    semver: "1.0.0",
    manifest: { instructions: "summarise", tool_needs: ["fs.read"], capability_needs: [{ effect_class: "fs.write.workspace", resource: { kind: "fs", selector: "**" } }] },
    provenance: "authored:test",
    status,
    resolves: false,
    promotable_to: promotableTo,
    created_at: "2026-09-20T00:00:00Z",
    ...extra,
  };
}

describe("skills", () => {
  it("decodes the ladder view and only an approved version can be activated", () => {
    const skills = decodeSkills({
      skills: [
        {
          id: "skl_1",
          name: "release-notes",
          scope: "workspace",
          owner: "usr_1",
          current_active_version_id: "sklv_active",
          versions: [
            versionRow("sklv_draft", "draft", ["candidate"]),
            versionRow("sklv_cand", "candidate", ["evaluating"]),
            versionRow("sklv_eval", "evaluating", ["approved", "rejected"]),
            versionRow("sklv_appr", "approved", ["active"]),
            versionRow("sklv_active", "active", ["deprecated"], { resolves: true }),
            versionRow("sklv_dep", "deprecated", ["retired"]),
            versionRow("sklv_rej", "rejected", []),
            versionRow("sklv_bad", "shipped", []),
          ],
        },
      ],
    });
    const skill = skills[0];
    if (!skill) {
      throw new Error("no skill");
    }
    expect(skill.versions.map((version) => version.status)).toEqual(["draft", "candidate", "evaluating", "approved", "active", "deprecated", "rejected"]);
    expect(skill.versions[0]?.manifest.capabilityNeeds).toEqual([{ effectClass: "fs.write.workspace", resource: { kind: "fs", selector: "**" } }]);
    expect(activeVersions(skills).map(({ version }) => version.id)).toEqual(["sklv_active"]);
    const byStatus = Object.fromEntries(skill.versions.map((version) => [version.status, version])) as Record<string, SkillVersionView>;
    for (const status of ["draft", "candidate", "evaluating", "deprecated", "rejected", "active"]) {
      const refused = activation(byStatus[status] as SkillVersionView);
      expect(refused).toMatchObject({ ok: false, code: "RUNTIME_ILLEGAL_TRANSITION" });
    }
    expect(activation(byStatus["approved"] as SkillVersionView)).toEqual({ ok: true, command: "PromoteSkillVersion", params: { skill_version_id: "sklv_appr", to: "active" } });
    // Any other edge follows the server's `promotable_to`, one rung at a time.
    expect(promotion(byStatus["draft"] as SkillVersionView, "candidate").ok).toBe(true);
    expect(promotion(byStatus["draft"] as SkillVersionView, "approved")).toMatchObject({ ok: false, code: "RUNTIME_ILLEGAL_TRANSITION" });
    expect(promotion(byStatus["rejected"] as SkillVersionView, "candidate")).toMatchObject({ ok: false, reason: expect.stringContaining("terminal") as string });
    expect(disable(skill)).toEqual({ ok: true, command: "DisableSkill", params: { skill_id: "skl_1" } });
    expect(disable({ ...skill, currentActiveVersionId: null })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
  });

  it("installs a draft with a validated manifest and never an active version", () => {
    const ok = install({ scope: "workspace", name: "release-notes", semver: "1.2.3", instructions: "summarise", toolNeeds: ["fs.read"], capabilityNeeds: [{ effectClass: "fs.write.workspace", resource: null }], provenance: "authored" });
    expect(ok).toEqual({
      ok: true,
      command: "InstallSkill",
      params: { scope: "workspace", name: "release-notes", version: { semver: "1.2.3", manifest: { instructions: "summarise", tool_needs: ["fs.read"], capability_needs: [{ effect_class: "fs.write.workspace" }] }, provenance: "authored" } },
    });
    expect(JSON.stringify(ok)).not.toContain("active");
    expect(install({ scope: "workspace", name: "Release Notes", semver: "1.0.0", instructions: "x" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(install({ scope: "workspace", name: "notes", semver: "1", instructions: "x" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(install({ scope: "workspace", name: "notes", semver: "1.0.0", instructions: " " })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
  });

  it("pins the ladder spelling to the Rust skill control plane", () => {
    const rust = readFileSync(join(REPO, "crates", "server", "src", "control", "skills", "state.rs"), "utf8");
    const spelled = [...rust.matchAll(/Self::[A-Z][a-z]+ => "([a-z]+)",/g)].map((match) => match[1]);
    expect(spelled).toEqual([...SKILL_LADDER]);
  });
});

describe("capability resolution", () => {
  const run = {
    id: "run_1",
    capability_projection: {
      id: "cap_1",
      subject: { kind: "run", id: "run_1" },
      inputs: [
        { layer: "tenant_policy", ref: "pol_1@1", digest: "a".repeat(64) },
        { layer: "active_skills", ref: "sklv_1,sklv_2", digest: "b".repeat(64) },
        { layer: "made_up", ref: "x", digest: "c".repeat(64) },
      ],
      grants: [{ effect_class: "read.internal", resource: { kind: "fs", selector: "**" }, constraints: {} }],
      inputs_digest: "d".repeat(64),
      computed_at: "2026-09-20T00:00:00Z",
      expires_at: "2999-01-01T00:00:00Z",
    },
  };

  it("lays the ten layers out in order, absent ones included, and names the skill inputs", () => {
    const resolution = decodeCapabilityResolution(run);
    expect(resolution).not.toBeNull();
    if (!resolution) {
      return;
    }
    expect(resolution.inputs.map((input) => input.layer)).toEqual(["tenant_policy", "active_skills"]);
    const rows = layersInOrder(resolution);
    expect(rows.map((row) => row.layer)).toEqual([...LAYERS]);
    expect(rows.filter((row) => row.input).map((row) => row.layer)).toEqual(["tenant_policy", "active_skills"]);
    expect(skillInputs(resolution)).toEqual(["sklv_1", "sklv_2"]);
    expect(resolution.grants).toEqual([{ effectClass: "read.internal", resource: { kind: "fs", selector: "**" }, constraints: {} }]);
    expect(isCurrent(resolution)).toBe(true);
    expect(isCurrent({ ...resolution, expiresAt: "2000-01-01T00:00:00Z" })).toBe(false);
    expect(decodeCapabilityResolution({ id: "run_2", capability_projection: null })).toBeNull();
  });

  it("pins the layer order to the Rust capability crate", () => {
    const rust = readFileSync(join(REPO, "crates", "capability", "src", "layer.rs"), "utf8");
    const spelled = [...rust.matchAll(/Self::[A-Za-z]+ => "([a-z_]+)",/g)].map((match) => match[1]);
    expect(spelled).toEqual([...LAYERS]);
  });
});
