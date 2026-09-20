/**
 * Skills (APP-011, INT-009): versioned procedural assets on the §11.5 ladder.
 *
 * The page decodes `GET /v1/skills` — every version with its state, whether it resolves
 * into production context, and the edges the ladder allows from it — and sends one edge
 * at a time as `PromoteSkillVersion`. Activation is the one edge a person is tempted to
 * skip to: {@link activation} refuses it for anything but an APPROVED version before any
 * command is sent, and the server refuses it again (`RUNTIME_ILLEGAL_TRANSITION`). A
 * skill's `capability_needs` are displayed as needs; a skill cannot grant (DOMAIN.md
 * §11.5), and nothing on this page can widen a projection.
 */

export type SkillStatus = "draft" | "candidate" | "evaluating" | "approved" | "active" | "deprecated" | "retired" | "rejected";

/** The ladder in order; the only production rung is `active`. */
export const SKILL_LADDER: readonly SkillStatus[] = ["draft", "candidate", "evaluating", "approved", "active", "deprecated", "retired", "rejected"];

export const INSTALL_COMMAND = "InstallSkill";
export const PROMOTE_COMMAND = "PromoteSkillVersion";
export const DISABLE_COMMAND = "DisableSkill";

export interface CapabilityNeed {
  readonly effectClass: string;
  readonly resource: { readonly kind: string; readonly selector: string } | null;
}

export interface SkillManifest {
  readonly instructions: string;
  readonly toolNeeds: readonly string[];
  readonly capabilityNeeds: readonly CapabilityNeed[];
  readonly raw: Record<string, unknown>;
}

export interface SkillVersionView {
  readonly id: string;
  readonly semver: string;
  readonly status: SkillStatus;
  /** ACTIVE *and* named by the skill's pointer — the server's word. */
  readonly resolves: boolean;
  readonly promotableTo: readonly SkillStatus[];
  readonly manifest: SkillManifest;
  readonly provenance: string | null;
  readonly createdAt: string;
}

export interface SkillView {
  readonly id: string;
  readonly name: string;
  readonly scope: string;
  readonly owner: string;
  readonly currentActiveVersionId: string | null;
  readonly versions: readonly SkillVersionView[];
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
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

function status(value: unknown): SkillStatus | null {
  const found = text(value);
  return found && (SKILL_LADDER as readonly string[]).includes(found) ? (found as SkillStatus) : null;
}

function manifest(value: unknown): SkillManifest {
  const raw = record(value);
  return {
    instructions: text(raw["instructions"]) ?? "",
    toolNeeds: strings(raw["tool_needs"]),
    capabilityNeeds: list(raw["capability_needs"]).map(record).flatMap((need) => {
      const effectClass = text(need["effect_class"]);
      const resource = record(need["resource"]);
      return effectClass ? [{ effectClass, resource: text(resource["kind"]) ? { kind: text(resource["kind"]) ?? "", selector: text(resource["selector"]) ?? "" } : null }] : [];
    }),
    raw,
  };
}

/** Decode `GET /v1/skills`. */
export function decodeSkills(payload: unknown): SkillView[] {
  const skills: SkillView[] = [];
  for (const raw of list(record(payload)["skills"])) {
    const row = record(raw);
    const id = text(row["id"]);
    if (!id || !id.startsWith("skl_")) {
      continue;
    }
    const currentActiveVersionId = text(row["current_active_version_id"]);
    skills.push({
      id,
      name: text(row["name"]) ?? "",
      scope: text(row["scope"]) ?? "",
      owner: text(row["owner"]) ?? "",
      currentActiveVersionId,
      versions: list(row["versions"]).map(record).flatMap((version) => {
        const versionId = text(version["id"]);
        const state = status(version["status"]);
        if (!versionId || !state) {
          return [];
        }
        return [
          {
            id: versionId,
            semver: text(version["semver"]) ?? "",
            status: state,
            resolves: version["resolves"] === true,
            promotableTo: list(version["promotable_to"]).map(status).filter((next): next is SkillStatus => next !== null),
            manifest: manifest(version["manifest"]),
            provenance: text(version["provenance"]),
            createdAt: text(version["created_at"]) ?? "",
          },
        ];
      }),
    });
  }
  return skills;
}

/** The versions in production context: what the resolver sees. */
export function activeVersions(skills: readonly SkillView[]): readonly { readonly skill: SkillView; readonly version: SkillVersionView }[] {
  return skills.flatMap((skill) => skill.versions.filter((version) => version.resolves).map((version) => ({ skill, version })));
}

export type Promotion =
  | { readonly ok: true; readonly command: string; readonly params: { readonly skill_version_id: string; readonly to: SkillStatus } }
  | { readonly ok: false; readonly code: "RUNTIME_ILLEGAL_TRANSITION"; readonly reason: string };

/** One edge along the ladder, or the refusal the ladder gives before the command is sent. */
export function promotion(version: SkillVersionView, to: SkillStatus): Promotion {
  if (!version.promotableTo.includes(to)) {
    return {
      ok: false,
      code: "RUNTIME_ILLEGAL_TRANSITION",
      reason: `${version.semver} is ${version.status} and cannot become ${to}${version.promotableTo.length > 0 ? ` (next: ${version.promotableTo.join(", ")})` : " (terminal)"}`,
    };
  }
  return { ok: true, command: PROMOTE_COMMAND, params: { skill_version_id: version.id, to } };
}

/**
 * Activation: only an APPROVED version may become ACTIVE. A draft, candidate, evaluating,
 * rejected, deprecated or retired version is refused here; the ladder has no such edge.
 */
export function activation(version: SkillVersionView): Promotion {
  if (version.status !== "approved") {
    return {
      ok: false,
      code: "RUNTIME_ILLEGAL_TRANSITION",
      reason: `${version.semver} is ${version.status}; only an approved version can be activated`,
    };
  }
  return promotion(version, "active");
}

export type Disable =
  | { readonly ok: true; readonly command: string; readonly params: { readonly skill_id: string } }
  | { readonly ok: false; readonly code: "CONFLICT_STATE"; readonly reason: string };

/** `DisableSkill`: deprecate the active version, or the refusal when there is none. */
export function disable(skill: SkillView): Disable {
  if (!skill.currentActiveVersionId) {
    return { ok: false, code: "CONFLICT_STATE", reason: `${skill.name} has no active version to disable` };
  }
  return { ok: true, command: DISABLE_COMMAND, params: { skill_id: skill.id } };
}

export interface InstallInput {
  readonly scope: "tenant" | "workspace" | "pack";
  readonly name: string;
  readonly owner?: string | null;
  readonly semver: string;
  readonly instructions: string;
  readonly toolNeeds?: readonly string[];
  readonly capabilityNeeds?: readonly CapabilityNeed[];
  readonly provenance?: string | null;
}

export type Install =
  | { readonly ok: true; readonly command: string; readonly params: Record<string, unknown> }
  | { readonly ok: false; readonly code: "VALIDATION_SCHEMA"; readonly reason: string };

const SEMVER = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;
const NAME = /^[a-z0-9][a-z0-9._-]{0,63}$/;

/** `InstallSkill`: a skill with one DRAFT version. Installing never activates. */
export function install(input: InstallInput): Install {
  if (!NAME.test(input.name)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a skill name is lowercase, 1–64 characters, letters, digits, dots, dashes" };
  }
  if (!SEMVER.test(input.semver)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a version is semver (major.minor.patch)" };
  }
  if (input.instructions.trim().length === 0) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "instructions are required" };
  }
  const params: Record<string, unknown> = {
    scope: input.scope,
    name: input.name,
    version: {
      semver: input.semver,
      manifest: {
        instructions: input.instructions,
        tool_needs: [...(input.toolNeeds ?? [])],
        capability_needs: (input.capabilityNeeds ?? []).map((need) => ({ effect_class: need.effectClass, ...(need.resource ? { resource: need.resource } : {}) })),
      },
      ...(input.provenance ? { provenance: input.provenance } : {}),
    },
  };
  if (input.owner) {
    params["owner"] = input.owner;
  }
  return { ok: true, command: INSTALL_COMMAND, params };
}
