/**
 * Capability packs and their install reports (ADP-008, DOMAIN.md §11.6).
 *
 * The page decodes `GET /v1/packs`: every pack with the revision that is active and the revisions kept for
 * rollback, and — for a tenant administrator — the recent install reports, each with its state, the phase it
 * reached and its outcome. A report's diagnostics arrive already redacted and bounded by the server, and are shown
 * as plain text only. The one control is `RollbackPack`, offered when a retained revision still has its content;
 * the server decides again.
 */

export const ROLLBACK_COMMAND = "RollbackPack";

export type InstallState = "running" | "succeeded" | "failed";
export type InstallPhase = "fetch" | "verify" | "compile" | "activate" | "done";
export type InstallOutcome = "activated" | "retained_previous" | "failed";
export type Severity = "error" | "warning";

/** The phases in the order an install takes them. */
export const INSTALL_PHASES: readonly InstallPhase[] = ["fetch", "verify", "compile", "activate", "done"];

export interface RevisionView {
  readonly versionId: string;
  readonly revision: string;
  readonly semver: string;
  /** `published` (active), `deprecated` (kept for rollback) or `withdrawn` (history only). */
  readonly status: string;
  /** Whether the content is still held, so the revision can be activated again. */
  readonly hasContent: boolean;
  readonly activatedAt: string | null;
  readonly retiredAt: string | null;
}

export interface PackView {
  readonly id: string;
  readonly name: string;
  readonly active: RevisionView | null;
  readonly revisions: readonly RevisionView[];
}

export interface DiagnosticView {
  readonly phase: string;
  readonly rule: string;
  readonly severity: Severity;
  readonly message: string;
  readonly pointer: string | null;
}

export interface InstallReportView {
  readonly requestId: string;
  readonly pack: string;
  readonly revision: string | null;
  readonly previousRevision: string | null;
  readonly state: InstallState;
  readonly phase: InstallPhase;
  readonly outcome: InstallOutcome | null;
  readonly diagnostics: readonly DiagnosticView[];
  readonly diagnosticsOmitted: number;
  readonly startedAt: string;
  readonly finishedAt: string | null;
}

export interface PacksView {
  readonly packs: readonly PackView[];
  /** `null` when the caller may not read reports (they are a tenant administrator's). */
  readonly reports: readonly InstallReportView[] | null;
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

function oneOf<T extends string>(value: unknown, allowed: readonly T[]): T | null {
  const found = text(value);
  return found !== null && (allowed as readonly string[]).includes(found) ? (found as T) : null;
}

function revision(value: unknown): RevisionView | null {
  const row = record(value);
  const versionId = text(row["version_id"]);
  const hash = text(row["revision"]);
  if (!versionId || !hash) {
    return null;
  }
  return {
    versionId,
    revision: hash,
    semver: text(row["semver"]) ?? "",
    status: text(row["status"]) ?? "",
    hasContent: row["has_content"] === true,
    activatedAt: text(row["activated_at"]),
    retiredAt: text(row["retired_at"]),
  };
}

function diagnostic(value: unknown): DiagnosticView | null {
  const row = record(value);
  const rule = text(row["rule"]);
  const severity = oneOf(row["severity"], ["error", "warning"] as const);
  if (!rule || !severity) {
    return null;
  }
  return { phase: text(row["phase"]) ?? "", rule, severity, message: text(row["message"]) ?? "", pointer: text(row["pointer"]) };
}

function report(value: unknown): InstallReportView | null {
  const row = record(value);
  const requestId = text(row["request_id"]);
  const state = oneOf(row["state"], ["running", "succeeded", "failed"] as const);
  const phase = oneOf(row["phase"], INSTALL_PHASES);
  if (!requestId || !state || !phase) {
    return null;
  }
  const omitted = row["diagnostics_omitted"];
  return {
    requestId,
    pack: text(row["pack"]) ?? "",
    revision: text(row["revision"]),
    previousRevision: text(row["previous_revision"]),
    state,
    phase,
    outcome: oneOf(row["outcome"], ["activated", "retained_previous", "failed"] as const),
    diagnostics: list(row["diagnostics"]).flatMap((item) => {
      const decoded = diagnostic(item);
      return decoded ? [decoded] : [];
    }),
    diagnosticsOmitted: typeof omitted === "number" && omitted > 0 ? omitted : 0,
    startedAt: text(row["started_at"]) ?? "",
    finishedAt: text(row["finished_at"]),
  };
}

/** Decode `GET /v1/packs`. A row that is not one the server could have sent is dropped, not guessed at. */
export function decodePacks(payload: unknown): PacksView {
  const body = record(payload);
  const packs = list(body["packs"]).flatMap((raw) => {
    const row = record(raw);
    const id = text(row["pack_id"]);
    const name = text(row["name"]);
    if (!id || !name) {
      return [];
    }
    return [
      {
        id,
        name,
        active: revision(row["active"]),
        revisions: list(row["revisions"]).flatMap((item) => {
          const decoded = revision(item);
          return decoded ? [decoded] : [];
        }),
      },
    ];
  });
  const rawReports = body["install_reports"];
  const reports = Array.isArray(rawReports)
    ? rawReports.flatMap((item) => {
        const decoded = report(item);
        return decoded ? [decoded] : [];
      })
    : null;
  return { packs, reports };
}

/** The revisions a rollback could activate: retired, with their content still held. */
export function retained(pack: PackView): readonly RevisionView[] {
  return pack.revisions.filter((candidate) => candidate.status === "deprecated" && candidate.hasContent);
}

export type Rollback = { readonly ok: true; readonly params: { readonly pack: string } } | { readonly ok: false; readonly code: string; readonly reason: string };

/** `RollbackPack`: offered only when a retained revision still has its content. */
export function rollback(pack: PackView): Rollback {
  if (!pack.active) {
    return { ok: false, code: "CONFLICT_STATE", reason: `${pack.name} has no active revision` };
  }
  if (retained(pack).length === 0) {
    return { ok: false, code: "CONFLICT_STATE", reason: `${pack.name} has no retained previous revision to roll back to` };
  }
  return { ok: true, params: { pack: pack.name } };
}

/** The first twelve hex digits of a `sha256:` revision, for display. */
export function shortRevision(value: string | null): string {
  return value ? value.replace(/^sha256:/, "").slice(0, 12) : "—";
}

/** One line a person can read: what happened and what is still active. */
export function summarizeReport(view: InstallReportView): string {
  switch (view.state) {
    case "running":
      return `installing — at ${view.phase}`;
    case "succeeded":
      return `activated ${shortRevision(view.revision)}`;
    case "failed":
      return view.outcome === "retained_previous"
        ? `failed at ${view.phase} — the previous revision ${shortRevision(view.previousRevision)} is still active`
        : `failed at ${view.phase} — nothing was activated`;
  }
}
