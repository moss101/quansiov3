/**
 * A routine's workflow script as the page shows it (ADP-007, DOMAIN.md §13.8 E-I).
 *
 * Two projections reach the page: the routine's `script` outline (its steps and budget as written)
 * and `script_runs[]` (what each run did, derived from its journal: status, calls used, the open
 * gate and every step's status, with item counts for a `parallel` step). This module decodes both
 * and puts them side by side as a list of steps with what each is waiting on, plus the call budget
 * and the gate. It decides nothing: spending above the threshold or past the cap is a person's
 * through the approval path (§13.8 G), and the page only points there.
 */

export type StepKind = "agent" | "parallel" | "reduce" | "log";
export type StepStatus = "pending" | "running" | "done" | "failed";
export type RunStatus = "running" | "waiting" | "succeeded" | "failed" | "cancelled" | "finished";

export interface OutlineStep {
  readonly id: string;
  readonly kind: StepKind;
  /** The earlier step a `parallel` fans out over, or a `reduce` collects. */
  readonly after: string | null;
  readonly maxItems: number | null;
}

export interface ScriptOutline {
  readonly maxAgentCalls: number;
  readonly estimatedAgentCalls: number;
  readonly steps: readonly OutlineStep[];
}

export interface ItemCounts {
  readonly total: number | null;
  readonly finished: number;
  readonly failed: number;
}

export interface RunStep {
  readonly id: string;
  readonly kind: StepKind;
  readonly status: StepStatus;
  readonly items: ItemCounts | null;
}

export type GateKind = "fan_out" | "extend_budget";

export interface OpenGate {
  readonly kind: GateKind;
  readonly calls: number | null;
  /** The approval request the person decides on, in the approvals surface. */
  readonly approvalId: string | null;
}

export interface ScriptRun {
  readonly fireId: string;
  readonly runId: string;
  readonly objectiveId: string | null;
  readonly status: RunStatus;
  readonly callsUsed: number;
  readonly maxAgentCalls: number;
  readonly approvedCalls: number;
  readonly estimatedAgentCalls: number;
  readonly openGate: OpenGate | null;
  /** `{code, message}` of the failure that ended the run, when it ended failed. */
  readonly error: string | null;
  readonly steps: readonly RunStep[];
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function num(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function kindOf(value: unknown): StepKind {
  return value === "parallel" || value === "reduce" || value === "log" ? value : "agent";
}

/** Decode the routine's `script` outline; `null` for a routine without a script. */
export function decodeOutline(raw: unknown): ScriptOutline | null {
  const source = record(raw);
  if (!Array.isArray(source["steps"])) {
    return null;
  }
  return {
    maxAgentCalls: num(source["max_agent_calls"]) ?? 0,
    estimatedAgentCalls: num(source["estimated_agent_calls"]) ?? 0,
    steps: source["steps"].map((entry): OutlineStep => {
      const step = record(entry);
      const kind = kindOf(step["kind"]);
      return {
        id: str(step["id"]) ?? "",
        kind,
        after: kind === "parallel" ? str(step["over"]) : kind === "reduce" ? str(step["of"]) : null,
        maxItems: num(step["max_items"]),
      };
    }),
  };
}

function statusOf(value: unknown): StepStatus {
  return value === "running" || value === "done" || value === "failed" ? value : "pending";
}

function runStatusOf(value: unknown): RunStatus {
  switch (value) {
    case "waiting":
    case "succeeded":
    case "failed":
    case "cancelled":
    case "finished":
      return value;
    default:
      return "running";
  }
}

function errorText(value: unknown): string | null {
  const error = record(value);
  const message = str(error["message"]);
  const code = str(error["code"]);
  if (code === null && message === null) {
    return null;
  }
  return [code, message].filter((part): part is string => part !== null).join(": ");
}

/** Decode `script_runs[]`, newest first as the server sent them. */
export function decodeScriptRuns(raw: unknown): ScriptRun[] {
  return (Array.isArray(raw) ? raw : []).map((entry): ScriptRun => {
    const source = record(entry);
    const gate = record(source["open_gate"]);
    const gateKind = gate["gate"];
    return {
      fireId: str(source["fire_id"]) ?? "",
      runId: str(source["run_id"]) ?? "",
      objectiveId: str(source["objective_id"]),
      status: runStatusOf(source["status"]),
      callsUsed: num(source["calls_used"]) ?? 0,
      maxAgentCalls: num(source["max_agent_calls"]) ?? 0,
      approvedCalls: num(source["approved_calls"]) ?? 0,
      estimatedAgentCalls: num(source["estimated_agent_calls"]) ?? 0,
      openGate: gateKind === "fan_out" || gateKind === "extend_budget" ? { kind: gateKind, calls: num(gate["calls"]), approvalId: str(gate["approval_id"]) } : null,
      error: errorText(source["error"]),
      steps: (Array.isArray(source["steps"]) ? source["steps"] : []).map((stepRaw): RunStep => {
        const step = record(stepRaw);
        const items = record(step["items"]);
        const hasItems = Object.keys(items).length > 0;
        return {
          id: str(step["id"]) ?? "",
          kind: kindOf(step["kind"]),
          status: statusOf(step["status"]),
          items: hasItems ? { total: num(items["total"]), finished: num(items["finished"]) ?? 0, failed: num(items["failed"]) ?? 0 } : null,
        };
      }),
    };
  });
}

/** One row of the step list: the step as written and what the latest run did with it. */
export interface StepRow {
  readonly id: string;
  readonly kind: StepKind;
  readonly after: string | null;
  readonly maxItems: number | null;
  readonly status: StepStatus;
  readonly items: ItemCounts | null;
}

/** The outline's steps in order, each with the status the run reports (pending when no run has reached it). */
export function stepRows(outline: ScriptOutline, run: ScriptRun | null): StepRow[] {
  return outline.steps.map((step) => {
    const reported = run?.steps.find((candidate) => candidate.id === step.id);
    return {
      id: step.id,
      kind: step.kind,
      after: step.after,
      maxItems: step.maxItems,
      status: reported?.status ?? "pending",
      items: reported?.items ?? null,
    };
  });
}

/** The newest run, when there is one. */
export function latestRun(runs: readonly ScriptRun[]): ScriptRun | null {
  return runs[0] ?? null;
}

export type BudgetLevel = "ok" | "near" | "paused" | "exhausted";

export interface Budget {
  readonly used: number;
  readonly max: number;
  readonly estimated: number;
  /** `used / max`, between 0 and 1; 0 when there is no cap. */
  readonly fraction: number;
  readonly level: BudgetLevel;
}

/**
 * The call budget of a run, or of the outline when no run has started. A run held at an
 * `extend_budget` gate is `paused`: it stopped instead of failing and a person decides whether to
 * spend more. `near` is at or above 80% of the cap.
 */
export function budgetOf(outline: ScriptOutline | null, run: ScriptRun | null): Budget | null {
  if (run) {
    const max = run.maxAgentCalls > 0 ? run.maxAgentCalls : (outline?.maxAgentCalls ?? 0);
    const fraction = max > 0 ? Math.min(1, run.callsUsed / max) : 0;
    const level: BudgetLevel = run.openGate?.kind === "extend_budget" ? "paused" : max > 0 && run.callsUsed >= max ? "exhausted" : fraction >= 0.8 ? "near" : "ok";
    return { used: run.callsUsed, max, estimated: run.estimatedAgentCalls > 0 ? run.estimatedAgentCalls : (outline?.estimatedAgentCalls ?? 0), fraction, level };
  }
  if (outline) {
    return { used: 0, max: outline.maxAgentCalls, estimated: outline.estimatedAgentCalls, fraction: 0, level: "ok" };
  }
  return null;
}

/** What needs a person: a fan-out above the threshold, or a run that stopped at its call cap. */
export type Attention =
  | { readonly kind: "none" }
  | { readonly kind: "paused_budget"; readonly calls: number | null; readonly approvalId: string | null }
  | { readonly kind: "needs_approval"; readonly calls: number | null; readonly approvalId: string | null };

export function attentionOf(run: ScriptRun | null): Attention {
  const gate = run?.openGate;
  if (!gate) {
    return { kind: "none" };
  }
  return gate.kind === "extend_budget" ? { kind: "paused_budget", calls: gate.calls, approvalId: gate.approvalId } : { kind: "needs_approval", calls: gate.calls, approvalId: gate.approvalId };
}

/** The progress of a `parallel` step in words-free numbers: finished of total, and how many failed. */
export function itemProgress(items: ItemCounts | null): { readonly finished: number; readonly total: number | null; readonly failed: number; readonly fraction: number | null } | null {
  if (!items) {
    return null;
  }
  return {
    finished: items.finished,
    total: items.total,
    failed: items.failed,
    fraction: items.total !== null && items.total > 0 ? Math.min(1, items.finished / items.total) : null,
  };
}
