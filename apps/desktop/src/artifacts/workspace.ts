/**
 * Artifact workspace (APP-008): the browser's state over CORE-007's projections.
 *
 * Artifacts are first-class: they are listed by workspace, not by chat, and a run's end
 * does not remove them. Everything here is decoded from `GET /v1/artifacts`,
 * `/v1/artifacts/{id}/versions`, `/v1/evidence/{id}` and `/v1/search`; the reducer holds
 * identities and projections, never bytes it could replay, and the preview decision is
 * `preview.ts`'s.
 */

import { decodeQualityRecord, type QualityRecord } from "./outputs.js";
import type { PreviewPlan } from "./preview.js";

export type ArtifactKind =
  | "document"
  | "spreadsheet"
  | "presentation"
  | "code"
  | "data"
  | "image"
  | "audio"
  | "video"
  | "archive"
  | "other";

export const ARTIFACT_KINDS: readonly ArtifactKind[] = [
  "document",
  "spreadsheet",
  "presentation",
  "code",
  "data",
  "image",
  "audio",
  "video",
  "archive",
  "other",
];

export interface ProducedBy {
  readonly runId: string | null;
  readonly stepId: string | null;
  readonly userId: string | null;
}

export interface ArtifactRow {
  readonly id: string;
  readonly kind: ArtifactKind;
  readonly title: string;
  readonly role: string | null;
  readonly origin: { readonly kind: string; readonly ref: string };
  readonly currentVersionId: string | null;
  readonly versionCount: number;
  readonly mediaType: string | null;
  readonly sizeBytes: number | null;
  readonly contentDigest: string | null;
  readonly producedBy: ProducedBy;
  readonly updatedAt: string;
}

export interface ArtifactVersion {
  readonly id: string;
  readonly seq: number;
  readonly contentDigest: string;
  readonly sizeBytes: number;
  readonly mediaType: string;
  readonly producedBy: ProducedBy;
  readonly evidenceIds: readonly string[];
  readonly parentVersionId: string | null;
  /** The authority's source-format check for this version (CAP-003); null when never checked. */
  readonly formatCheck: FormatCheck | null;
  /** The checks an output tool ran to make this version (ADP-006); null for a version no output tool made. */
  readonly qualityRecord: QualityRecord | null;
  readonly createdAt: string;
}

/** `artifact_versions.format_check` (DOMAIN.md §10.1). */
export interface FormatCheck {
  readonly ok: boolean;
  readonly rule: string;
  readonly detail: string;
}

export interface ArtifactHistory {
  readonly artifactId: string;
  readonly title: string;
  readonly kind: ArtifactKind;
  readonly origin: { readonly kind: string; readonly ref: string };
  readonly currentVersionId: string | null;
  readonly versions: readonly ArtifactVersion[];
}

export interface EvidenceLink {
  readonly id: string;
  readonly kind: string;
  readonly runId: string | null;
  readonly stepId: string | null;
  readonly summary: string | null;
  readonly capturedAt: string;
}

export interface LoadedPreview {
  readonly versionId: string;
  readonly plan: PreviewPlan;
  /** Text mode: the decoded text (a DOM text node, never markup). */
  readonly text: string | null;
  /** Image mode: a `data:` URL the renderer's `<img>` decodes in-process. */
  readonly imageUrl: string | null;
  readonly truncated: boolean;
}

export interface WorkspaceFilter {
  readonly kind: ArtifactKind | null;
  readonly runId: string | null;
}

export interface ArtifactWorkspaceState {
  readonly workspaceId: string;
  readonly artifacts: readonly ArtifactRow[];
  readonly filter: WorkspaceFilter;
  readonly selectedId: string | null;
  readonly history: ArtifactHistory | null;
  readonly selectedVersionId: string | null;
  /** The other side of a diff, when the user compares two text versions. */
  readonly compareVersionId: string | null;
  readonly preview: LoadedPreview | null;
  readonly evidence: Readonly<Record<string, EvidenceLink>>;
  readonly stale: { readonly list: boolean; readonly history: boolean };
  readonly notice: string | null;
}

export type ArtifactWorkspaceAction =
  | { readonly type: "list.loaded"; readonly artifacts: readonly ArtifactRow[] }
  | { readonly type: "filter"; readonly filter: Partial<WorkspaceFilter> }
  | { readonly type: "select"; readonly artifactId: string | null }
  | { readonly type: "history.loaded"; readonly history: ArtifactHistory }
  | { readonly type: "version.select"; readonly versionId: string }
  | { readonly type: "version.compare"; readonly versionId: string | null }
  | { readonly type: "preview.loaded"; readonly preview: LoadedPreview }
  | { readonly type: "evidence.loaded"; readonly evidence: EvidenceLink }
  | { readonly type: "artifact.changed"; readonly artifactId: string }
  /** A version this page just saved (an edit, CAP-003): reload the history and show it. */
  | { readonly type: "version.created"; readonly artifactId: string; readonly versionId: string }
  | { readonly type: "notice"; readonly notice: string | null };

export function initialArtifactWorkspace(workspaceId: string): ArtifactWorkspaceState {
  return {
    workspaceId,
    artifacts: [],
    filter: { kind: null, runId: null },
    selectedId: null,
    history: null,
    selectedVersionId: null,
    compareVersionId: null,
    preview: null,
    evidence: {},
    stale: { list: true, history: false },
    notice: null,
  };
}

export function artifactWorkspaceReducer(state: ArtifactWorkspaceState, action: ArtifactWorkspaceAction): ArtifactWorkspaceState {
  switch (action.type) {
    case "list.loaded":
      return { ...state, artifacts: action.artifacts, stale: { ...state.stale, list: false } };
    case "filter":
      return { ...state, filter: { ...state.filter, ...action.filter }, stale: { ...state.stale, list: true } };
    case "select":
      if (action.artifactId === state.selectedId) {
        return state;
      }
      return {
        ...state,
        selectedId: action.artifactId,
        history: null,
        selectedVersionId: null,
        compareVersionId: null,
        preview: null,
        stale: { ...state.stale, history: action.artifactId !== null },
      };
    case "history.loaded": {
      if (action.history.artifactId !== state.selectedId) {
        return state;
      }
      const latest = action.history.currentVersionId ?? action.history.versions[action.history.versions.length - 1]?.id ?? null;
      const stillThere = (id: string | null): string | null => (id && action.history.versions.some((version) => version.id === id) ? id : null);
      return {
        ...state,
        history: action.history,
        selectedVersionId: stillThere(state.selectedVersionId) ?? latest,
        compareVersionId: stillThere(state.compareVersionId),
        stale: { ...state.stale, history: false },
      };
    }
    case "version.select":
      if (!state.history?.versions.some((version) => version.id === action.versionId)) {
        return state;
      }
      return { ...state, selectedVersionId: action.versionId, preview: state.preview?.versionId === action.versionId ? state.preview : null };
    case "version.compare":
      if (action.versionId !== null && !state.history?.versions.some((version) => version.id === action.versionId)) {
        return state;
      }
      return { ...state, compareVersionId: action.versionId };
    case "preview.loaded":
      if (action.preview.versionId !== state.selectedVersionId) {
        return state;
      }
      return { ...state, preview: action.preview };
    case "evidence.loaded":
      return { ...state, evidence: { ...state.evidence, [action.evidence.id]: action.evidence } };
    case "artifact.changed":
      return {
        ...state,
        stale: { list: true, history: state.stale.history || state.selectedId === action.artifactId },
      };
    case "version.created":
      if (action.artifactId !== state.selectedId) {
        return state;
      }
      return {
        ...state,
        selectedVersionId: action.versionId,
        compareVersionId: null,
        preview: null,
        stale: { list: true, history: true },
      };
    case "notice":
      return { ...state, notice: action.notice };
    default:
      return state;
  }
}

/** The selected version, when the history is loaded. */
export function selectedVersion(state: ArtifactWorkspaceState): ArtifactVersion | null {
  return state.history?.versions.find((version) => version.id === state.selectedVersionId) ?? null;
}

/** The version the selected one is compared with: explicit, else its parent, else none. */
export function comparedVersion(state: ArtifactWorkspaceState): ArtifactVersion | null {
  const versions = state.history?.versions ?? [];
  if (state.compareVersionId) {
    return versions.find((version) => version.id === state.compareVersionId) ?? null;
  }
  const current = selectedVersion(state);
  if (!current) {
    return null;
  }
  return versions.find((version) => version.id === current.parentVersionId) ?? versions.find((version) => version.seq === current.seq - 1) ?? null;
}

// ---- decoding the projections ----

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function number(value: unknown): number | null {
  return typeof value === "number" ? value : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function kind(value: unknown): ArtifactKind {
  return typeof value === "string" && (ARTIFACT_KINDS as readonly string[]).includes(value) ? (value as ArtifactKind) : "other";
}

function producedBy(value: unknown): ProducedBy {
  const raw = record(value);
  return { runId: text(raw["run_id"]), stepId: text(raw["step_id"]), userId: text(raw["user_id"]) };
}

function origin(value: unknown): { kind: string; ref: string } {
  const raw = record(value);
  return { kind: text(raw["kind"]) ?? "unknown", ref: text(raw["ref"]) ?? "" };
}

/** Decode `GET /v1/artifacts`. */
export function decodeArtifacts(payload: unknown): ArtifactRow[] {
  const rows: ArtifactRow[] = [];
  for (const raw of list(record(payload)["artifacts"])) {
    const row = record(raw);
    const id = text(row["id"]);
    if (!id) {
      continue;
    }
    rows.push({
      id,
      kind: kind(row["kind"]),
      title: text(row["title"]) ?? id,
      role: text(row["role"]),
      origin: origin(row["origin"]),
      currentVersionId: text(row["current_version_id"]),
      versionCount: number(row["version_count"]) ?? 0,
      mediaType: text(row["media_type"]),
      sizeBytes: number(row["size_bytes"]),
      contentDigest: text(row["content_digest"]),
      producedBy: producedBy(row["produced_by"]),
      updatedAt: text(row["updated_at"]) ?? "",
    });
  }
  return rows;
}

/** Decode `GET /v1/artifacts/{id}/versions`. */
function formatCheck(raw: unknown): FormatCheck | null {
  if (!raw || typeof raw !== "object") {
    return null;
  }
  const check = raw as Record<string, unknown>;
  if (typeof check["ok"] !== "boolean") {
    return null;
  }
  return { ok: check["ok"], rule: text(check["rule"]) ?? "", detail: text(check["detail"]) ?? "" };
}

/** The kinds a person edits as text, and the media types that text is (DOMAIN.md §10.1). */
const TEXT_EDITABLE: Readonly<Record<string, readonly string[]>> = {
  document: ["text/markdown", "text/plain"],
  spreadsheet: ["text/csv", "text/tab-separated-values"],
  presentation: ["application/vnd.quansio.slides+json"],
  code: ["application/vnd.quansio.code+json"],
  data: ["application/json"],
};

/** Whether a version of this kind can be edited in place and saved as the next version. */
export function editableAsText(artifactKind: string, mediaType: string): boolean {
  return (TEXT_EDITABLE[artifactKind] ?? []).includes(mediaType);
}

/** A one-line account of a version's format check. */
export function formatCheckLabel(check: FormatCheck | null): string {
  if (!check) {
    return "not checked";
  }
  if (check.ok) {
    return "valid source";
  }
  // A PDF or a Word file is stored as a file; it is not an editable source.
  return check.rule === "artifact.media_type" ? `stored as a file, not an editable source (${check.detail})` : `fails ${check.rule}: ${check.detail}`;
}

export function decodeHistory(payload: unknown): ArtifactHistory {
  const root = record(payload);
  const artifact = record(root["artifact"]);
  const artifactId = text(artifact["id"]);
  if (!artifactId) {
    throw new Error("version history has no artifact");
  }
  const versions: ArtifactVersion[] = [];
  for (const raw of list(root["versions"])) {
    const row = record(raw);
    const id = text(row["id"]);
    if (!id) {
      continue;
    }
    versions.push({
      id,
      seq: number(row["seq"]) ?? versions.length + 1,
      contentDigest: text(row["content_digest"]) ?? "",
      sizeBytes: number(row["size_bytes"]) ?? 0,
      mediaType: text(row["media_type"]) ?? "application/octet-stream",
      producedBy: producedBy(row["produced_by"]),
      evidenceIds: list(row["evidence_ids"]).filter((item): item is string => typeof item === "string"),
      parentVersionId: text(row["parent_version_id"]),
      formatCheck: formatCheck(row["format_check"]),
      qualityRecord: decodeQualityRecord(row["quality_record"]),
      createdAt: text(row["created_at"]) ?? "",
    });
  }
  versions.sort((a, b) => a.seq - b.seq);
  return {
    artifactId,
    title: text(artifact["title"]) ?? artifactId,
    kind: kind(artifact["kind"]),
    origin: origin(artifact["origin"]),
    currentVersionId: text(artifact["current_version_id"]),
    versions,
  };
}

/** Decode `GET /v1/evidence/{id}`. */
export function decodeEvidence(payload: unknown): EvidenceLink {
  const row = record(payload);
  const id = text(row["id"]);
  if (!id) {
    throw new Error("evidence has no id");
  }
  return {
    id,
    kind: text(row["kind"]) ?? "unknown",
    runId: text(row["run_id"]),
    stepId: text(row["step_id"]),
    summary: text(row["inline_summary"]),
    capturedAt: text(row["captured_at"]) ?? "",
  };
}

// ---- diff ----

export interface DiffLine {
  readonly kind: "same" | "added" | "removed";
  readonly text: string;
}

/** Largest input the line diff handles; beyond it the caller shows both texts instead. */
export const MAX_DIFF_LINES = 4_000;

/** A line diff (LCS) between two texts, oldest first; null when either side is too long. */
export function diffLines(before: string, after: string): DiffLine[] | null {
  const a = before.split("\n");
  const b = after.split("\n");
  if (a.length > MAX_DIFF_LINES || b.length > MAX_DIFF_LINES) {
    return null;
  }
  // LCS table over lines; sizes are bounded above so the quadratic table is fine.
  const width = b.length + 1;
  const table = new Uint32Array((a.length + 1) * width);
  const at = (i: number, j: number): number => table[i * width + j] ?? 0;
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      table[i * width + j] = a[i] === b[j] ? at(i + 1, j + 1) + 1 : Math.max(at(i + 1, j), at(i, j + 1));
    }
  }
  const lines: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    const left = a[i] ?? "";
    const right = b[j] ?? "";
    if (left === right) {
      lines.push({ kind: "same", text: left });
      i += 1;
      j += 1;
    } else if (at(i + 1, j) >= at(i, j + 1)) {
      lines.push({ kind: "removed", text: left });
      i += 1;
    } else {
      lines.push({ kind: "added", text: right });
      j += 1;
    }
  }
  for (; i < a.length; i += 1) {
    lines.push({ kind: "removed", text: a[i] ?? "" });
  }
  for (; j < b.length; j += 1) {
    lines.push({ kind: "added", text: b[j] ?? "" });
  }
  return lines;
}

/** Whether two versions can be diffed: both text, both bounded. */
export function diffApplicable(left: ArtifactVersion, right: ArtifactVersion, isText: (mediaType: string) => boolean): boolean {
  return isText(left.mediaType) && isText(right.mediaType) && left.sizeBytes <= 1_000_000 && right.sizeBytes <= 1_000_000;
}
