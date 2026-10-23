// @vitest-environment jsdom
/** APP-012 on a page: each review/administration page over the real client with a scripted fetch. */

import { afterEach, describe, expect, it } from "vitest";

import { Approvals, Artifacts, Audit, Capabilities, Connectors, Policies, Refusal, Usage, Users, WorkReview } from "../../src/app/pages.js";
import { ApiError } from "../../src/client.js";
import type { Roles } from "../../src/rbac.js";
import { click, fill, mountWeb, text, type WebRig } from "./rig.js";

const DIGEST = "4ba5eba4d64e598bc80db004e78d4548093a4f021dc076c00e0be1b97b066bb0";
const ADMIN: Roles = { tenant: "owner", workspace: "admin" };
const VIEWER: Roles = { tenant: "member", workspace: "viewer" };

const approval = (digest = DIGEST): Record<string, unknown> => ({
  id: "apr_1", workspace_id: "ws_1", run_id: "run_1", effect_id: "eff_1", summary: "Approve fs.write", params_digest: digest, status: "requested",
  expires_at: "2999-01-01T00:00:00Z", created_at: "2026-09-20T00:00:00Z",
  effect: { class: "fs.write.host", tier: 3, tool_name: "fs.write", resource: { kind: "fs", selector: "/x" } },
  consequence_preview: { untrusted_origin: { trust: "UNTRUSTED_EXTERNAL" }, data_classes: ["pii"] }, receipts: [],
});

const members = {
  members: [
    { user: { id: "usr_1", primary_email: "o@example.com", display_name: "Owner" }, membership_id: "tm_1", role: "owner", status: "active", workspaces: [{ membership_id: "wm_1", workspace_id: "ws_1", role: "admin", status: "active" }] },
    { user: { id: "usr_2", primary_email: "v@example.com", display_name: "Viewer" }, membership_id: "tm_2", role: "member", status: "active", workspaces: [{ membership_id: "wm_2", workspace_id: "ws_1", role: "viewer", status: "active" }] },
    { user: { id: "usr_4", primary_email: "s@example.com", display_name: "Sleeper" }, membership_id: "tm_4", role: "member", status: "suspended", workspaces: [] },
  ],
};

const policies = { policies: [{ id: "pol_1", scope: "tenant", workspace_id: null, rules: [{ effect_class: "read.internal", resource_selector: { kind: "fs", selector: "**" }, decision: "allow" }], question_default_ttl_seconds: 86400, approval_default_ttl_seconds: 3600, max_plan_nodes: 25, version: 2 }] };

let rig: WebRig;
afterEach(async () => {
  await rig.unmount();
});

describe("the review pages", () => {
  it("shows the WorkGraph with the nodes that need attention", async () => {
    const graph = { workspace_id: "ws_1", revision: 4, nodes: [{ id: "wn_1", kind: "objective", title: "Ship", status: "active", run: { id: "run_1", status: "WAITING_APPROVAL" }, open_questions: [{ id: "q" }] }, { id: "wn_3", kind: "task", title: "Done", status: "done" }], edges: [], agents: [], blockers: [] };
    rig = await mountWeb((session) => <WorkReview client={session.client()} workspaceId="ws_1" roles={VIEWER} />, { "GET /v1/workgraph": graph });
    expect(text(rig)).toContain("rev 4");
    expect(rig.container.querySelector(".attention")?.textContent).toContain("Ship");
    expect(rig.container.querySelectorAll("[data-node]").length).toBe(2);
  });

  it("previews an approval bound to its digest, then approves it through the command", async () => {
    let listed = [approval()];
    rig = await mountWeb((session) => <Approvals client={session.client()} workspaceId="ws_1" roles={ADMIN} />, {
      "GET /v1/approvals": () => ({ approvals: listed }),
      "POST /v1/commands/ApproveEffect": () => {
        listed = [];
        return { result: {} };
      },
    });
    await click(rig, /Review/i);
    expect(rig.container.querySelector("[role=dialog]")?.getAttribute("data-digest")).toBe(DIGEST);
    expect(rig.container.querySelector(".warnings")?.textContent).toContain("untrusted origin");
    await click(rig, /^Approve$/i);
    const sent = rig.requests.find((request) => request.path === "/v1/commands/ApproveEffect");
    expect((sent?.body as { params: unknown }).params).toEqual({ approval_request_id: "apr_1", params_digest: DIGEST });
    expect(rig.container.querySelector("[role=dialog]")).toBeNull();
  });

  it("refuses to approve when the request changed under the preview, and hides the buttons from a viewer", async () => {
    let digest = DIGEST;
    rig = await mountWeb((session) => <Approvals client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/approvals": () => ({ approvals: [approval(digest)] }) });
    await click(rig, /Review/i);
    digest = "3b1698580da742407583708ba40ccc0484d44f8848d62ddccc6940023aa1f13c";
    await click(rig, /^Deny$/i);
    expect(text(rig)).toContain("APPROVAL_SUPERSEDED");
    expect(rig.requests.some((request) => request.path.startsWith("/v1/commands/"))).toBe(false);
    await rig.unmount();
    rig = await mountWeb((session) => <Approvals client={session.client()} workspaceId="ws_1" roles={VIEWER} />, { "GET /v1/approvals": { approvals: [approval()] } });
    await click(rig, /Review/i);
    expect(rig.container.querySelector(".approve")).toBeNull();
  });

  it("lists artifacts with a download link to the version's content", async () => {
    rig = await mountWeb((session) => <Artifacts client={session.client()} workspaceId="ws_1" roles={VIEWER} />, {
      "GET /v1/artifacts": { artifacts: [{ id: "art_1", kind: "document", title: "Notes", current_version_id: "artv_1", version_count: 2, media_type: "text/markdown", size_bytes: 12, content_digest: DIGEST, updated_at: "" }] },
    });
    expect(rig.container.querySelector<HTMLAnchorElement>("[data-artifact=art_1] a")?.href).toContain("art_1");
    expect(text(rig)).toContain("12 B");
  });

  it("shows a refusal as the refusal, and any other failure as its message", async () => {
    rig = await mountWeb((session) => <Connectors client={session.client()} workspaceId="ws_1" roles={VIEWER} />, { "GET /v1/connectors": { status: 403, body: { code: "SCOPE_FORBIDDEN", message: "not yours", correlation_id: "c", retryable: false } } });
    expect(rig.container.querySelector("[role=alert]")?.getAttribute("data-code")).toBe("SCOPE_FORBIDDEN");
    await rig.unmount();
    rig = await mountWeb(() => <Refusal error={new Error("boom")} />);
    expect(rig.container.querySelector("[role=alert]")?.getAttribute("data-code")).toBe("INTERNAL");
    await rig.unmount();
    rig = await mountWeb(() => <Refusal error={new ApiError(500, { code: "INTERNAL", message: "x", correlation_id: "c", retryable: false, details: null })} />);
    expect(text(rig)).toContain("x");
  });

  it("lists connectors, the audit chain, usage and capabilities", async () => {
    rig = await mountWeb((session) => <Connectors client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/connectors": { connectors: [{ id: "cnx_1", connector_id: "github", display_name: "GitHub", status: "connected", credential_handle: "sec_1" }] } });
    expect(text(rig)).toContain("credential held as a handle");
    await rig.unmount();
    rig = await mountWeb((session) => <Audit client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/audit": { chain: { entries: 2, holds: false, broken_at: "aud_2" }, entries: [{ id: "aud_2", actor: { kind: "user", id: "usr_1" }, action: "SetPolicy", decision: "applied", hash: "h2", prev_hash: "h1", occurred_at: "t" }] } });
    expect(rig.container.querySelector("[data-holds=false]")).not.toBeNull();
    expect(rig.container.querySelector("[data-audit=aud_2]")?.textContent).toContain("user:usr_1");
    await rig.unmount();
    rig = await mountWeb((session) => <Usage client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/usage": { events_read: 10, meters: [{ meter: "model_input_tokens", total: 12, unit: "tokens" }], records: [{}, {}] } });
    expect(text(rig)).toContain("model_input_tokens");
    await rig.unmount();
    rig = await mountWeb((session) => <Capabilities client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/policies": policies, "GET /v1/skills": { skills: [{}, {}] } });
    expect(text(rig)).toContain("tenant: allow");
    expect(text(rig)).toContain("2 skill(s)");
  });
});

describe("the administration pages", () => {
  it("edits a policy's rules and saves them through SetPolicy; an invalid rule is refused before sending", async () => {
    rig = await mountWeb((session) => <Policies client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/policies": policies, "POST /v1/commands/SetPolicy": { result: {} } });
    await click(rig, /Edit/i);
    await click(rig, /Add rule/i);
    await fill(rig, "effect class", "Not Valid");
    await click(rig, /Save/i);
    expect(rig.requests.some((request) => request.path.endsWith("SetPolicy"))).toBe(false);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("VALIDATION_SCHEMA");
    const classes = rig.container.querySelectorAll<HTMLInputElement>("[aria-label='effect class']");
    expect(classes.length).toBe(2);
    await click(rig, /Remove/i);
    await click(rig, /Save/i);
    expect(rig.requests.some((request) => request.path.endsWith("SetPolicy"))).toBe(true);
    expect(rig.container.querySelector("form.editor")).toBeNull();
  });

  it("changes a rule's fields and decision before saving", async () => {
    rig = await mountWeb((session) => <Policies client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/policies": policies, "POST /v1/commands/SetPolicy": { result: {} } });
    await click(rig, /Edit/i);
    await fill(rig, "resource kind", "net");
    await fill(rig, "resource selector", "example.org");
    await fill(rig, "decision", "ask");
    await click(rig, /Save/i);
    const body = rig.requests.find((request) => request.path.endsWith("SetPolicy"))?.body as { params: { rules: { decision: string; resource: { kind: string } }[] } };
    expect(body.params.rules[0]).toMatchObject({ decision: "ask", resource: { kind: "net", selector: "example.org" } });
  });

  it("invites, suspends, activates and removes members as a tenant admin, and offers nothing to a member", async () => {
    rig = await mountWeb((session) => <Users client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/members": members, "POST /v1/commands/InviteMember": { result: {} }, "POST /v1/commands/UpdateMembership": { result: {} }, "POST /v1/commands/RemoveMember": { result: {} } });
    await fill(rig, "example.com", "nova@example.com");
    await click(rig, /Invite/i);
    expect(rig.requests.find((request) => request.path.endsWith("InviteMember"))?.body).toMatchObject({ params: { email: "nova@example.com" } });
    await fill(rig, "example.com", "nope");
    await click(rig, /Invite/i);
    expect(text(rig)).toContain("VALIDATION_SCHEMA");
    await click(rig, /Suspend/i);
    await click(rig, /Activate/i);
    await click(rig, /Remove/i);
    for (const name of ["UpdateMembership", "RemoveMember"]) {
      expect(rig.requests.some((request) => request.path.endsWith(name))).toBe(true);
    }
    await rig.unmount();
    rig = await mountWeb((session) => <Users client={session.client()} workspaceId="ws_1" roles={VIEWER} />, { "GET /v1/members": members });
    expect(rig.container.querySelector(".row-actions button")).toBeNull();
  });

  it("shows a failed command's message as a notice", async () => {
    rig = await mountWeb((session) => <Users client={session.client()} workspaceId="ws_1" roles={ADMIN} />, { "GET /v1/members": members, "POST /v1/commands/UpdateMembership": { status: 409, body: { code: "CONFLICT_STATE", message: "already suspended", correlation_id: "c", retryable: false } } });
    await click(rig, /Suspend/i);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("already suspended");
  });
});
