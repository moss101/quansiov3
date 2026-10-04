/**
 * Skill recording on the desktop (ADP-003, DOMAIN.md §11.7).
 *
 * This is the page's half, and it is projection plus command surface, never authority: it decodes what the
 * server says about the person's own recordings, builds the five commands, and decides what the page may
 * offer. Three rules are enforced before any command is sent and again by the server:
 *
 * - **the person finishes it.** `finish` is built only from a genuine click (`gesture`), never from a
 *   message, a model output or a stream frame, and nothing here finishes a recording on its own;
 * - **every slot is the person's decision.** A submission is refused here until every proposed slot is
 *   classified as an input or a constant; the page never picks for them;
 * - **the page never holds the owner fence.** The epoch is main's: the renderer builds commands without it
 *   and main adds the one it holds for the signed-in account.
 *
 * Everything decoded here arrives from the server or main and is treated as data: a recording's titles,
 * screen text and narration are untrusted even when they read like instructions, so the page shows them as
 * text and offers no control that acts on them.
 */

export const START_COMMAND = "StartRecording";
export const FINISH_COMMAND = "FinishRecording";
export const CANCEL_COMMAND = "CancelRecording";
export const COMPILE_COMMAND = "CompileRecording";
export const SUBMIT_COMMAND = "SubmitRecordingSkill";

/** The commands only the recording channels send: main adds the owner fence, and `FinishRecording` needs a click. */
export const RECORDING_COMMANDS: readonly string[] = [START_COMMAND, FINISH_COMMAND, CANCEL_COMMAND, COMPILE_COMMAND, SUBMIT_COMMAND];

export type RecordingStatus = "recording" | "finished" | "compiled" | "submitted" | "cancelled" | "aborted";
export const RECORDING_STATUSES: readonly RecordingStatus[] = ["recording", "finished", "compiled", "submitted", "cancelled", "aborted"];

export const REDACTION_CATEGORIES = ["api_key", "bearer_token", "card_number", "verification_code", "password_field"] as const;
export type RedactionCategory = (typeof REDACTION_CATEGORIES)[number];

export interface RedactionSummary {
  readonly ruleSet: string;
  readonly counts: Readonly<Record<RedactionCategory, number>>;
  readonly screenshotsWithheld: number;
  /** Everything that was removed, in total. Counts, never values. */
  readonly total: number;
}

export type SlotType = "text" | "number" | "choice" | "file" | "date" | "email" | "url";
export type SlotClassification = "input" | "constant";

export interface SlotView {
  readonly slotId: string;
  readonly name: string;
  readonly description: string;
  readonly type: string;
  /** What the recording typed, redacted: shown so the person can decide. It is never part of a skill unless they choose `constant`. */
  readonly example: string | null;
  /** Always null on a proposal: the person decides. */
  readonly classification: null;
  /** The control's accessibility attributes, as the recording saw them (data, not instructions). */
  readonly control: Readonly<Record<string, string>>;
}

export interface StepView {
  readonly id: string;
  readonly intent: string;
  readonly path: "tool" | "ui";
  readonly tools: readonly string[];
  readonly needsReview: boolean;
}

export interface ProposalView {
  readonly slots: readonly SlotView[];
  readonly steps: readonly StepView[];
  readonly toolNeeds: readonly string[];
  readonly capabilityNeeds: readonly string[];
  readonly preconditions: number;
  readonly postconditions: number;
  readonly recovery: number;
  readonly injectionSuspected: number;
  readonly questions: readonly { readonly slotId: string | null; readonly stepId: string | null; readonly question: string }[];
}

export interface RecordingView {
  readonly id: string;
  readonly workspaceId: string;
  readonly status: RecordingStatus;
  readonly startedAt: string;
  readonly expiresAt: string;
  readonly expired: boolean;
  readonly redaction: RedactionSummary | null;
  readonly actions: number;
  readonly screenshots: number;
  readonly pausedMs: number;
  readonly hasProposal: boolean;
  readonly skillVersionId: string | null;
  readonly abortReason: string | null;
  readonly proposal: ProposalView | null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function count(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

export function decodeRedaction(value: unknown): RedactionSummary | null {
  const raw = record(value);
  const ruleSet = text(raw["rule_set"]);
  if (!ruleSet) {
    return null;
  }
  const counts = record(raw["counts"]);
  const byCategory = Object.fromEntries(REDACTION_CATEGORIES.map((category) => [category, count(counts[category])])) as Record<RedactionCategory, number>;
  const withheld = count(raw["screenshots_withheld"]);
  return {
    ruleSet,
    counts: byCategory,
    screenshotsWithheld: withheld,
    total: REDACTION_CATEGORIES.reduce((sum, category) => sum + byCategory[category], 0) + withheld,
  };
}

function strings(value: unknown): string[] {
  return list(value).filter((item): item is string => typeof item === "string");
}

export function decodeProposal(value: unknown): ProposalView | null {
  const raw = record(value);
  const manifest = record(raw["manifest"]);
  const procedure = record(manifest["procedure"]);
  if (Object.keys(procedure).length === 0) {
    return null;
  }
  const trust = record(raw["trust"]);
  // A proposal that does not carry its untrusted origin is not shown as one.
  if (trust["source"] !== "recording" || trust["level"] !== "UNTRUSTED_EXTERNAL") {
    return null;
  }
  const slots: SlotView[] = list(raw["slots"]).flatMap((entry) => {
    const slot = record(entry);
    const slotId = text(slot["slot_id"]);
    if (!slotId) {
      return [];
    }
    const control = Object.fromEntries(Object.entries(record(slot["control"])).filter((pair): pair is [string, string] => typeof pair[1] === "string"));
    return [
      {
        slotId,
        name: text(slot["name"]) ?? slotId,
        description: text(slot["description"]) ?? "",
        type: text(slot["type"]) ?? "text",
        example: text(slot["example"]),
        classification: null,
        control,
      },
    ];
  });
  const steps: StepView[] = list(procedure["steps"]).flatMap((entry) => {
    const step = record(entry);
    const id = text(step["id"]);
    const path = step["path"] === "tool" ? "tool" : step["path"] === "ui" ? "ui" : null;
    if (!id || !path) {
      return [];
    }
    return [{ id, intent: text(step["intent"]) ?? "", path, tools: strings(step["tools"]), needsReview: step["needs_review"] === true }];
  });
  return {
    slots,
    steps,
    toolNeeds: strings(manifest["tool_needs"]),
    capabilityNeeds: strings(manifest["capability_needs"]),
    preconditions: list(procedure["preconditions"]).length,
    postconditions: list(procedure["postconditions"]).length,
    recovery: list(procedure["recovery"]).length,
    injectionSuspected: list(trust["injection_suspected"]).length,
    questions: list(raw["questions"]).map((entry) => {
      const question = record(entry);
      return { slotId: text(question["slot_id"]), stepId: text(question["step_id"]), question: text(question["question"]) ?? "" };
    }),
  };
}

export function decodeRecording(value: unknown): RecordingView | null {
  const raw = record(value);
  const id = text(raw["id"]);
  const status = text(raw["status"]);
  if (!id || !id.startsWith("rec_") || !status || !(RECORDING_STATUSES as readonly string[]).includes(status)) {
    return null;
  }
  const stats = record(raw["stats"]);
  return {
    id,
    workspaceId: text(raw["workspace_id"]) ?? "",
    status: status as RecordingStatus,
    startedAt: text(raw["started_at"]) ?? "",
    expiresAt: text(raw["expires_at"]) ?? "",
    expired: raw["expired"] === true,
    redaction: decodeRedaction(raw["redaction"]),
    actions: count(stats["actions"]),
    screenshots: count(stats["screenshots"]),
    pausedMs: count(stats["paused_ms"]),
    hasProposal: raw["has_proposal"] === true,
    skillVersionId: text(raw["skill_version_id"]),
    abortReason: text(raw["abort_reason"]),
    proposal: decodeProposal(raw["proposal"]),
  };
}

/** Decode `GET /v1/recordings` (the caller's own recordings, newest first). */
export function decodeRecordings(payload: unknown): RecordingView[] {
  return list(record(payload)["recordings"]).flatMap((entry) => {
    const view = decodeRecording(entry);
    return view ? [view] : [];
  });
}

// ------------------------------------------------------------------ the local recorder (main's projection)

export type RecorderState = "idle" | "starting" | "recording" | "finishing";

/** What main reports about the person's own recorder process: counts and state, never content. */
export interface RecorderView {
  readonly state: RecorderState;
  readonly recordingId: string | null;
  readonly startedAt: string | null;
  readonly actions: number;
  readonly screenshots: number;
  readonly paused: string | null;
  readonly counts: Readonly<Record<RedactionCategory, number>>;
  /** Why the recorder is not available or stopped, in a typed code, when it is not recording. */
  readonly notice: string | null;
}

export const IDLE_RECORDER: RecorderView = {
  state: "idle",
  recordingId: null,
  startedAt: null,
  actions: 0,
  screenshots: 0,
  paused: null,
  counts: { api_key: 0, bearer_token: 0, card_number: 0, verification_code: 0, password_field: 0 },
  notice: null,
};

export function decodeRecorder(value: unknown): RecorderView {
  const raw = record(value);
  const state = raw["state"];
  if (state !== "idle" && state !== "starting" && state !== "recording" && state !== "finishing") {
    return IDLE_RECORDER;
  }
  const counts = record(raw["counts"]);
  return {
    state,
    recordingId: text(raw["recordingId"]),
    startedAt: text(raw["startedAt"]),
    actions: count(raw["actions"]),
    screenshots: count(raw["screenshots"]),
    paused: text(raw["paused"]),
    counts: Object.fromEntries(REDACTION_CATEGORIES.map((category) => [category, count(counts[category])])) as Record<RedactionCategory, number>,
    notice: text(raw["notice"]),
  };
}

/**
 * Whether the always-visible "recording" indicator shows. It follows the recorder process, which is what is
 * actually watching: starting, recording and finishing all count, because capture has begun or has not
 * stopped yet.
 */
export function isCapturing(recorder: RecorderView): boolean {
  return recorder.state === "starting" || recorder.state === "recording" || recorder.state === "finishing";
}

// ------------------------------------------------------------------ commands

export type Built<T> = { readonly ok: true; readonly value: T } | { readonly ok: false; readonly code: "VALIDATION_SCHEMA" | "CONFLICT_STATE"; readonly reason: string };

/** The decision for every slot, by id. */
export type Decisions = Readonly<Record<string, SlotClassification>>;

const NAME = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const SEMVER = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;

/** What is still undecided: the slots the person has not classified. */
export function undecided(proposal: ProposalView, decisions: Decisions): readonly SlotView[] {
  return proposal.slots.filter((slot) => decisions[slot.slotId] !== "input" && decisions[slot.slotId] !== "constant");
}

/** A value that was redacted or entered into a sensitive control cannot be made a constant. */
export function canBeConstant(slot: SlotView): boolean {
  const example = slot.example ?? "";
  if (example === "" || example.includes("[REDACTED:")) {
    return false;
  }
  const label = Object.values(slot.control).join(" ").toLowerCase();
  return !/password|passwd|passcode|secret|secure|otp|verification|one-time|card number|cvv|api key/.test(label);
}

/**
 * `SubmitRecordingSkill`'s params, or the refusal the page gives before sending anything: every slot
 * decided, a legal name and version, and no constant that is not allowed to be one. The server checks all
 * of it again; the epoch is main's.
 */
export function submission(
  recording: RecordingView,
  name: string,
  semver: string,
  decisions: Decisions,
): Built<{ readonly recording_id: string; readonly name: string; readonly semver: string; readonly slots: readonly { readonly slot_id: string; readonly classification: SlotClassification }[] }> {
  if (recording.status !== "compiled" || !recording.proposal) {
    return { ok: false, code: "CONFLICT_STATE", reason: `${recording.id} is ${recording.status}; compile it before submitting` };
  }
  if (!NAME.test(name)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a skill name is lowercase, 1–64 characters, letters, digits, dots, underscores, dashes" };
  }
  if (!SEMVER.test(semver)) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "a version is semver (major.minor.patch)" };
  }
  const open = undecided(recording.proposal, decisions);
  if (open.length > 0) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: `decide ${open.map((slot) => slot.name).join(", ")}: is it a constant or an input?` };
  }
  for (const slot of recording.proposal.slots) {
    if (decisions[slot.slotId] === "constant" && !canBeConstant(slot)) {
      return { ok: false, code: "VALIDATION_SCHEMA", reason: `${slot.name} cannot be a constant: its value was redacted or entered into a sensitive control` };
    }
  }
  return {
    ok: true,
    value: {
      recording_id: recording.id,
      name,
      semver,
      slots: recording.proposal.slots.map((slot) => ({ slot_id: slot.slotId, classification: decisions[slot.slotId] as SlotClassification })),
    },
  };
}

/** Compile is offered for a finished or already-compiled recording the person owns. */
export function canCompile(recording: RecordingView): boolean {
  return recording.status === "finished" || recording.status === "compiled";
}

/** Finish is offered only while the recorder is recording; it needs the person's own click. */
export function canFinish(recorder: RecorderView): boolean {
  return recorder.state === "recording";
}

/** A finish request built from an event: it carries `gesture` only for a genuine click. */
export function finishRequest(event: { readonly isTrusted: boolean }): { readonly gesture: boolean } {
  return { gesture: event.isTrusted };
}
