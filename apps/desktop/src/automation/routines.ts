/**
 * Routines (APP-010, DOMAIN.md §13.1): the client half.
 *
 * The server owns routines and when they fire; this module decodes the projection
 * (`GET /v1/routines`), checks a draft before it is sent (the server checks again), builds
 * the commands, and summarizes each routine's fires — which is how a person sees that a
 * restart did not fire a window twice.
 */

export type TriggerKind = "cron" | "interval" | "event";
export type AbsencePolicy = "skip" | "queue" | "catch_up_once";
export type RoutineStatus = "active" | "paused" | "deleted";

export interface RoutineTrigger {
  readonly kind: TriggerKind;
  readonly spec: string;
  readonly timezone?: string;
}

export interface FireView {
  readonly id: string;
  readonly fireKey: string;
  readonly source: "schedule" | "event" | "manual";
  readonly status: "pending" | "running" | "fired" | "failed";
  readonly attempts: number;
  readonly windowAt: string | null;
  readonly runId: string | null;
  readonly threadId: string | null;
  readonly lastError: string | null;
}

export interface RoutineView {
  readonly id: string;
  readonly title: string;
  readonly prompt: string;
  readonly trigger: RoutineTrigger;
  readonly teammateId: string | null;
  readonly absencePolicy: AbsencePolicy;
  readonly status: RoutineStatus;
  readonly ownerUserId: string;
  readonly nextDueAt: string | null;
  readonly lastFiredAt: string | null;
  readonly fires: readonly FireView[];
}

export interface RoutineDraft {
  readonly title: string;
  readonly prompt: string;
  readonly trigger: RoutineTrigger;
  readonly teammateId?: string | null;
  readonly absencePolicy?: AbsencePolicy;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function str(source: Record<string, unknown>, key: string): string | null {
  const value = source[key];
  return typeof value === "string" ? value : null;
}

function triggerOf(value: unknown): RoutineTrigger {
  const source = record(value);
  const kind = source["kind"];
  const timezone = str(source, "timezone");
  return {
    kind: kind === "cron" || kind === "event" ? kind : "interval",
    spec: str(source, "spec") ?? "",
    ...(timezone ? { timezone } : {}),
  };
}

function fireOf(value: unknown): FireView {
  const source = record(value);
  const status = source["status"];
  const kind = source["source"];
  return {
    id: str(source, "id") ?? "",
    fireKey: str(source, "fire_key") ?? "",
    source: kind === "event" || kind === "manual" ? kind : "schedule",
    status: status === "running" || status === "fired" || status === "failed" ? status : "pending",
    attempts: typeof source["attempts"] === "number" ? source["attempts"] : 0,
    windowAt: str(source, "window_at"),
    runId: str(source, "run_id"),
    threadId: str(source, "thread_id"),
    lastError: str(source, "last_error"),
  };
}

/** Decode `GET /v1/routines`. */
export function decodeRoutines(raw: unknown): RoutineView[] {
  const items = record(raw)["items"];
  return (Array.isArray(items) ? items : []).map((item) => {
    const source = record(item);
    const template = record(source["objective_template"]);
    const policy = source["absence_policy"];
    const status = source["status"];
    return {
      id: str(source, "id") ?? "",
      title: str(template, "title") ?? "",
      prompt: str(template, "prompt") ?? "",
      trigger: triggerOf(source["trigger"]),
      teammateId: str(source, "teammate_id"),
      absencePolicy: policy === "queue" || policy === "catch_up_once" ? policy : "skip",
      status: status === "paused" || status === "deleted" ? status : "active",
      ownerUserId: str(source, "owner_user_id") ?? "",
      nextDueAt: str(source, "next_due_at"),
      lastFiredAt: str(source, "last_fired_at"),
      fires: (Array.isArray(source["fires"]) ? source["fires"] : []).map(fireOf),
    };
  });
}

const INTERVAL = /^\d+[smhd]?$/;
const EVENT = /^[a-z_]+\.(\*|[a-z0-9_]+)$/;

/** What is wrong with a draft, before the server is asked (it checks again); `null` when nothing. */
export function draftProblem(draft: RoutineDraft): string | null {
  if (!draft.title.trim() || draft.title.trim().length > 200) {
    return "a routine needs a title of at most 200 characters";
  }
  if (!draft.prompt.trim()) {
    return "a routine needs a prompt";
  }
  const spec = draft.trigger.spec.trim();
  switch (draft.trigger.kind) {
    case "interval":
      return INTERVAL.test(spec) ? null : "an interval is a number with s, m, h or d (e.g. 30m)";
    case "cron":
      return spec.split(/\s+/).length === 5 ? null : "a cron expression has five fields";
    case "event":
      if (!EVENT.test(spec)) {
        return "an event trigger is family.name or family.*";
      }
      return spec.startsWith("routine.") || spec.startsWith("notification.") ? "a routine cannot listen to routine or notification events" : null;
  }
}

export function createRoutineParams(workspaceId: string, draft: RoutineDraft): Record<string, unknown> {
  const problem = draftProblem(draft);
  if (problem) {
    throw new Error(problem);
  }
  const params: Record<string, unknown> = {
    workspace_id: workspaceId,
    title: draft.title.trim(),
    prompt: draft.prompt.trim(),
    trigger: { kind: draft.trigger.kind, spec: draft.trigger.spec.trim(), timezone: draft.trigger.timezone ?? "UTC" },
    absence_policy: draft.absencePolicy ?? "skip",
  };
  if (draft.teammateId) {
    params["teammate_id"] = draft.teammateId;
  }
  return params;
}

export type RoutineControl = "PauseRoutine" | "ResumeRoutine" | "DeleteRoutine" | "TriggerRoutineNow";

/** The controls a routine's state allows. */
export function routineControls(routine: RoutineView): readonly RoutineControl[] {
  switch (routine.status) {
    case "active":
      return ["TriggerRoutineNow", "PauseRoutine", "DeleteRoutine"];
    case "paused":
      return ["ResumeRoutine", "DeleteRoutine"];
    case "deleted":
      return [];
  }
}

export interface FireSummary {
  readonly fired: number;
  readonly pending: number;
  readonly failed: number;
  /** Fires that share a run: never expected, shown if the server ever did it. */
  readonly duplicateRuns: number;
  /** Schedule windows fired more than once: never expected either. */
  readonly duplicateWindows: number;
}

/** Count a routine's fires, and any window or run that appears twice. */
export function summarizeFires(fires: readonly FireView[]): FireSummary {
  const runs = fires.map((fire) => fire.runId).filter((run): run is string => run !== null);
  const windows = fires.filter((fire) => fire.source === "schedule").map((fire) => fire.windowAt).filter((at): at is string => at !== null);
  return {
    fired: fires.filter((fire) => fire.status === "fired").length,
    pending: fires.filter((fire) => fire.status === "pending" || fire.status === "running").length,
    failed: fires.filter((fire) => fire.status === "failed").length,
    duplicateRuns: runs.length - new Set(runs).size,
    duplicateWindows: windows.length - new Set(windows).size,
  };
}
