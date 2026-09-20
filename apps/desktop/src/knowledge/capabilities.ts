/**
 * Task-scoped capability resolution (APP-011, CORE-004): why a run may do what it may.
 *
 * `GET /v1/runs/{id}` carries the run's capability projection — its inputs in the fixed
 * layer order of DOMAIN.md §6.2 (platform → tenant policy → workspace policy → user role →
 * teammate definition → delegation → active skills → tool declaration → execution target
 * class → user rules) and the grants that resulted. The page lays the ten layers out in
 * that order, absent layers included, so it is visible which inputs the resolution had
 * and which it did not. Active skills appear as an input that contributes no grant.
 */

export const LAYERS = [
  "platform",
  "tenant_policy",
  "workspace_policy",
  "user_role",
  "agent_definition",
  "delegation",
  "active_skills",
  "tool_declaration",
  "execution_target_class",
  "user_rules",
] as const;

export type Layer = (typeof LAYERS)[number];

export interface LayerInput {
  readonly layer: Layer;
  readonly ref: string;
  readonly digest: string;
}

export interface GrantView {
  readonly effectClass: string;
  readonly resource: { readonly kind: string; readonly selector: string } | null;
  readonly constraints: Record<string, unknown>;
}

export interface CapabilityResolution {
  readonly id: string;
  readonly subject: { readonly kind: string; readonly id: string };
  readonly inputs: readonly LayerInput[];
  readonly grants: readonly GrantView[];
  readonly inputsDigest: string;
  readonly computedAt: string;
  readonly expiresAt: string | null;
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

/** Decode the `capability_projection` of a run projection; `null` when the run has none. */
export function decodeCapabilityResolution(runPayload: unknown): CapabilityResolution | null {
  const projection = record(record(runPayload)["capability_projection"]);
  const id = text(projection["id"]);
  if (!id) {
    return null;
  }
  const subject = record(projection["subject"]);
  return {
    id,
    subject: { kind: text(subject["kind"]) ?? "run", id: text(subject["id"]) ?? text(record(runPayload)["id"]) ?? "" },
    inputs: list(projection["inputs"]).map(record).flatMap((input) => {
      const layer = text(input["layer"]);
      const ref = text(input["ref"]);
      return layer && (LAYERS as readonly string[]).includes(layer) && ref ? [{ layer: layer as Layer, ref, digest: text(input["digest"]) ?? "" }] : [];
    }),
    grants: list(projection["grants"]).map(record).flatMap((grant) => {
      const effectClass = text(grant["effect_class"]);
      const resource = record(grant["resource"]);
      return effectClass
        ? [{ effectClass, resource: text(resource["kind"]) ? { kind: text(resource["kind"]) ?? "", selector: text(resource["selector"]) ?? "" } : null, constraints: record(grant["constraints"]) }]
        : [];
    }),
    inputsDigest: text(projection["inputs_digest"]) ?? "",
    computedAt: text(projection["computed_at"]) ?? "",
    expiresAt: text(projection["expires_at"]),
  };
}

export interface LayerRow {
  readonly layer: Layer;
  readonly input: LayerInput | null;
}

/** All ten layers in the fixed order, each with its input or `null` when it was absent. */
export function layersInOrder(resolution: CapabilityResolution): readonly LayerRow[] {
  return LAYERS.map((layer) => ({ layer, input: resolution.inputs.find((input) => input.layer === layer) ?? null }));
}

/** The skill version ids the resolution took as its `active_skills` input. */
export function skillInputs(resolution: CapabilityResolution): readonly string[] {
  const input = resolution.inputs.find((candidate) => candidate.layer === "active_skills");
  return input ? input.ref.split(",").filter((id) => id.startsWith("sklv_")) : [];
}

/** Whether the resolution still stands, by its own expiry. */
export function isCurrent(resolution: CapabilityResolution, now: number = Date.now()): boolean {
  return resolution.expiresAt === null || new Date(resolution.expiresAt).getTime() > now;
}
