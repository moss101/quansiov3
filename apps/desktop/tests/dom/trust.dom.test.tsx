// @vitest-environment jsdom
/** APP-006 on a page: the Trust view over the real preload API — approvals bound to their preview, effects, evidence. */

import { createHash } from "node:crypto";

import { afterEach, describe, expect, it } from "vitest";

import { Trust } from "../../src/renderer/Trust.js";
import { click, mount, text, type Rig } from "./rig.js";

const FUTURE = "2999-01-01T00:00:00Z";

function approval(id: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, workspace_id: "ws_1", run_id: "run_1", effect_id: "eff_1", summary: `send ${id}`, params_digest: "d1".repeat(32), status: "requested",
    expires_at: FUTURE, created_at: "2026-09-20T00:00:00Z",
    consequence_preview: {
      recipients: ["a@example.com"], amounts: [{ currency: "USD", minor_units: 1250 }], targets: ["inbox"], data_classes: ["pii"],
      untrusted_origin: { trust: "web", source_refs: ["page:1"] },
    },
    effect: { class: "email.send", tier: 3, resource: { kind: "mail", selector: "*" }, target: { kind: "mailbox", id: "mb_1" }, tool_name: "mail.send", args: { to: "a@example.com" } },
    ...extra,
  };
}

function effectRow(id: string, status: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, workspace_id: "ws_1", run_id: "run_1", effect_class: "email.send", tier: 3, resource: { kind: "mail", selector: "*" }, status,
    created_at: `2026-09-20T00:00:0${id.slice(-1)}Z`, ...extra,
  };
}

const HELLO = Buffer.from("hello evidence");
const HELLO_DIGEST = createHash("sha256").update(HELLO).digest("hex");

function effects(): Record<string, unknown> {
  return {
    effects: [
      effectRow("eff_1", "SETTLED_SUCCESS", { outcome: { kind: "sent", evidence_ids: ["ev_1"] }, settled_at: "2026-09-20T00:01:00Z" }),
      effectRow("eff_2", "OUTCOME_UNKNOWN", { reconciliation: { strategy: "lookup", attempts: 2, last_at: "x", result: null } }),
      effectRow("eff_3", "RECONCILING"),
    ],
  };
}

const ok = { ok: true, result: {} };
let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

describe("the Trust view", () => {
  it("lists approvals with their count and the timeline with unsettled effects kept open", async () => {
    const inspected: string[] = [];
    rig = await mount(<Trust workspaceId="ws_1" onInspectRun={(id) => { inspected.push(id); }} />, { "approvals.list": { approvals: [approval("apr_1"), approval("apr_2", { status: "granted" })] }, "effects.list": effects() });
    expect(rig.container.querySelector(".approvals h2")?.textContent).toContain("(1)");
    expect(rig.container.querySelector("[data-resolution=unknown]")).not.toBeNull();
    expect(rig.container.querySelector("[data-resolution=reconciling]")).not.toBeNull();
    expect(rig.container.querySelector(".unresolved")?.textContent).toContain("lookup ×2");
    expect(rig.container.querySelectorAll(".entries li").length).toBe(4);
    await click(rig, /^Run$/);
    expect(inspected).toEqual(["run_1"]);
  });

  it("scopes the reads to a run", async () => {
    rig = await mount(<Trust workspaceId="ws_1" runId="run_9" />, { "approvals.list": { approvals: [] }, "effects.list": { effects: [] } });
    expect(sent("approvals.list")[0]?.payload).toEqual({ runId: "run_9" });
    expect(sent("effects.list")[0]?.payload).toEqual({ runId: "run_9" });
  });

  it("previews the full request with its warnings, and approves with the previewed digest", async () => {
    rig = await mount(<Trust workspaceId="ws_1" />, { "approvals.list": { approvals: [approval("apr_1")] }, "effects.list": { effects: [] }, "approvals.approve": ok });
    await click(rig, "send apr_1");
    const dialog = rig.container.querySelector("[role=dialog]");
    expect(dialog?.getAttribute("data-digest")).toBe("d1".repeat(32));
    const body = dialog?.textContent ?? "";
    expect(body).toContain("email.send");
    expect(body).toContain("mail *");
    expect(body).toContain("a@example.com");
    expect(body).toContain("12.5 USD");
    expect(body).toContain("mailbox mb_1");
    expect(body).toContain("mail.send");
    const warns = [...rig.container.querySelectorAll(".warning")].map((node) => node.textContent);
    expect(warns).toEqual(expect.arrayContaining([expect.stringContaining("untrusted origin: web"), expect.stringContaining("tier 3"), "moves money", expect.stringContaining("pii")]));
    await click(rig, "Approve");
    expect(sent("approvals.approve")[0]?.payload).toEqual({ approvalRequestId: "apr_1", paramsDigest: "d1".repeat(32) });
    expect(text(rig)).toContain("Approved.");
    expect(rig.container.querySelector("[role=dialog]")).toBeNull();
  });

  it("denies, and closes a preview without deciding", async () => {
    rig = await mount(<Trust workspaceId="ws_1" />, { "approvals.list": { approvals: [approval("apr_1")] }, "effects.list": { effects: [] }, "approvals.deny": ok });
    await click(rig, "send apr_1");
    await click(rig, "Close");
    expect(rig.container.querySelector("[role=dialog]")).toBeNull();
    await click(rig, "send apr_1");
    await click(rig, "Deny");
    expect(sent("approvals.deny")[0]?.payload).toEqual({ approvalRequestId: "apr_1" });
    expect(text(rig)).toContain("Denied.");
  });

  it("refuses before sending when the request changed under the preview", async () => {
    let digest = "d1".repeat(32);
    rig = await mount(<Trust workspaceId="ws_1" />, { "approvals.list": () => ({ approvals: [approval("apr_1", { params_digest: digest })] }), "effects.list": { effects: [] }, "approvals.approve": ok });
    await click(rig, "send apr_1");
    digest = "e2".repeat(32);
    await click(rig, "Approve");
    expect(sent("approvals.approve")).toHaveLength(0);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("APPROVAL_SUPERSEDED");
    expect(rig.container.querySelector("[role=dialog]")).toBeNull();
  });

  it("says so when the request has disappeared", async () => {
    let gone = false;
    rig = await mount(<Trust workspaceId="ws_1" />, { "approvals.list": () => ({ approvals: gone ? [] : [approval("apr_1")] }), "effects.list": { effects: [] } });
    await click(rig, "send apr_1");
    gone = true;
    await click(rig, "Deny");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("no longer exists");
    expect(sent("approvals.deny")).toHaveLength(0);
  });

  it("shows the server's refusal of a decision, and a decided approval as not decidable", async () => {
    rig = await mount(<Trust workspaceId="ws_1" />, {
      "approvals.list": { approvals: [approval("apr_1"), approval("apr_2", { status: "denied" })] }, "effects.list": { effects: [] },
      "approvals.approve": { ok: false, error: { code: "APPROVAL_EXPIRED", message: "too late", details: null } },
    });
    await click(rig, "send apr_2");
    expect(rig.container.querySelector(".decided")?.textContent).toContain("denied");
    await click(rig, "send apr_1");
    await click(rig, "Approve");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("APPROVAL_EXPIRED: too late");
  });

  it("verifies evidence against its digest before showing it", async () => {
    rig = await mount(<Trust workspaceId="ws_1" />, {
      "approvals.list": { approvals: [] }, "effects.list": effects(),
      "evidence.content": { ok: true, result: { bytesBase64: HELLO.toString("base64"), digest: HELLO_DIGEST } },
    });
    await click(rig, "View evidence");
    const panel = rig.container.querySelector("[data-evidence=ev_1]");
    expect(panel?.getAttribute("data-state")).toBe("verified");
    expect(panel?.textContent).toContain("hello evidence");
    expect(panel?.textContent).toContain(HELLO_DIGEST);
  });

  it("shows non-text evidence by size, a page-side digest mismatch, a server mismatch, and an unavailable read", async () => {
    const binary = Buffer.from([0xff, 0xfe, 0xfd]);
    rig = await mount(<Trust workspaceId="ws_1" />, {
      "approvals.list": { approvals: [] }, "effects.list": effects(),
      "evidence.content": { ok: true, result: { bytesBase64: binary.toString("base64"), digest: createHash("sha256").update(binary).digest("hex") } },
    });
    await click(rig, "View evidence");
    expect(rig.container.querySelector("[data-evidence]")?.textContent).toContain("3 B");

    rig.answer("evidence.content", { ok: true, result: { bytesBase64: HELLO.toString("base64"), digest: "0".repeat(64) } });
    await click(rig, "View evidence");
    expect(rig.container.querySelector("[data-state=digest_error]")?.textContent).toContain("(page)");

    rig.answer("evidence.content", { ok: false, error: { code: "INTEGRITY", message: "bad", details: { kind: "digest_mismatch", expected: "aa", actual: "bb" } } });
    await click(rig, "View evidence");
    expect(rig.container.querySelector("[data-state=digest_error]")?.textContent).toContain("(server)");

    rig.answer("evidence.content", { ok: false, error: { code: "NOT_FOUND", message: "gone" } });
    await click(rig, "View evidence");
    expect(rig.container.querySelector("[data-state=unavailable]")?.textContent).toContain("Evidence unavailable");
  });

  it("reloads on approval and effect events only", async () => {
    rig = await mount(<Trust workspaceId="ws_1" />, { "approvals.list": { approvals: [] }, "effects.list": { effects: [] } });
    const before = sent("effects.list").length;
    await rig.push("stream.frame", { event: { type: "thread.message_posted" } });
    await rig.push("stream.frame", null);
    expect(sent("effects.list").length).toBe(before);
    await rig.push("stream.frame", { event: { type: "effect.settled" } });
    expect(sent("effects.list").length).toBeGreaterThan(before);
  });

  it("shows a failed read as a notice", async () => {
    rig = await mount(<Trust workspaceId="ws_1" />, { "approvals.list": () => Promise.reject(new Error("approvals down")), "effects.list": { effects: [] } });
    expect(rig.container.querySelector(".notice")?.textContent).toContain("approvals down");
  });
});
