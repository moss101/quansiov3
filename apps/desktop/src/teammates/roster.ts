/**
 * Teammate roster (APP-014): persistent, capability-bounded participants as
 * `GET /v1/teammates` projects them — the definition (persona, standing instructions,
 * needs, memory scope, route preference, default target, availability), its AgentThread,
 * its active run, its attention state and its effective capability — and the three
 * commands that govern it: `CreateTeammate`, `UpdateTeammate`, `ArchiveTeammate`.
 *
 * The page never computes authority: `effective_capability` is the server's word (the
 * workspace's policy grants narrowed by the definition's needs, as the next run's
 * projection will resolve them). `narrowing()` only explains it — which needs are met,
 * which the policy does not grant — so a person sees that instructions and needs can take
 * capability away but never add any. Archiving is offered as what it is: the thread is
 * suspended, nothing is deleted.
 */

export type TeammateStatus = "active" | "archived";
export type Availability = "available" | "away";
export type MemoryScope = "user" | "workspace" | "teammate";

export const AVAILABILITIES: readonly Availability[] = ["available", "away"];
export const MEMORY_SCOPES: readonly MemoryScope[] = ["user", "workspace", "teammate"];
export const CREATE_COMMAND = "CreateTeammate";
export const UPDATE_COMMAND = "UpdateTeammate";
export const ARCHIVE_COMMAND = "ArchiveTeammate";
export const MAX_PERSONA_CHARS = 4000;
export const MAX_INSTRUCTIONS_CHARS = 20_000;

export interface GrantView {
  readonly effectClass: string;
  readonly resource: unknown;
}

export interface EffectiveCapability {
  readonly grants: readonly GrantView[];
  readonly policyGrants: number;
  readonly narrowedByNeeds: boolean;
  readonly needsNotGranted: readonly string[];
}

export interface TeammateView {
  readonly id: string;
  readonly workspaceId: string;
  readonly name: string;
  readonly persona: string;
  readonly standingInstructions: string;
  readonly defaultCapabilityNeeds: readonly string[];
  readonly memoryScope: MemoryScope;
  readonly status: TeammateStatus;
  readonly templateId: string | null;
  readonly modelRoutePreference: string | null;
  readonly defaultExecutionTargetId: string | null;
  readonly availability: Availability;
  readonly version: number;
  readonly definitionDigest: string;
  readonly thread: { readonly id: string; readonly status: string; readonly generation: number; readonly suspendedReason: string | null; readonly runs: number } | null;
  readonly activeRun: { readonly id: string; readonly status: string } | null;
  readonly attention: { readonly openQuestions: number; readonly pendingApprovals: number; readonly needsHuman: boolean };
  readonly effectiveCapability: EffectiveCapability;
  readonly updatedAt: string;
}

export interface TemplateView {
  readonly id: string;
  readonly name: string;
  readonly persona: string;
  readonly standingInstructions: string;
  readonly defaultCapabilityNeeds: readonly string[];
  readonly memoryScope: MemoryScope;
}

export interface Roster {
  readonly workspaceId: string;
  readonly teammates: readonly TeammateView[];
  readonly templates: readonly TemplateView[];
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function number(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function strings(value: unknown): string[] {
  return list(value).filter((item): item is string => typeof item === "string");
}

function memoryScope(value: unknown): MemoryScope {
  return typeof value === "string" && (MEMORY_SCOPES as readonly string[]).includes(value) ? (value as MemoryScope) : "workspace";
}

/** Decode one roster entry; a row without an id, a status or a name is not a teammate. */
export function decodeTeammate(raw: unknown): TeammateView | null {
  const row = record(raw);
  const id = text(row["id"]);
  const status = text(row["status"]);
  const name = text(row["name"]);
  if (!id || !id.startsWith("agt_") || !name || (status !== "active" && status !== "archived")) {
    return null;
  }
  const thread = record(row["thread"]);
  const activeRun = record(row["active_run"]);
  const attention = record(row["attention"]);
  const capability = record(row["effective_capability"]);
  const availability = text(row["availability"]);
  return {
    id,
    workspaceId: text(row["workspace_id"]) ?? "",
    name,
    persona: text(row["persona"]) ?? "",
    standingInstructions: text(row["standing_instructions"]) ?? "",
    defaultCapabilityNeeds: strings(row["default_capability_needs"]),
    memoryScope: memoryScope(row["memory_scope"]),
    status,
    templateId: text(row["template_id"]),
    modelRoutePreference: text(row["model_route_preference"]),
    defaultExecutionTargetId: text(row["default_execution_target_id"]),
    availability: availability === "away" ? "away" : "available",
    version: number(row["version"]) ?? 1,
    definitionDigest: text(row["definition_digest"]) ?? "",
    thread: text(thread["id"])
      ? { id: text(thread["id"]) ?? "", status: text(thread["status"]) ?? "", generation: number(thread["generation"]) ?? 0, suspendedReason: text(thread["suspended_reason"]), runs: number(thread["runs"]) ?? 0 }
      : null,
    activeRun: text(activeRun["id"]) ? { id: text(activeRun["id"]) ?? "", status: text(activeRun["status"]) ?? "" } : null,
    attention: {
      openQuestions: number(attention["open_questions"]) ?? 0,
      pendingApprovals: number(attention["pending_approvals"]) ?? 0,
      needsHuman: attention["needs_human"] === true,
    },
    effectiveCapability: {
      grants: list(capability["grants"]).map(record).flatMap((grant) => {
        const effectClass = text(grant["effect_class"]);
        return effectClass ? [{ effectClass, resource: grant["resource"] ?? null }] : [];
      }),
      policyGrants: number(capability["policy_grants"]) ?? 0,
      narrowedByNeeds: capability["narrowed_by_needs"] === true,
      needsNotGranted: strings(capability["needs_not_granted"]),
    },
    updatedAt: text(row["updated_at"]) ?? "",
  };
}

/** Decode `GET /v1/teammates`. */
export function decodeRoster(payload: unknown): Roster {
  const root = record(payload);
  return {
    workspaceId: text(root["workspace_id"]) ?? "",
    teammates: list(root["teammates"]).flatMap((raw) => {
      const decoded = decodeTeammate(raw);
      return decoded ? [decoded] : [];
    }),
    templates: list(root["templates"]).map(record).flatMap((template) => {
      const id = text(template["id"]);
      const name = text(template["name"]);
      return id && name
        ? [{ id, name, persona: text(template["persona"]) ?? "", standingInstructions: text(template["standing_instructions"]) ?? "", defaultCapabilityNeeds: strings(template["default_capability_needs"]), memoryScope: memoryScope(template["memory_scope"]) }]
        : [];
    }),
  };
}

/** The roster as shown: active teammates first, archived ones after — history, not absence. */
export function visibleRoster(teammates: readonly TeammateView[]): readonly TeammateView[] {
  return [...teammates].sort((a, b) => (a.status === b.status ? a.name.localeCompare(b.name) : a.status === "active" ? -1 : 1));
}

/** Teammates a person should look at: an open question, a pending approval, or a suspended thread of an active definition. */
export function needsAttention(teammates: readonly TeammateView[]): readonly TeammateView[] {
  return teammates.filter((teammate) => teammate.attention.needsHuman || (teammate.status === "active" && teammate.thread?.status === "SUSPENDED"));
}

export interface Narrowing {
  /** Needs the policy grants: what the teammate may actually do. */
  readonly met: readonly string[];
  /** Needs the policy does not grant: declared, never granted. */
  readonly unmet: readonly string[];
  /** Policy grants the definition's needs left out. */
  readonly removed: number;
  /** One sentence for the card. */
  readonly summary: string;
}

/**
 * Explain the server's effective capability against the definition's needs. The page
 * cannot compute authority; it can show that needs only ever take away and that the text
 * fields are not in the picture at all.
 */
export function narrowing(teammate: TeammateView): Narrowing {
  const granted = new Set(teammate.effectiveCapability.grants.map((grant) => grant.effectClass));
  const met = teammate.defaultCapabilityNeeds.filter((need) => granted.has(need));
  const unmet = teammate.effectiveCapability.needsNotGranted;
  const removed = teammate.effectiveCapability.narrowedByNeeds ? Math.max(0, teammate.effectiveCapability.policyGrants - teammate.effectiveCapability.grants.length) : 0;
  const summary = teammate.effectiveCapability.narrowedByNeeds
    ? `${String(teammate.effectiveCapability.grants.length)} of ${String(teammate.effectiveCapability.policyGrants)} policy grants (needs narrow; ${unmet.length > 0 ? `${String(unmet.length)} need(s) not granted` : "every need granted"})`
    : `${String(teammate.effectiveCapability.grants.length)} policy grants (no needs declared; nothing narrowed)`;
  return { met, unmet, removed, summary };
}

export type Prepared =
  | { readonly ok: true; readonly command: string; readonly params: Record<string, unknown> }
  | { readonly ok: false; readonly code: "VALIDATION_SCHEMA" | "CONFLICT_STATE"; readonly reason: string };

const EFFECT_CLASS = /^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*$/;

function validText(persona: string | undefined, instructions: string | undefined): string | null {
  if (persona !== undefined && persona.length > MAX_PERSONA_CHARS) {
    return `persona is at most ${String(MAX_PERSONA_CHARS)} characters`;
  }
  if (instructions !== undefined && instructions.length > MAX_INSTRUCTIONS_CHARS) {
    return `standing instructions are at most ${String(MAX_INSTRUCTIONS_CHARS)} characters`;
  }
  return null;
}

function validNeeds(needs: readonly string[] | undefined): string | null {
  for (const need of needs ?? []) {
    if (!EFFECT_CLASS.test(need)) {
      return `${need} is not an effect class`;
    }
  }
  return null;
}

export interface CreateInput {
  readonly workspaceId: string;
  readonly templateId?: string | null;
  readonly name?: string;
  readonly persona?: string;
  readonly standingInstructions?: string;
  readonly defaultCapabilityNeeds?: readonly string[];
  readonly memoryScope?: MemoryScope;
  readonly modelRoutePreference?: string | null;
  readonly defaultExecutionTargetId?: string | null;
  readonly availability?: Availability;
}

/** `CreateTeammate`, validated: a template or a name; text within bounds; needs are effect classes. */
export function createTeammate(input: CreateInput): Prepared {
  if (!input.workspaceId.startsWith("ws_")) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "workspace_id must be a ws_ id" };
  }
  const name = input.name?.trim();
  if (!input.templateId && (!name || name.length > 120)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a teammate needs a template or a name (1–120 characters)" };
  }
  const problem = validText(input.persona, input.standingInstructions) ?? validNeeds(input.defaultCapabilityNeeds);
  if (problem) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: problem };
  }
  const params: Record<string, unknown> = { workspace_id: input.workspaceId };
  if (input.templateId) {
    params["template_id"] = input.templateId;
  }
  if (name) {
    params["name"] = name;
  }
  if (input.persona !== undefined) {
    params["persona"] = input.persona;
  }
  if (input.standingInstructions !== undefined) {
    params["standing_instructions"] = input.standingInstructions;
  }
  if (input.defaultCapabilityNeeds !== undefined) {
    params["default_capability_needs"] = [...input.defaultCapabilityNeeds];
  }
  if (input.memoryScope !== undefined) {
    params["memory_scope"] = input.memoryScope;
  }
  if (input.modelRoutePreference) {
    params["model_route_preference"] = input.modelRoutePreference;
  }
  if (input.defaultExecutionTargetId) {
    params["default_execution_target_id"] = input.defaultExecutionTargetId;
  }
  if (input.availability !== undefined) {
    params["availability"] = input.availability;
  }
  return { ok: true, command: CREATE_COMMAND, params };
}

export interface UpdateInput {
  readonly name?: string;
  readonly persona?: string;
  readonly standingInstructions?: string;
  readonly defaultCapabilityNeeds?: readonly string[];
  readonly memoryScope?: MemoryScope;
  /** `null` clears the preference. */
  readonly modelRoutePreference?: string | null;
  /** `null` clears the default target. */
  readonly defaultExecutionTargetId?: string | null;
  readonly availability?: Availability;
}

/** `UpdateTeammate`, validated; an archived teammate is refused before the command. */
export function updateTeammate(teammate: TeammateView, change: UpdateInput): Prepared {
  if (teammate.status !== "active") {
    return { ok: false, code: "CONFLICT_STATE", reason: `${teammate.name} is archived and cannot be changed` };
  }
  const name = change.name?.trim();
  if (name !== undefined && (name.length === 0 || name.length > 120)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a name is 1–120 characters" };
  }
  const problem = validText(change.persona, change.standingInstructions) ?? validNeeds(change.defaultCapabilityNeeds);
  if (problem) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: problem };
  }
  const params: Record<string, unknown> = { teammate_id: teammate.id };
  if (name !== undefined) {
    params["name"] = name;
  }
  if (change.persona !== undefined) {
    params["persona"] = change.persona;
  }
  if (change.standingInstructions !== undefined) {
    params["standing_instructions"] = change.standingInstructions;
  }
  if (change.defaultCapabilityNeeds !== undefined) {
    params["default_capability_needs"] = [...change.defaultCapabilityNeeds];
  }
  if (change.memoryScope !== undefined) {
    params["memory_scope"] = change.memoryScope;
  }
  if (change.modelRoutePreference !== undefined) {
    params["model_route_preference"] = change.modelRoutePreference;
  }
  if (change.defaultExecutionTargetId !== undefined) {
    params["default_execution_target_id"] = change.defaultExecutionTargetId;
  }
  if (change.availability !== undefined) {
    params["availability"] = change.availability;
  }
  if (Object.keys(params).length === 1) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "nothing to change" };
  }
  return { ok: true, command: UPDATE_COMMAND, params };
}

/** `ArchiveTeammate`: suspends the thread, keeps the lineage; refused for an archived one. */
export function archiveTeammate(teammate: TeammateView): Prepared {
  if (teammate.status === "archived") {
    return { ok: false, code: "CONFLICT_STATE", reason: `${teammate.name} is already archived` };
  }
  return { ok: true, command: ARCHIVE_COMMAND, params: { teammate_id: teammate.id } };
}

/** The roster after an archive as the server reports it: the entry stays, suspended. */
export function archiveInRoster(teammates: readonly TeammateView[], id: string, threadStatus = "SUSPENDED"): readonly TeammateView[] {
  return teammates.map((teammate) =>
    teammate.id === id
      ? { ...teammate, status: "archived", activeRun: null, thread: teammate.thread ? { ...teammate.thread, status: threadStatus, suspendedReason: "teammate archived" } : null }
      : teammate,
  );
}
