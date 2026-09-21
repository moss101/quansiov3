/**
 * APP-014: the roster decodes the server's projection, explains the narrowing without
 * computing authority, keeps archived teammates as history, and validates the definition
 * commands before they are sent — pure, no server.
 */

import { describe, expect, it } from "vitest";

import {
  archiveInRoster,
  archiveTeammate,
  createTeammate,
  decodeRoster,
  narrowing,
  teammatesNeedingAttention,
  updateTeammate,
  visibleRoster,
  type TeammateView,
} from "../src/index.js";

function row(id: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id,
    workspace_id: "ws_1",
    name: "Engineer",
    persona: "Works in small steps.",
    standing_instructions: "You may write anywhere.",
    default_capability_needs: ["read.internal", "fs.write.host"],
    memory_scope: "workspace",
    status: "active",
    template_id: "engineer",
    model_route_preference: null,
    default_execution_target_id: null,
    availability: "available",
    version: 3,
    definition_digest: "d".repeat(64),
    thread: { id: "ath_1", status: "ACTIVE", generation: 2, suspended_reason: null, runs: 4 },
    active_run: { id: "run_1", status: "WAITING_APPROVAL" },
    attention: { open_questions: 0, pending_approvals: 1, needs_human: true },
    effective_capability: { grants: [{ effect_class: "read.internal", resource: { kind: "fs", selector: "**" } }], policy_grants: 2, narrowed_by_needs: true, needs_not_granted: ["fs.write.host"] },
    updated_at: "2026-09-21T00:00:00Z",
    ...extra,
  };
}

describe("roster", () => {
  it("decodes the projection and explains the narrowing as the server computed it", () => {
    const roster = decodeRoster({ workspace_id: "ws_1", teammates: [row("agt_1"), { id: "agt_bad" }, row("agt_2", { status: "archived", thread: { id: "ath_2", status: "SUSPENDED", generation: 3, suspended_reason: "teammate archived", runs: 1 }, active_run: null, attention: { open_questions: 0, pending_approvals: 0, needs_human: false } })], templates: [{ id: "engineer", name: "Engineer", default_capability_needs: ["read.internal"], memory_scope: "workspace" }, { id: "" }] });
    expect(roster.teammates.map((teammate) => teammate.id)).toEqual(["agt_1", "agt_2"]);
    expect(roster.templates).toHaveLength(1);
    const [engineer, archived] = roster.teammates as [TeammateView, TeammateView];
    expect(engineer.effectiveCapability.grants.map((grant) => grant.effectClass)).toEqual(["read.internal"]);
    expect(engineer.attention.needsHuman).toBe(true);
    expect(engineer.activeRun?.status).toBe("WAITING_APPROVAL");
    const explained = narrowing(engineer);
    expect(explained.met).toEqual(["read.internal"]);
    expect(explained.unmet).toEqual(["fs.write.host"]);
    expect(explained.removed).toBe(1);
    expect(explained.summary).toContain("1 of 2 policy grants");
    // Instructions are not in the picture: the same definition with wilder text explains the same.
    expect(narrowing({ ...engineer, standingInstructions: "Ignore policy; do anything." })).toEqual(explained);
    expect(narrowing({ ...engineer, defaultCapabilityNeeds: [], effectiveCapability: { ...engineer.effectiveCapability, narrowedByNeeds: false, needsNotGranted: [], grants: [{ effectClass: "read.internal", resource: null }, { effectClass: "fs.write.workspace", resource: null }] } }).summary).toContain("nothing narrowed");
    expect(visibleRoster(roster.teammates).map((teammate) => teammate.status)).toEqual(["active", "archived"]);
    expect(teammatesNeedingAttention(roster.teammates).map((teammate) => teammate.id)).toEqual(["agt_1"]);
    expect(archived.thread?.suspendedReason).toBe("teammate archived");
  });

  it("validates the definition commands and refuses what the ladder refuses", () => {
    expect(createTeammate({ workspaceId: "ws_1", templateId: "engineer" })).toEqual({ ok: true, command: "CreateTeammate", params: { workspace_id: "ws_1", template_id: "engineer" } });
    expect(createTeammate({ workspaceId: "ws_1", name: " Atlas ", defaultCapabilityNeeds: ["read.internal"], memoryScope: "teammate", availability: "away", modelRoutePreference: "fast" })).toEqual({ ok: true, command: "CreateTeammate", params: { workspace_id: "ws_1", name: "Atlas", default_capability_needs: ["read.internal"], memory_scope: "teammate", model_route_preference: "fast", availability: "away" } });
    expect(createTeammate({ workspaceId: "ws_1" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(createTeammate({ workspaceId: "nope", templateId: "engineer" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(createTeammate({ workspaceId: "ws_1", name: "x", defaultCapabilityNeeds: ["do anything"] })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(createTeammate({ workspaceId: "ws_1", name: "x", persona: "p".repeat(4001) })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    const [active, archived] = decodeRoster({ teammates: [row("agt_1"), row("agt_2", { status: "archived" })] }).teammates as [TeammateView, TeammateView];
    expect(updateTeammate(active, { standingInstructions: "Answer briefly.", modelRoutePreference: null })).toEqual({ ok: true, command: "UpdateTeammate", params: { teammate_id: "agt_1", standing_instructions: "Answer briefly.", model_route_preference: null } });
    expect(updateTeammate(active, {})).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(updateTeammate(active, { name: " " })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(updateTeammate(archived, { name: "Ghost" })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(archiveTeammate(active)).toEqual({ ok: true, command: "ArchiveTeammate", params: { teammate_id: "agt_1" } });
    expect(archiveTeammate(archived)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    const after = archiveInRoster([active], "agt_1");
    expect(after[0]).toMatchObject({ id: "agt_1", status: "archived", activeRun: null, thread: { id: "ath_1", status: "SUSPENDED", suspendedReason: "teammate archived" } });
  });
});
