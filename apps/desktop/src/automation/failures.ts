/**
 * What a refused routine command means to the page (ADP-007, DOMAIN.md §15 "Automation errors").
 *
 * The bridge settles every routine command to `{ok, result | error}`; a refusal is data. This
 * module decodes it, recognises a revision conflict (`CONFLICT_REVISION` with the revision the
 * routine is at now), maps the refusals that belong to one field onto that field, and keeps an
 * edit session that survives a conflict so the person's draft stays visible for re-applying.
 */

import { draftsDiffer, type RoutineDraft, type RoutineView } from "./routines.js";

export interface Failure {
  readonly code: string;
  readonly message: string;
  readonly details: Record<string, unknown> | null;
}

export type Settled = { readonly ok: true; readonly result: unknown } | { readonly ok: false; readonly failure: Failure };

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

/** Decode the bridge's settled payload. Anything that is not `{ok: true}` is a refusal. */
export function decodeSettled(raw: unknown): Settled {
  const source = record(raw);
  if (source["ok"] === true) {
    return { ok: true, result: source["result"] };
  }
  const error = record(source["error"]);
  const details = error["details"];
  return {
    ok: false,
    failure: {
      code: typeof error["code"] === "string" ? error["code"] : "INTERNAL",
      message: typeof error["message"] === "string" ? error["message"] : "",
      details: details && typeof details === "object" && !Array.isArray(details) ? (details as Record<string, unknown>) : null,
    },
  };
}

export interface Conflict {
  readonly routineId: string;
  readonly expectedRevision: number | null;
  readonly currentRevision: number | null;
}

function integer(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) ? value : null;
}

/** The conflict a `CONFLICT_REVISION` refusal names, or `null` for any other failure. */
export function conflictOf(failure: Failure): Conflict | null {
  if (failure.code !== "CONFLICT_REVISION") {
    return null;
  }
  const details = failure.details ?? {};
  const routineId = details["routine_id"];
  return {
    routineId: typeof routineId === "string" ? routineId : "",
    expectedRevision: integer(details["expected_revision"]),
    currentRevision: integer(details["current_revision"]),
  };
}

function scalarText(value: unknown, fallback: string): string {
  return typeof value === "string" || typeof value === "number" ? String(value) : fallback;
}

/** Where a refusal is shown: next to the field it is about, or with the form as a whole. */
export type FieldErrors = Readonly<Partial<Record<"timezone" | "trigger" | "filters" | "budget" | "form", string>>>;

/** Map a refusal onto the field it is about. Codes of other kinds map to nothing. */
export function fieldErrorsOf(failure: Failure): FieldErrors {
  const details = failure.details ?? {};
  switch (failure.code) {
    case "AUTOMATION_TIMEZONE_UNKNOWN": {
      const zone = details["timezone"];
      const named = typeof zone === "string" && zone !== "" ? ` (${zone})` : "";
      return { timezone: `${failure.message || "This is not a time zone the server knows."}${named}` };
    }
    case "AUTOMATION_LIMIT_EXCEEDED": {
      const limit = details["limit"];
      const text = `${failure.message} (${scalarText(limit, "limit")}: ${scalarText(details["value"], "?")}, allowed ${scalarText(details["allowed"], "?")})`;
      if (limit === "min_interval_seconds") {
        return { trigger: text };
      }
      if (limit === "max_agent_calls_ceiling") {
        return { budget: text };
      }
      return { form: text };
    }
    case "VALIDATION_SCHEMA":
    case "VALIDATION_BOUNDS": {
      const message = failure.message;
      if (/\bfilter/i.test(message)) {
        return { filters: message };
      }
      if (/\btrigger|\bcron|\binterval|\bevent\b/i.test(message)) {
        return { trigger: message };
      }
      return { form: message };
    }
    default:
      return {};
  }
}

/** A refusal as one sentence for a notice. */
export function failureText(failure: Failure, refused: string): string {
  return `${refused} ${failure.code}: ${failure.message}`;
}

// ---- an edit that survives a conflict ----

/** An edit in progress: the revision it started from, and the draft the person is changing. */
export interface EditSession {
  readonly routineId: string;
  /** The revision the draft was started from, sent as `expected_revision`. */
  readonly baseRevision: number;
  readonly draft: RoutineDraft;
  /** Set when the routine moved on; the draft is kept and still shown. */
  readonly conflict: { readonly currentRevision: number | null; readonly changedFields: readonly string[]; readonly reloaded: boolean } | null;
}

export function startEdit(routine: RoutineView, draft: RoutineDraft): EditSession {
  return { routineId: routine.id, baseRevision: routine.revision, draft, conflict: null };
}

/** The person changed the draft: the draft moves, the base and any conflict stay. */
export function editDraft(session: EditSession, draft: RoutineDraft): EditSession {
  return { ...session, draft };
}

/** The server refused the save with a conflict: keep the draft, remember the revision it is at now. */
export function markConflict(session: EditSession, conflict: Conflict): EditSession {
  return { ...session, conflict: { currentRevision: conflict.currentRevision, changedFields: [], reloaded: false } };
}

/**
 * "Reload and review": the routine as it is now has been fetched. The draft stays as the person
 * wrote it; the base moves to the current revision so a second save is a deliberate re-apply, and
 * the fields where the draft and the current definition differ are named. `latest` is `null` when
 * the routine is gone.
 */
export function reviewAfterReload(session: EditSession, latest: RoutineView | null, latestDraft: RoutineDraft | null): EditSession {
  if (!latest || !latestDraft) {
    return { ...session, conflict: { currentRevision: null, changedFields: [], reloaded: true } };
  }
  return {
    ...session,
    baseRevision: latest.revision,
    conflict: { currentRevision: latest.revision, changedFields: draftsDiffer(session.draft, latestDraft), reloaded: true },
  };
}

/** Whether a save may be sent: not while a conflict is shown and not yet reviewed, and never for a routine that is gone. */
export function canSave(session: EditSession): boolean {
  return session.conflict === null || (session.conflict.reloaded && session.conflict.currentRevision !== null);
}
