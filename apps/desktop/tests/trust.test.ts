/**
 * APP-006: approvals bound to the preview, the effect timeline with unresolved effects
 * kept open, and evidence verified against its digest — pure, no server.
 */

import { describe, expect, it } from "vitest";

import {
  bindApproval,
  decodeApprovals,
  decodeEffects,
  errorState,
  evidenceText,
  isDecidable,
  isUnsettled,
  resolution,
  sha256Hex,
  timeline,
  unresolved,
  verifiedState,
  warnings,
  type ApprovalPreview,
} from "../src/index.js";

const RUN = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const DIGEST = "4ba5eba4d64e598bc80db004e78d4548093a4f021dc076c00e0be1b97b066bb0";
const OTHER = "3b1698580da742407583708ba40ccc0484d44f8848d62ddccc6940023aa1f13c";
const FUTURE = "2999-01-01T00:00:00Z";

function wireApproval(extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    approvals: [
      {
        id: "apr_1",
        workspace_id: "ws_1",
        run_id: RUN,
        effect_id: "eff_1",
        requested_of: [],
        summary: "Approve fs.write (fs.write.host)",
        consequence_preview: {
          recipients: ["ops@example.com"],
          amounts: [{ currency: "USD", minor_units: 1250 }],
          targets: ["fs.write"],
          data_classes: ["pii"],
          untrusted_origin: { trust: "UNTRUSTED_EXTERNAL", source_refs: ["tool_call:tc_1", "run:run_1"] },
        },
        params_digest: DIGEST,
        capability_projection_id: "cap_1",
        status: "requested",
        expires_at: FUTURE,
        created_at: "2026-09-20T00:00:00Z",
        effect: { class: "fs.write.host", tier: 3, resource: { kind: "fs", selector: "/work/root/out.txt" }, target: { kind: "server", id: "tool:fs.write" }, status: "PROPOSED", tool_call_id: "tc_1", tool_name: "fs.write", args: { path: "/work/root/out.txt" } },
        receipts: [],
        ...extra,
      },
    ],
  };
}

describe("approval binding", () => {
  it("decodes the preview with its consequences, origin, effect and expiry, and names the warnings", () => {
    const [preview] = decodeApprovals(wireApproval());
    expect(preview).toBeDefined();
    expect(preview?.digest).toBe(DIGEST);
    expect(preview?.effect).toMatchObject({ effectClass: "fs.write.host", tier: 3, toolName: "fs.write", target: { kind: "server", id: "tool:fs.write" } });
    expect(preview?.consequences.untrustedOrigin?.trust).toBe("UNTRUSTED_EXTERNAL");
    expect(preview?.consequences.amounts).toEqual([{ currency: "USD", minorUnits: 1250 }]);
    expect(warnings(preview as ApprovalPreview)).toEqual([
      "untrusted origin: UNTRUSTED_EXTERNAL (tool_call:tc_1, run:run_1)",
      "tier 3 effect: a fresh approval is required every time",
      "moves money",
      "involves pii data",
    ]);
    expect(isDecidable(preview as ApprovalPreview)).toBe(true);
    expect(isDecidable({ ...(preview as ApprovalPreview), expiresAt: "2000-01-01T00:00:00Z" })).toBe(false);
    expect(decodeApprovals({ approvals: [{ id: "apr_2" }] })).toEqual([]);
  });

  it("cannot approve a changed payload after preview, nor a superseded, expired or decided request", () => {
    const [previewed] = decodeApprovals(wireApproval());
    const current = previewed as ApprovalPreview;
    expect(bindApproval(current, current)).toEqual({ ok: true, params: { approval_request_id: "apr_1", params_digest: DIGEST } });
    // The payload changed after the preview: a different digest on the same request.
    expect(bindApproval(current, { ...current, digest: OTHER })).toMatchObject({ ok: false, code: "APPROVAL_SUPERSEDED" });
    expect(bindApproval(current, { ...current, status: "superseded" })).toMatchObject({ ok: false, code: "APPROVAL_SUPERSEDED" });
    expect(bindApproval(current, { ...current, status: "expired" })).toMatchObject({ ok: false, code: "APPROVAL_EXPIRED" });
    expect(bindApproval(current, { ...current, expiresAt: "2000-01-01T00:00:00Z" })).toMatchObject({ ok: false, code: "APPROVAL_EXPIRED" });
    expect(bindApproval(current, { ...current, status: "granted" })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(bindApproval(current, { ...current, id: "apr_9" })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    // The params the command carries are the previewed digest, never the current one.
    const binding = bindApproval(current, current);
    expect(binding.ok && binding.params.params_digest).toBe(DIGEST);
  });
});

describe("effect timeline", () => {
  const wire = {
    effects: [
      { id: "eff_ok", run_id: RUN, effect_class: "read.internal", tier: 0, resource: { kind: "fs", selector: "**" }, params_digest: DIGEST, status: "SETTLED_SUCCESS", unsettled: false, terminal: true, outcome: { kind: "success", evidence_ids: ["evd_1"] }, created_at: "2026-09-20T00:00:01Z", settled_at: "2026-09-20T00:00:02Z" },
      { id: "eff_unknown", run_id: RUN, effect_class: "network.egress", tier: 2, resource: { kind: "domain", selector: "api.example.com" }, params_digest: OTHER, status: "OUTCOME_UNKNOWN", unsettled: true, terminal: false, created_at: "2026-09-20T00:00:03Z" },
      { id: "eff_recon", run_id: RUN, effect_class: "message.send", tier: 2, resource: { kind: "domain", selector: "mail" }, params_digest: OTHER, status: "RECONCILING", unsettled: true, terminal: false, reconciliation: { strategy: "poll", attempts: 2, last_at: "2026-09-20T00:00:05Z", result: null }, created_at: "2026-09-20T00:00:04Z" },
      { id: "eff_manual", run_id: RUN, effect_class: "payment", tier: 4, resource: { kind: "domain", selector: "bank" }, params_digest: OTHER, status: "RECONCILIATION_MANUAL", unsettled: true, terminal: false, created_at: "2026-09-20T00:00:06Z" },
      { id: "eff_denied", run_id: RUN, effect_class: "fs.write.host", tier: 3, resource: { kind: "fs", selector: "/etc" }, params_digest: OTHER, status: "DENIED", unsettled: false, terminal: false, created_at: "2026-09-20T00:00:07Z" },
      { id: "eff_bad", status: "WHATEVER" },
    ],
  };

  it("keeps unknown, reconciling and manual effects visibly unresolved and distinct", () => {
    const effects = decodeEffects(wire);
    expect(effects.map((effect) => effect.id)).toEqual(["eff_ok", "eff_unknown", "eff_recon", "eff_manual", "eff_denied"]);
    expect(unresolved(effects).map((effect) => effect.id)).toEqual(["eff_unknown", "eff_recon", "eff_manual"]);
    expect(effects.map(resolution)).toEqual(["succeeded", "unknown", "reconciling", "needs_human", "denied"]);
    expect(isUnsettled("OUTCOME_UNKNOWN")).toBe(true);
    expect(isUnsettled("DISPATCHED")).toBe(true);
    expect(isUnsettled("SETTLED_FAILED")).toBe(false);
    // The server's flag wins even when the page would not infer it, and vice versa.
    const [forced] = decodeEffects({ effects: [{ id: "eff_x", effect_class: "x", status: "SETTLED_SUCCESS", unsettled: true, resource: {}, params_digest: "d" }] });
    expect(forced?.unsettled).toBe(true);
    const entries = timeline(effects);
    expect(entries.map((entry) => `${entry.kind}:${entry.id}`)).toEqual([
      "effect:eff_ok",
      "evidence:evd_1",
      "effect:eff_unknown",
      "effect:eff_recon",
      "effect:eff_manual",
      "effect:eff_denied",
    ]);
    expect(entries.find((entry) => entry.id === "eff_unknown")?.resolution).toBe("unknown");
    expect(entries.find((entry) => entry.id === "eff_recon")?.resolution).toBe("reconciling");
  });
});

describe("evidence digest", () => {
  it("verifies bytes against the labelled digest and turns a mismatch into an error state", async () => {
    const bytes = new TextEncoder().encode("exit 0\nall tests passed\n");
    const digest = await sha256Hex(bytes);
    expect(digest).toHaveLength(64);
    const good = await verifiedState("evd_1", bytes, digest, evidenceText);
    expect(good).toMatchObject({ kind: "verified", digest, text: "exit 0\nall tests passed\n" });
    const bad = await verifiedState("evd_1", bytes, OTHER, evidenceText);
    expect(bad).toMatchObject({ kind: "digest_error", expected: OTHER, actual: digest, source: "page" });
    const server = errorState("evd_1", { details: { kind: "digest_mismatch", expected: DIGEST, actual: OTHER } });
    expect(server).toEqual({ kind: "digest_error", evidenceId: "evd_1", expected: DIGEST, actual: OTHER, source: "server" });
    expect(errorState("evd_1", new Error("boom"))).toEqual({ kind: "unavailable", evidenceId: "evd_1", reason: "boom" });
    expect(evidenceText(new Uint8Array([0xff, 0xfe, 0x00]))).toBeNull();
    expect(evidenceText(new TextEncoder().encode("x".repeat(30)), 10)).toBe("xxxxxxxxxx\n…[truncated]");
  });
});
