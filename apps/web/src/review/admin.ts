/**
 * Administration on the web (APP-012): members, policies, connectors, the audit log and
 * usage as `GET /v1/{members,policies,connectors,audit,usage}` project them, and the
 * commands that change them — `InviteMember`, `UpdateMembership`, `RemoveMember`,
 * `SetPolicy` — validated on the page before they are sent and decided by the server's
 * RBAC when they are.
 */

import { TENANT_ROLES, WORKSPACE_ROLES, type TenantRole, type WorkspaceRole } from "../rbac.js";
import { list, number, record, text } from "./wire.js";

export interface MemberView {
  readonly userId: string;
  readonly email: string;
  readonly displayName: string;
  readonly membershipId: string;
  readonly role: string;
  readonly status: string;
  readonly workspaces: readonly { readonly membershipId: string; readonly workspaceId: string; readonly role: string; readonly status: string }[];
}

export function decodeMembers(payload: unknown): MemberView[] {
  return list(record(payload)["members"]).map(record).flatMap((row) => {
    const user = record(row["user"]);
    const userId = text(user["id"]);
    const membershipId = text(row["membership_id"]);
    return userId && membershipId
      ? [
          {
            userId,
            email: text(user["primary_email"]) ?? "",
            displayName: text(user["display_name"]) ?? "",
            membershipId,
            role: text(row["role"]) ?? "",
            status: text(row["status"]) ?? "",
            workspaces: list(row["workspaces"]).map(record).flatMap((membership) => {
              const id = text(membership["membership_id"]);
              const workspaceId = text(membership["workspace_id"]);
              return id && workspaceId ? [{ membershipId: id, workspaceId, role: text(membership["role"]) ?? "", status: text(membership["status"]) ?? "" }] : [];
            }),
          },
        ]
      : [];
  });
}

export interface PolicyView {
  readonly id: string;
  readonly scope: "tenant" | "workspace";
  readonly workspaceId: string | null;
  readonly rules: readonly PolicyRuleView[];
  readonly questionTtlSeconds: number;
  readonly approvalTtlSeconds: number;
  readonly maxPlanNodes: number;
  readonly version: number;
}

export interface PolicyRuleView {
  readonly effectClass: string;
  readonly resource: { readonly kind: string; readonly selector: string };
  readonly decision: string;
}

export function decodePolicies(payload: unknown): PolicyView[] {
  return list(record(payload)["policies"]).map(record).flatMap((row) => {
    const id = text(row["id"]);
    const scope = text(row["scope"]);
    if (!id || (scope !== "tenant" && scope !== "workspace")) {
      return [];
    }
    return [
      {
        id,
        scope,
        workspaceId: text(row["workspace_id"]),
        rules: list(row["rules"]).map(record).flatMap((rule) => {
          const effectClass = text(rule["effect_class"]);
          const resource = record(rule["resource_selector"] ?? rule["resource"]);
          const decision = text(rule["decision"]);
          return effectClass && decision ? [{ effectClass, resource: { kind: text(resource["kind"]) ?? "", selector: text(resource["selector"]) ?? "" }, decision }] : [];
        }),
        questionTtlSeconds: number(row["question_default_ttl_seconds"]) ?? 0,
        approvalTtlSeconds: number(row["approval_default_ttl_seconds"]) ?? 0,
        maxPlanNodes: number(row["max_plan_nodes"]) ?? 0,
        version: number(row["version"]) ?? 0,
      },
    ];
  });
}

export interface ConnectorView {
  readonly id: string;
  readonly connectorId: string;
  readonly displayName: string;
  readonly status: string;
  readonly workspaceId: string | null;
  readonly hasCredential: boolean;
  readonly lastHealthAt: string | null;
}

export function decodeConnectors(payload: unknown): ConnectorView[] {
  return list(record(payload)["connectors"]).map(record).flatMap((row) => {
    const id = text(row["id"]);
    return id
      ? [{ id, connectorId: text(row["connector_id"]) ?? "", displayName: text(row["display_name"]) ?? id, status: text(row["status"]) ?? "", workspaceId: text(row["workspace_id"]), hasCredential: text(row["credential_handle"]) !== null, lastHealthAt: text(row["last_health_at"]) }]
      : [];
  });
}

export interface AuditEntryView {
  readonly id: string;
  readonly workspaceId: string | null;
  readonly actor: string;
  readonly action: string;
  readonly targetRef: string;
  readonly decision: string;
  readonly reason: string;
  readonly correlationId: string;
  readonly occurredAt: string;
  readonly prevHash: string;
  readonly hash: string;
}

export interface AuditView {
  readonly chain: { readonly entries: number; readonly holds: boolean; readonly brokenAt: string | null };
  readonly entries: readonly AuditEntryView[];
}

export function decodeAudit(payload: unknown): AuditView {
  const root = record(payload);
  const chain = record(root["chain"]);
  return {
    chain: { entries: number(chain["entries"]) ?? 0, holds: chain["holds"] === true, brokenAt: text(chain["broken_at"]) },
    entries: list(root["entries"]).map(record).flatMap((row) => {
      const id = text(row["id"]);
      const hash = text(row["hash"]);
      if (!id || !hash) {
        return [];
      }
      const actor = record(row["actor"]);
      return [
        {
          id,
          workspaceId: text(row["workspace_id"]),
          actor: `${text(actor["kind"]) ?? "?"}:${text(actor["id"]) ?? "?"}`,
          action: text(row["action"]) ?? "",
          targetRef: text(row["target_ref"]) ?? "",
          decision: text(row["decision"]) ?? "",
          reason: text(row["reason"]) ?? "",
          correlationId: text(row["correlation_id"]) ?? "",
          occurredAt: text(row["occurred_at"]) ?? "",
          prevHash: text(row["prev_hash"]) ?? "",
          hash,
        },
      ];
    }),
  };
}

export interface UsageView {
  readonly eventsRead: number;
  readonly meters: readonly { readonly meter: string; readonly total: number; readonly unit: string | null }[];
  readonly records: number;
}

export function decodeUsage(payload: unknown): UsageView {
  const root = record(payload);
  return {
    eventsRead: number(root["events_read"]) ?? 0,
    meters: list(root["meters"]).map(record).flatMap((row) => {
      const meter = text(row["meter"]);
      return meter ? [{ meter, total: number(row["total"]) ?? 0, unit: text(row["unit"]) }] : [];
    }),
    records: list(root["records"]).length,
  };
}

export type Prepared =
  | { readonly ok: true; readonly command: string; readonly params: Record<string, unknown> }
  | { readonly ok: false; readonly code: "VALIDATION_SCHEMA" | "CONFLICT_STATE"; readonly reason: string };

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

/** `InviteMember`, validated. */
export function invite(input: { readonly email: string; readonly role: TenantRole; readonly workspaceId?: string | null; readonly workspaceRole?: WorkspaceRole | null }): Prepared {
  const email = input.email.trim().toLowerCase();
  if (!EMAIL.test(email)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "an email address is required" };
  }
  if (!TENANT_ROLES.includes(input.role)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: `${input.role} is not a tenant role` };
  }
  const params: Record<string, unknown> = { email, role: input.role };
  if (input.workspaceId) {
    if (!input.workspaceId.startsWith("ws_")) {
      return { ok: false, code: "VALIDATION_SCHEMA", reason: "workspace_id must be a ws_ id" };
    }
    params["workspace_id"] = input.workspaceId;
    params["workspace_role"] = input.workspaceRole && WORKSPACE_ROLES.includes(input.workspaceRole) ? input.workspaceRole : "viewer";
  }
  return { ok: true, command: "InviteMember", params };
}

/** `UpdateMembership`: a role in the membership's own vocabulary, or a status. */
export function updateMembership(membershipId: string, change: { readonly role?: string; readonly status?: "active" | "suspended" }): Prepared {
  const tenantScope = membershipId.startsWith("tm_");
  if (!tenantScope && !membershipId.startsWith("wm_")) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "membership_id must be a tm_ or wm_ id" };
  }
  const params: Record<string, unknown> = { membership_id: membershipId };
  if (change.role !== undefined) {
    const known = tenantScope ? (TENANT_ROLES as readonly string[]).includes(change.role) : (WORKSPACE_ROLES as readonly string[]).includes(change.role);
    if (!known) {
      return { ok: false, code: "VALIDATION_SCHEMA", reason: `${change.role} is not a ${tenantScope ? "tenant" : "workspace"} role` };
    }
    params["role"] = change.role;
  }
  if (change.status !== undefined) {
    params["status"] = change.status;
  }
  if (change.role === undefined && change.status === undefined) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "nothing to change" };
  }
  return { ok: true, command: "UpdateMembership", params };
}

/** `RemoveMember`; the page refuses the last owner it can see, the server refuses it for sure. */
export function removeMember(member: MemberView, all: readonly MemberView[]): Prepared {
  if (member.status === "removed") {
    return { ok: false, code: "CONFLICT_STATE", reason: "already removed" };
  }
  const otherOwners = all.filter((other) => other.userId !== member.userId && other.role === "owner" && other.status === "active").length;
  if (member.role === "owner" && member.status === "active" && otherOwners === 0) {
    return { ok: false, code: "CONFLICT_STATE", reason: "the last active owner cannot be removed" };
  }
  return { ok: true, command: "RemoveMember", params: { membership_id: member.membershipId } };
}

export const DECISIONS = ["allow", "ask", "deny"] as const;

/** `SetPolicy`: rules in the typed vocabulary the server parses again. */
export function setPolicy(input: { readonly scope: "tenant" | "workspace"; readonly workspaceId?: string | null; readonly rules: readonly PolicyRuleView[]; readonly questionTtlSeconds?: number; readonly approvalTtlSeconds?: number; readonly maxPlanNodes?: number }): Prepared {
  if (input.scope === "workspace" && !input.workspaceId) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a workspace policy needs a workspace" };
  }
  if (input.scope === "tenant" && input.workspaceId) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a tenant policy names no workspace" };
  }
  for (const rule of input.rules) {
    if (!/^[a-z][a-z0-9_.]*$/.test(rule.effectClass)) {
      return { ok: false, code: "VALIDATION_SCHEMA", reason: `${rule.effectClass} is not an effect class` };
    }
    if (!(DECISIONS as readonly string[]).includes(rule.decision)) {
      return { ok: false, code: "VALIDATION_SCHEMA", reason: `${rule.decision} is not a decision (allow, ask, deny)` };
    }
    if (!rule.resource.kind || !rule.resource.selector) {
      return { ok: false, code: "VALIDATION_SCHEMA", reason: "a rule needs a resource kind and selector" };
    }
  }
  const params: Record<string, unknown> = {
    scope: input.scope,
    rules: input.rules.map((rule) => ({ effect_class: rule.effectClass, resource: { kind: rule.resource.kind, selector: rule.resource.selector }, decision: rule.decision })),
  };
  if (input.workspaceId) {
    params["workspace_id"] = input.workspaceId;
  }
  if (input.questionTtlSeconds !== undefined) {
    params["question_default_ttl_seconds"] = input.questionTtlSeconds;
  }
  if (input.approvalTtlSeconds !== undefined) {
    params["approval_default_ttl_seconds"] = input.approvalTtlSeconds;
  }
  if (input.maxPlanNodes !== undefined) {
    params["max_plan_nodes"] = input.maxPlanNodes;
  }
  return { ok: true, command: "SetPolicy", params };
}
