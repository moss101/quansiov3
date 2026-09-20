/**
 * APP-012: page decoders and command builders — approvals bound to the previewed digest,
 * the WorkGraph's attention list, admin commands validated before they are sent.
 */

import { describe, expect, it } from "vitest";

import { decide, decodeApprovals, decodeArtifacts, decodeAudit, decodeConnectors, decodeMembers, decodePolicies, decodeUsage, decodeWorkGraph, invite, needsAttention, removeMember, setPolicy, updateMembership, warnings, type ApprovalView, type MemberView } from "../src/index.js";

const DIGEST = "4ba5eba4d64e598bc80db004e78d4548093a4f021dc076c00e0be1b97b066bb0";
const OTHER = "3b1698580da742407583708ba40ccc0484d44f8848d62ddccc6940023aa1f13c";

describe("approvals", () => {
  const wire = { approvals: [{ id: "apr_1", workspace_id: "ws_1", run_id: "run_1", effect_id: "eff_1", summary: "Approve fs.write", params_digest: DIGEST, status: "requested", expires_at: "2999-01-01T00:00:00Z", created_at: "2026-09-20T00:00:00Z", effect: { class: "fs.write.host", tier: 3, tool_name: "fs.write", resource: { kind: "fs", selector: "/x" } }, consequence_preview: { untrusted_origin: { trust: "UNTRUSTED_EXTERNAL" }, data_classes: ["pii"], amounts: [{ currency: "USD", minor_units: 250 }] }, receipts: [] }, { id: "apr_bad" }] };

  it("binds a decision to the previewed digest and refuses a changed, expired or decided request", () => {
    const [view] = decodeApprovals(wire) as [ApprovalView];
    expect(decodeApprovals(wire)).toHaveLength(1);
    expect(warnings(view)).toEqual(["untrusted origin: UNTRUSTED_EXTERNAL", "tier 3 effect: a fresh approval is required every time", "moves money", "involves pii data"]);
    expect(decide("approve", view, view)).toEqual({ ok: true, command: "ApproveEffect", params: { approval_request_id: "apr_1", params_digest: DIGEST } });
    expect(decide("deny", view, view)).toEqual({ ok: true, command: "DenyEffect", params: { approval_request_id: "apr_1" } });
    expect(decide("approve", view, { ...view, digest: OTHER })).toMatchObject({ ok: false, code: "APPROVAL_SUPERSEDED" });
    expect(decide("approve", view, { ...view, status: "superseded" })).toMatchObject({ ok: false, code: "APPROVAL_SUPERSEDED" });
    expect(decide("approve", view, { ...view, expiresAt: "2000-01-01T00:00:00Z" })).toMatchObject({ ok: false, code: "APPROVAL_EXPIRED" });
    expect(decide("approve", view, { ...view, status: "granted" })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(decide("approve", view, { ...view, id: "apr_2" })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
  });
});

describe("work and artifacts", () => {
  it("lists nodes needing attention and decodes artifacts", () => {
    const graph = decodeWorkGraph({ workspace_id: "ws_1", revision: 4, nodes: [{ id: "wn_1", kind: "objective", title: "Ship", status: "active", run: { id: "run_1", status: "WAITING_APPROVAL" }, open_questions: [] }, { id: "wn_2", kind: "task", title: "Test", status: "blocked", open_questions: [{ id: "q_1" }] }, { id: "wn_3", kind: "task", title: "Done", status: "done" }, { kind: "task" }], edges: [{ id: "we_1", from_node_id: "wn_2", to_node_id: "wn_1", kind: "depends_on" }], agents: [{ id: "agt_1", kind: "teammate", status: "active" }], blockers: [{ node_id: "wn_2", kind: "dependency", detail: "waits on wn_1" }] });
    expect(graph.revision).toBe(4);
    expect(graph.nodes.map((node) => node.id)).toEqual(["wn_1", "wn_2", "wn_3"]);
    expect(needsAttention(graph).map((node) => node.id)).toEqual(["wn_1", "wn_2"]);
    expect(graph.edges[0]).toEqual({ id: "we_1", from: "wn_2", to: "wn_1", kind: "depends_on" });
    const artifacts = decodeArtifacts({ artifacts: [{ id: "art_1", kind: "document", title: "Notes", current_version_id: "artv_1", version_count: 2, media_type: "text/markdown", size_bytes: 12, content_digest: DIGEST, updated_at: "" }, { kind: "x" }] });
    expect(artifacts).toHaveLength(1);
    expect(artifacts[0]?.versionCount).toBe(2);
  });
});

describe("administration", () => {
  const members: MemberView[] = decodeMembers({ members: [
    { user: { id: "usr_1", primary_email: "o@example.com", display_name: "Owner" }, membership_id: "tm_1", role: "owner", status: "active", workspaces: [{ membership_id: "wm_1", workspace_id: "ws_1", role: "admin", status: "active" }] },
    { user: { id: "usr_2", primary_email: "v@example.com", display_name: "Viewer" }, membership_id: "tm_2", role: "member", status: "active", workspaces: [{ membership_id: "wm_2", workspace_id: "ws_1", role: "viewer", status: "active" }] },
    { user: { id: "usr_3", primary_email: "r@example.com", display_name: "Removed" }, membership_id: "tm_3", role: "member", status: "removed", workspaces: [] },
  ] });

  it("validates invitations, membership changes and removals before they are sent", () => {
    expect(members).toHaveLength(3);
    expect(invite({ email: " Nova@Example.com ", role: "member", workspaceId: "ws_1", workspaceRole: "editor" })).toEqual({ ok: true, command: "InviteMember", params: { email: "nova@example.com", role: "member", workspace_id: "ws_1", workspace_role: "editor" } });
    expect(invite({ email: "nope", role: "member" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(invite({ email: "a@b.co", role: "member", workspaceId: "nope" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(updateMembership("tm_2", { role: "admin" })).toEqual({ ok: true, command: "UpdateMembership", params: { membership_id: "tm_2", role: "admin" } });
    expect(updateMembership("wm_2", { role: "editor", status: "active" })).toMatchObject({ ok: true, params: { membership_id: "wm_2", role: "editor", status: "active" } });
    expect(updateMembership("tm_2", { role: "editor" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(updateMembership("wm_2", { role: "owner" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(updateMembership("x_2", { status: "active" })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(updateMembership("tm_2", {})).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(removeMember(members[1] as MemberView, members)).toEqual({ ok: true, command: "RemoveMember", params: { membership_id: "tm_2" } });
    expect(removeMember(members[0] as MemberView, members)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(removeMember(members[2] as MemberView, members)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
  });

  it("validates a policy in the typed vocabulary and decodes policies, connectors, audit and usage", () => {
    const rules = [{ effectClass: "fs.write.host", resource: { kind: "fs", selector: "**" }, decision: "ask" }];
    expect(setPolicy({ scope: "tenant", rules, approvalTtlSeconds: 600 })).toEqual({ ok: true, command: "SetPolicy", params: { scope: "tenant", rules: [{ effect_class: "fs.write.host", resource: { kind: "fs", selector: "**" }, decision: "ask" }], approval_default_ttl_seconds: 600 } });
    expect(setPolicy({ scope: "workspace", rules })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(setPolicy({ scope: "tenant", workspaceId: "ws_1", rules })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(setPolicy({ scope: "tenant", rules: [{ effectClass: "fs.write.host", resource: { kind: "fs", selector: "**" }, decision: "maybe" }] })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(setPolicy({ scope: "tenant", rules: [{ effectClass: "Fs Write", resource: { kind: "fs", selector: "**" }, decision: "ask" }] })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    expect(setPolicy({ scope: "tenant", rules: [{ effectClass: "fs.write.host", resource: { kind: "", selector: "" }, decision: "ask" }] })).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    const policies = decodePolicies({ policies: [{ id: "pol_1", scope: "tenant", workspace_id: null, rules: [{ effect_class: "read.internal", resource_selector: { kind: "fs", selector: "**" }, decision: "allow" }], question_default_ttl_seconds: 86400, approval_default_ttl_seconds: 3600, max_plan_nodes: 25, version: 2 }, { id: "pol_2", scope: "galaxy" }] });
    expect(policies).toHaveLength(1);
    expect(policies[0]?.rules[0]).toEqual({ effectClass: "read.internal", resource: { kind: "fs", selector: "**" }, decision: "allow" });
    expect(decodeConnectors({ connectors: [{ id: "cnx_1", connector_id: "github", display_name: "GitHub", status: "connected", credential_handle: "sec_1" }] })[0]).toMatchObject({ hasCredential: true, status: "connected" });
    const audit = decodeAudit({ chain: { entries: 2, holds: false, broken_at: "aud_2" }, entries: [{ id: "aud_2", actor: { kind: "user", id: "usr_1" }, action: "SetPolicy", decision: "applied", hash: "h2", prev_hash: "h1", occurred_at: "t" }, { id: "aud_1" }] });
    expect(audit.chain).toEqual({ entries: 2, holds: false, brokenAt: "aud_2" });
    expect(audit.entries).toHaveLength(1);
    expect(audit.entries[0]?.actor).toBe("user:usr_1");
    expect(decodeUsage({ events_read: 10, meters: [{ meter: "model_input_tokens", total: 12, unit: "tokens" }], records: [{}, {}] })).toEqual({ eventsRead: 10, meters: [{ meter: "model_input_tokens", total: 12, unit: "tokens" }], records: 2 });
  });
});
