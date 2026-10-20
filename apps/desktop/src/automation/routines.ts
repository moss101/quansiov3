/**
 * Routines (APP-010, DOMAIN.md §13.1, ADP-007 §13.8): the client half.
 *
 * The server owns routines and when they fire; this module decodes the projection
 * (`GET /v1/routines`: the revision, an inbound hook's endpoint and delivery counts, a workflow
 * script and its runs), checks a draft before it is sent (the server checks again), builds the
 * commands (an update is a full replacement under `expected_revision`), and summarizes each
 * routine's fires — which is how a person sees that a restart did not fire a window twice.
 */

import { filterProblem, filtersFromWire, filtersToWire, wireFiltersOf, type FilterDraft, type HookConditionWire } from "./hooks.js";
import { decodeOutline, decodeScriptRuns, type ScriptOutline, type ScriptRun } from "./script-view.js";

export type TriggerKind = "cron" | "interval" | "event" | "webhook";
export type AbsencePolicy = "skip" | "queue" | "catch_up_once";
export type RoutineStatus = "active" | "paused" | "deleted";

export interface RoutineTrigger {
  readonly kind: TriggerKind;
  readonly spec: string;
  readonly timezone?: string;
  /** A webhook's typed filter groups as the server stores them: any group (OR) of every condition (AND). */
  readonly filters?: readonly (readonly HookConditionWire[])[];
}

export interface FireView {
  readonly id: string;
  readonly fireKey: string;
  readonly source: "schedule" | "event" | "manual" | "hook";
  readonly status: "pending" | "running" | "fired" | "failed";
  readonly attempts: number;
  readonly windowAt: string | null;
  readonly runId: string | null;
  readonly threadId: string | null;
  readonly lastError: string | null;
  /** The digest of an inbound hook's payload; the body itself is never in the projection. */
  readonly payloadDigest: string | null;
}

/** An inbound hook as the projection shows it: the address, that a secret is set, and the last day's deliveries. */
export interface HookView {
  readonly endpointId: string;
  /** `/v1/hooks/hk_…`. The signing secret is never part of the projection. */
  readonly path: string;
  readonly secretSet: boolean;
  readonly deliveries: { readonly accepted: number; readonly filtered: number } | null;
}

export type RoutineMode = "act" | "observe";

export interface RoutineView {
  readonly id: string;
  readonly title: string;
  readonly prompt: string;
  readonly trigger: RoutineTrigger;
  readonly teammateId: string | null;
  readonly absencePolicy: AbsencePolicy;
  /** `observe`: its runs can read and report and nothing else (DOMAIN.md §13.8). */
  readonly mode: RoutineMode;
  readonly status: RoutineStatus;
  readonly ownerUserId: string;
  readonly nextDueAt: string | null;
  readonly lastFiredAt: string | null;
  readonly fires: readonly FireView[];
  /** Every change to the definition or status increments it; an edit names the one it started from. */
  readonly revision: number;
  readonly hook: HookView | null;
  readonly script: ScriptOutline | null;
  readonly scriptRuns: readonly ScriptRun[];
  /** The stored script, kept so a full-replacement update sends it back (an absent script is removed). */
  readonly workflow: Record<string, unknown> | null;
  readonly budget: Record<string, unknown>;
  readonly notificationPrefs: Record<string, unknown>;
}

export interface RoutineDraft {
  readonly title: string;
  readonly prompt: string;
  readonly trigger: RoutineTrigger;
  readonly teammateId?: string | null;
  readonly absencePolicy?: AbsencePolicy;
  readonly mode?: RoutineMode;
  /** A webhook's filters while they are being edited (values are text until checked). */
  readonly filters?: FilterDraft;
  /** Carried through an edit unchanged: a full replacement returns an absent field to its default. */
  readonly workflow?: Record<string, unknown> | null;
  readonly budget?: Record<string, unknown>;
  readonly notificationPrefs?: Record<string, unknown>;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function plainRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

function str(source: Record<string, unknown>, key: string): string | null {
  const value = source[key];
  return typeof value === "string" ? value : null;
}

function triggerOf(value: unknown): RoutineTrigger {
  const source = record(value);
  const kind = source["kind"];
  const timezone = str(source, "timezone");
  const filters = wireFiltersOf(source["filters"]);
  return {
    kind: kind === "cron" || kind === "event" || kind === "webhook" ? kind : "interval",
    spec: str(source, "spec") ?? "",
    ...(timezone ? { timezone } : {}),
    ...(filters.length > 0 ? { filters } : {}),
  };
}

function fireOf(value: unknown): FireView {
  const source = record(value);
  const status = source["status"];
  const kind = source["source"];
  return {
    id: str(source, "id") ?? "",
    fireKey: str(source, "fire_key") ?? "",
    source: kind === "event" || kind === "manual" || kind === "hook" ? kind : "schedule",
    status: status === "running" || status === "fired" || status === "failed" ? status : "pending",
    attempts: typeof source["attempts"] === "number" ? source["attempts"] : 0,
    windowAt: str(source, "window_at"),
    runId: str(source, "run_id"),
    threadId: str(source, "thread_id"),
    lastError: str(source, "last_error"),
    payloadDigest: str(source, "payload_digest"),
  };
}

function hookOf(value: unknown): HookView | null {
  const source = record(value);
  const path = str(source, "path");
  const endpointId = str(source, "endpoint_id");
  if (path === null || endpointId === null) {
    return null;
  }
  const deliveries = record(source["deliveries"]);
  const accepted = deliveries["accepted"];
  const filtered = deliveries["filtered"];
  return {
    endpointId,
    path,
    secretSet: source["secret"] === "set",
    deliveries: typeof accepted === "number" && typeof filtered === "number" ? { accepted, filtered } : null,
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
      mode: template["mode"] === "observe" ? "observe" : "act",
      status: status === "paused" || status === "deleted" ? status : "active",
      ownerUserId: str(source, "owner_user_id") ?? "",
      nextDueAt: str(source, "next_due_at"),
      lastFiredAt: str(source, "last_fired_at"),
      fires: (Array.isArray(source["fires"]) ? source["fires"] : []).map(fireOf),
      revision: typeof source["revision"] === "number" ? source["revision"] : 1,
      hook: hookOf(source["hook"]),
      script: decodeOutline(source["script"]),
      scriptRuns: decodeScriptRuns(source["script_runs"]),
      workflow: plainRecord(template["workflow"]),
      budget: plainRecord(source["budget"]) ?? {},
      notificationPrefs: plainRecord(source["notification_prefs"]) ?? {},
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
    case "webhook":
      return filterProblem(draft.filters ?? []);
  }
}

/** The trigger as the server takes it: filters only for a webhook, no spec for a webhook, a zone for every kind. */
function triggerParams(draft: RoutineDraft, zone: "kept" | "cron-only" = "kept"): Record<string, unknown> {
  // A zone only means something to a cron trigger; a new routine of another kind is not given the viewer's.
  const timezone = zone === "cron-only" && draft.trigger.kind !== "cron" ? "UTC" : draft.trigger.timezone?.trim() || "UTC";
  const trigger: Record<string, unknown> = {
    kind: draft.trigger.kind,
    spec: draft.trigger.kind === "webhook" ? "" : draft.trigger.spec.trim(),
    timezone,
  };
  if (draft.trigger.kind === "webhook") {
    const filters = filtersToWire(draft.filters ?? []);
    if (filters.length > 0) {
      trigger["filters"] = filters;
    }
  }
  return trigger;
}

/**
 * A fresh `create_request_id` (1 to 64 characters of `[A-Za-z0-9._:-]`). The same id with the same
 * definition returns the routine already made instead of a second one, so a retry after a lost
 * answer never duplicates a trigger; a changed draft takes a fresh id.
 */
export function newCreateRequestId(): string {
  return `create-${globalThis.crypto.randomUUID()}`;
}

export function createRoutineParams(workspaceId: string, draft: RoutineDraft, createRequestId?: string): Record<string, unknown> {
  const problem = draftProblem(draft);
  if (problem) {
    throw new Error(problem);
  }
  const params: Record<string, unknown> = {
    workspace_id: workspaceId,
    title: draft.title.trim(),
    prompt: draft.prompt.trim(),
    trigger: triggerParams(draft, "cron-only"),
    absence_policy: draft.absencePolicy ?? "skip",
  };
  if (draft.mode === "observe") {
    params["mode"] = "observe";
  }
  if (draft.teammateId) {
    params["teammate_id"] = draft.teammateId;
  }
  if (createRequestId) {
    params["create_request_id"] = createRequestId;
  }
  return params;
}

/**
 * `UpdateRoutine` with `expected_revision`: a full replacement. Whatever the form does not edit
 * (the script, the budget, the notification preferences, the teammate) is sent back as it is,
 * because a field that is absent returns to its default and a script that is absent is removed.
 */
export function updateRoutineParams(routineId: string, expectedRevision: number, draft: RoutineDraft): Record<string, unknown> {
  const problem = draftProblem(draft);
  if (problem) {
    throw new Error(problem);
  }
  const params: Record<string, unknown> = {
    routine_id: routineId,
    expected_revision: expectedRevision,
    title: draft.title.trim(),
    prompt: draft.prompt.trim(),
    trigger: triggerParams(draft),
    absence_policy: draft.absencePolicy ?? "skip",
    // Said either way on a full replacement, so what the form shows is what is stored.
    mode: draft.mode ?? "act",
    budget: draft.budget ?? {},
    notification_prefs: draft.notificationPrefs ?? {},
  };
  if (draft.teammateId) {
    params["teammate_id"] = draft.teammateId;
  }
  if (draft.workflow) {
    params["workflow"] = draft.workflow;
  }
  return params;
}

/** A routine as an editable draft: what the form shows, plus what a full replacement must carry. */
export function draftOf(routine: RoutineView): RoutineDraft {
  return {
    title: routine.title,
    prompt: routine.prompt,
    trigger: { kind: routine.trigger.kind, spec: routine.trigger.spec, timezone: routine.trigger.timezone ?? "UTC" },
    teammateId: routine.teammateId,
    absencePolicy: routine.absencePolicy,
    mode: routine.mode,
    filters: filtersFromWire(routine.trigger.filters),
    workflow: routine.workflow,
    budget: routine.budget,
    notificationPrefs: routine.notificationPrefs,
  };
}

/** The names of the fields in which two drafts differ, in the order the form shows them. */
export function draftsDiffer(left: RoutineDraft, right: RoutineDraft): string[] {
  const differences: string[] = [];
  if (left.title.trim() !== right.title.trim()) {
    differences.push("title");
  }
  if (left.prompt.trim() !== right.prompt.trim()) {
    differences.push("prompt");
  }
  if (left.trigger.kind !== right.trigger.kind) {
    differences.push("trigger kind");
  }
  if (left.trigger.spec.trim() !== right.trigger.spec.trim()) {
    differences.push("schedule or event");
  }
  if ((left.trigger.timezone ?? "UTC") !== (right.trigger.timezone ?? "UTC")) {
    differences.push("time zone");
  }
  if (JSON.stringify(left.filters ?? []) !== JSON.stringify(right.filters ?? [])) {
    differences.push("filters");
  }
  if ((left.absencePolicy ?? "skip") !== (right.absencePolicy ?? "skip")) {
    differences.push("missed windows");
  }
  if ((left.mode ?? "act") !== (right.mode ?? "act")) {
    differences.push("mode");
  }
  if ((left.teammateId ?? null) !== (right.teammateId ?? null)) {
    differences.push("teammate");
  }
  if (JSON.stringify(left.workflow ?? null) !== JSON.stringify(right.workflow ?? null)) {
    differences.push("script");
  }
  return differences;
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

/**
 * The params of a control. Pause, resume and delete name the revision the list showed, so a
 * routine that changed since is refused (`CONFLICT_REVISION`) instead of changed under the person.
 */
export function controlParams(control: RoutineControl, routine: RoutineView): Record<string, unknown> {
  return control === "TriggerRoutineNow" ? { routine_id: routine.id } : { routine_id: routine.id, expected_revision: routine.revision };
}

/** `RotateRoutineHook`: a new signing secret, returned once; the old one stops working at once. */
export function rotateHookParams(routine: RoutineView): Record<string, unknown> {
  return { routine_id: routine.id, expected_revision: routine.revision };
}

/** `DryRunRoutine` for a saved routine. */
export function dryRunRoutineParams(routineId: string, sample: Record<string, unknown> | null): Record<string, unknown> {
  return sample ? { routine_id: routineId, sample_payload: sample } : { routine_id: routineId };
}

/**
 * `DryRunRoutine` for the form's draft. Unlike a create it does not refuse a draft the server
 * would refuse: the answer lists every problem at once. Filters that cannot be typed (a value
 * that is not the operator's kind) are the one thing it cannot send.
 */
export function dryRunDraftParams(workspaceId: string, draft: RoutineDraft, sample: Record<string, unknown> | null): Record<string, unknown> {
  const definition: Record<string, unknown> = {
    title: draft.title.trim(),
    prompt: draft.prompt.trim(),
    trigger: triggerParams(draft),
  };
  if (draft.workflow) {
    definition["workflow"] = draft.workflow;
  }
  const params: Record<string, unknown> = { workspace_id: workspaceId, definition };
  if (sample && draft.trigger.kind === "webhook") {
    params["sample_payload"] = sample;
  }
  return params;
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
