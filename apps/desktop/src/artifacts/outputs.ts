/**
 * Output generators in the artifact browser (ADP-006, DOMAIN.md §10.4): quality records, deck page
 * states, transcripts and video timelines.
 *
 * Everything here is a projection of what the authority stored. A quality record is written by the
 * server from checks it ran; this module decodes it and says what it says, it never produces one and
 * it never decides whether a deck may be delivered (that is `deck.deliver`'s answer, recomputed from the
 * stored sources on the server). Summaries are of text the preview already fetched, shown as text.
 */

export const DECK_PAGES_MEDIA_TYPE = "application/vnd.quansio.deck-pages+json";
export const TRANSCRIPT_MEDIA_TYPE = "application/vnd.quansio.transcript+json";
export const TIMELINE_MEDIA_TYPE = "application/vnd.quansio.video-timeline+json";

export interface QualityCheck {
  readonly rule: string;
  readonly ok: boolean;
  readonly detail: string;
  readonly subject: string | null;
}

export interface QualityInput {
  readonly artifactId: string;
  readonly versionId: string;
  readonly contentDigest: string;
}

/** `artifact_versions.quality_record` (DOMAIN.md §10.4 A). */
export interface QualityRecord {
  readonly adapter: string;
  readonly adapterVersion: string;
  /** Derived by the authority from the checks; an empty list is never ok. */
  readonly ok: boolean;
  readonly trust: string;
  readonly inputs: readonly QualityInput[];
  readonly checks: readonly QualityCheck[];
  readonly environment: Readonly<Record<string, string>>;
  readonly evidenceIds: readonly string[];
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

function count(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.floor(value) : 0;
}

/** Decode a stored quality record; null when there is none or it is not one (never a guessed one). */
export function decodeQualityRecord(raw: unknown): QualityRecord | null {
  const value = record(raw);
  if (value["record_version"] !== 1 || typeof value["ok"] !== "boolean") {
    return null;
  }
  const adapter = text(value["adapter"]);
  if (!adapter) {
    return null;
  }
  const checks: QualityCheck[] = [];
  for (const entry of list(value["checks"])) {
    const check = record(entry);
    const rule = text(check["rule"]);
    if (rule && typeof check["ok"] === "boolean") {
      checks.push({ rule, ok: check["ok"], detail: text(check["detail"]) ?? "", subject: text(check["subject"]) });
    }
  }
  const environment: Record<string, string> = {};
  for (const [key, entry] of Object.entries(record(value["environment"]))) {
    if (typeof entry === "string" || typeof entry === "number") {
      environment[key] = String(entry);
    }
  }
  return {
    adapter,
    adapterVersion: text(value["adapter_version"]) ?? "",
    // The record's own `ok` is not trusted over its checks: a record that lists a failing check is not ok.
    ok: value["ok"] && checks.length > 0 && checks.every((check) => check.ok),
    trust: text(value["trust"]) ?? "UNKNOWN",
    inputs: list(value["inputs"]).flatMap((entry) => {
      const input = record(entry);
      const artifactId = text(input["artifact_id"]);
      const versionId = text(input["version_id"]);
      return artifactId && versionId ? [{ artifactId, versionId, contentDigest: text(input["content_digest"]) ?? "" }] : [];
    }),
    checks,
    environment,
    evidenceIds: list(value["evidence_ids"]).filter((item): item is string => typeof item === "string"),
  };
}

/** A one-line account of a version's quality record. */
export function qualityLabel(quality: QualityRecord | null): string {
  if (!quality) {
    return "no quality record";
  }
  const failing = quality.checks.filter((check) => !check.ok).length;
  if (quality.ok) {
    return `${quality.adapter}: ${String(quality.checks.length)} check${quality.checks.length === 1 ? "" : "s"} passed`;
  }
  if (quality.checks.length === 0) {
    return `${quality.adapter}: nothing was checked`;
  }
  return `${quality.adapter}: ${String(failing)} of ${String(quality.checks.length)} check${quality.checks.length === 1 ? "" : "s"} failed`;
}

/** The checks that did not hold, first. */
export function orderedChecks(quality: QualityRecord): QualityCheck[] {
  return [...quality.checks.filter((check) => !check.ok), ...quality.checks.filter((check) => check.ok)];
}

/** Content that came from outside the trust boundary must be labelled as such wherever it is shown. */
export function isUntrusted(quality: QualityRecord | null): boolean {
  return quality?.trust === "UNTRUSTED_EXTERNAL";
}

// ---- deck pages ----

export type PageState = "pending" | "edited" | "failed_quality" | "ready";
const PAGE_STATES: readonly PageState[] = ["pending", "edited", "failed_quality", "ready"];

export interface DeckPageSummary {
  readonly slideCount: number;
  readonly states: Readonly<Record<PageState, number>>;
  /** Pages that would stop `deck.deliver`, as the pages Artifact last recorded them. */
  readonly blockers: readonly { readonly index: number; readonly state: PageState; readonly detail: string }[];
  readonly deckArtifactId: string | null;
}

export interface TranscriptSummary {
  readonly language: string;
  readonly durationMs: number;
  readonly segmentCount: number;
  readonly trust: string;
  readonly firstSegments: readonly { readonly startMs: number; readonly text: string }[];
}

export interface TimelineSummary {
  readonly revision: number;
  readonly width: number;
  readonly height: number;
  readonly fps: number;
  readonly mediaCount: number;
  readonly trackCount: number;
  readonly clipCount: number;
  readonly subtitleCount: number;
  readonly lengthMs: number;
}

export type OutputSummary =
  | { readonly kind: "deck_pages"; readonly summary: DeckPageSummary }
  | { readonly kind: "transcript"; readonly summary: TranscriptSummary }
  | { readonly kind: "timeline"; readonly summary: TimelineSummary };

function mediaTypeOf(mediaType: string): string {
  return (mediaType.split(";")[0] ?? "").trim().toLowerCase();
}

function parseJson(source: string): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(source);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function summarizePages(root: Record<string, unknown>): DeckPageSummary | null {
  const slides = list(root["slides"]);
  if (slides.length === 0) {
    return null;
  }
  const states: Record<PageState, number> = { pending: 0, edited: 0, failed_quality: 0, ready: 0 };
  const blockers: { index: number; state: PageState; detail: string }[] = [];
  for (const [position, entry] of slides.entries()) {
    const slide = record(entry);
    const raw = text(slide["state"]);
    const state = PAGE_STATES.find((candidate) => candidate === raw) ?? "pending";
    states[state] += 1;
    if (state !== "ready") {
      const failing = list(slide["checks"])
        .map(record)
        .find((check) => check["ok"] === false);
      const detail = failing ? `${text(failing["rule"]) ?? "check"}: ${text(failing["detail"]) ?? ""}` : state === "pending" ? "not rendered yet" : "";
      blockers.push({ index: typeof slide["index"] === "number" ? slide["index"] : position, state, detail });
    }
  }
  return { slideCount: slides.length, states, blockers, deckArtifactId: text(record(root["deck"])["artifact_id"]) };
}

function summarizeTranscript(root: Record<string, unknown>): TranscriptSummary | null {
  const segments = list(root["segments"]);
  const language = text(root["language"]);
  if (!language) {
    return null;
  }
  return {
    language,
    durationMs: count(root["duration_ms"]),
    segmentCount: segments.length,
    trust: text(root["trust"]) ?? "UNKNOWN",
    firstSegments: segments.slice(0, 5).map((entry) => {
      const segment = record(entry);
      return { startMs: count(segment["start_ms"]), text: text(segment["text"]) ?? "" };
    }),
  };
}

function summarizeTimeline(root: Record<string, unknown>): TimelineSummary | null {
  const output = record(root["output"]);
  if (typeof root["revision"] !== "number") {
    return null;
  }
  let clipCount = 0;
  let lengthMs = 0;
  const tracks = list(root["tracks"]).map(record);
  for (const track of tracks) {
    for (const clip of list(track["clips"]).map(record)) {
      clipCount += 1;
      lengthMs = Math.max(lengthMs, count(clip["start_ms"]) + Math.max(0, count(clip["out_ms"]) - count(clip["in_ms"])));
    }
  }
  return {
    revision: count(root["revision"]),
    width: count(output["width"]),
    height: count(output["height"]),
    fps: count(output["fps"]),
    mediaCount: list(root["media"]).length,
    trackCount: tracks.length,
    clipCount,
    subtitleCount: list(root["subtitles"]).length,
    lengthMs,
  };
}

/** Summarize the text of a version the authority made, by its media type; null for anything else. */
export function summarizeOutput(mediaType: string, source: string): OutputSummary | null {
  const kind = mediaTypeOf(mediaType);
  if (kind !== DECK_PAGES_MEDIA_TYPE && kind !== TRANSCRIPT_MEDIA_TYPE && kind !== TIMELINE_MEDIA_TYPE) {
    return null;
  }
  const root = parseJson(source);
  if (!root) {
    return null;
  }
  if (kind === DECK_PAGES_MEDIA_TYPE) {
    const summary = summarizePages(root);
    return summary ? { kind: "deck_pages", summary } : null;
  }
  if (kind === TRANSCRIPT_MEDIA_TYPE) {
    const summary = summarizeTranscript(root);
    return summary ? { kind: "transcript", summary } : null;
  }
  const summary = summarizeTimeline(root);
  return summary ? { kind: "timeline", summary } : null;
}

/** The role of the pages Artifact of a deck. */
export function deckPagesRole(deckArtifactId: string): string {
  return `deck_pages:${deckArtifactId}`;
}

/** `m:ss` for a time in milliseconds. */
export function clockTime(ms: number): string {
  const total = Math.floor(Math.max(0, ms) / 1000);
  const seconds = total % 60;
  return `${String(Math.floor(total / 60))}:${seconds < 10 ? "0" : ""}${String(seconds)}`;
}
