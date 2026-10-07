/**
 * The dry run (ADP-007, DOMAIN.md §13.8 C): what an entry would do, with nothing started.
 *
 * `DryRunRoutine` answers `{valid, problems[], trigger, next_windows[], filter, instruction, script}`.
 * It claims no fire, creates no run and makes no effect. This module decodes the answer, reads a
 * sample payload the person typed (checked here for shape and size; the server checks it again),
 * and says which refusals mean "slow down" rather than "this entry is wrong".
 */

import type { Failure } from "./failures.js";
import { offsetLabel } from "./timezones.js";

/** The bound on a hook body, the same one the server holds (`hooks.max_body_bytes`). */
export const MAX_SAMPLE_BYTES = 64 * 1024;

export interface DryRunProblem {
  readonly code: string;
  readonly message: string;
  /** Which part of the definition it is about: `trigger`, `prompt`, `objective_template`, `sample_payload`. */
  readonly at: string;
}

export interface InstructionBlock {
  readonly type: "text" | "untrusted_data";
  readonly text: string;
  /** For `untrusted_data`: where the text came from (`inbound_hook`). */
  readonly source: string | null;
}

export interface DryRunScript {
  readonly staticCalls: number;
  readonly worstCaseCalls: number | null;
  readonly estimatedAgentCalls: number;
  readonly maxAgentCalls: number;
  readonly approvalThresholdCalls: number | null;
  readonly needsApproval: boolean;
  readonly mayNeedApproval: boolean;
  readonly steps: readonly { readonly id: string; readonly kind: string }[];
}

export interface DryRunView {
  readonly valid: boolean;
  readonly problems: readonly DryRunProblem[];
  readonly triggerKind: string | null;
  /** The zone a cron trigger would use, with its offset now (`+02:00`). */
  readonly zone: { readonly name: string; readonly offset: string } | null;
  readonly nextWindows: readonly string[];
  readonly filter: { readonly evaluated: boolean; readonly matched: boolean | null; readonly matchedGroup: number | null };
  readonly instruction: { readonly blocks: readonly InstructionBlock[]; readonly bytes: number };
  readonly script: DryRunScript | null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function num(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** Decode a `DryRunRoutine` result. Missing parts decode to their empty form, never to a guess. */
export function decodeDryRun(raw: unknown): DryRunView {
  const source = record(raw);
  const trigger = record(source["trigger"]);
  const zone = record(trigger["timezone"]);
  const filter = record(source["filter"]);
  const instruction = record(source["instruction"]);
  const script = record(source["script"]);
  const hasScript = Object.keys(script).length > 0;
  const zoneName = zone["name"];
  const offset = num(zone["utc_offset_seconds"]);
  return {
    valid: source["valid"] === true,
    problems: (Array.isArray(source["problems"]) ? source["problems"] : []).map((entry) => {
      const problem = record(entry);
      return {
        code: typeof problem["code"] === "string" ? problem["code"] : "INTERNAL",
        message: typeof problem["message"] === "string" ? problem["message"] : "",
        at: typeof problem["at"] === "string" ? problem["at"] : "",
      };
    }),
    triggerKind: typeof trigger["kind"] === "string" ? trigger["kind"] : null,
    zone: typeof zoneName === "string" && offset !== null ? { name: zoneName, offset: offsetLabel(offset) } : null,
    nextWindows: (Array.isArray(source["next_windows"]) ? source["next_windows"] : []).filter((window): window is string => typeof window === "string"),
    filter: {
      evaluated: filter["evaluated"] === true,
      matched: typeof filter["matched"] === "boolean" ? filter["matched"] : null,
      matchedGroup: num(filter["matched_group"]),
    },
    instruction: {
      blocks: (Array.isArray(instruction["blocks"]) ? instruction["blocks"] : []).map((entry): InstructionBlock => {
        const block = record(entry);
        return {
          type: block["type"] === "untrusted_data" ? "untrusted_data" : "text",
          text: typeof block["text"] === "string" ? block["text"] : "",
          source: typeof block["source"] === "string" ? block["source"] : null,
        };
      }),
      bytes: num(instruction["bytes"]) ?? 0,
    },
    script: hasScript
      ? {
          staticCalls: num(script["static_calls"]) ?? 0,
          worstCaseCalls: num(script["worst_case_calls"]),
          estimatedAgentCalls: num(script["estimated_agent_calls"]) ?? 0,
          maxAgentCalls: num(script["max_agent_calls"]) ?? 0,
          approvalThresholdCalls: num(script["approval_threshold_calls"]),
          needsApproval: script["needs_approval"] === true,
          mayNeedApproval: script["may_need_approval"] === true,
          steps: (Array.isArray(script["steps"]) ? script["steps"] : []).map((entry) => {
            const step = record(entry);
            return { id: typeof step["id"] === "string" ? step["id"] : "", kind: typeof step["kind"] === "string" ? step["kind"] : "" };
          }),
        }
      : null,
  };
}

export type SampleResult = { readonly ok: true; readonly value: Record<string, unknown> | null } | { readonly ok: false; readonly message: string };

/** The sample payload a person typed: empty is none; otherwise one JSON object within the hook's size bound. */
export function parseSamplePayload(text: string): SampleResult {
  const trimmed = text.trim();
  if (trimmed === "") {
    return { ok: true, value: null };
  }
  if (new TextEncoder().encode(trimmed).length > MAX_SAMPLE_BYTES) {
    return { ok: false, message: "a sample payload is at most 64 KiB" };
  }
  try {
    const parsed: unknown = JSON.parse(trimmed);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { ok: true, value: parsed as Record<string, unknown> };
    }
  } catch {
    return { ok: false, message: "a sample payload is valid JSON" };
  }
  return { ok: false, message: "a sample payload is one JSON object" };
}

/** Which of the dry run's own states a refusal is: the tenant is making too many, or the request itself was refused. */
export function dryRunRefusal(failure: Failure): "rate_limited" | "refused" {
  return failure.code === "RATE_LIMITED" ? "rate_limited" : "refused";
}

/** The dry run's state on the page: one at a time, for one entry (a routine id, or the form). */
export type DryRunState =
  | { readonly target: string; readonly status: "running" }
  | { readonly target: string; readonly status: "done"; readonly view: DryRunView }
  | { readonly target: string; readonly status: "rate_limited" | "refused"; readonly failure: Failure };

/** The form's draft is dry-run under this target; a saved routine under its id. */
export const DRAFT_TARGET = "draft";

/** The dry run's verdict in a word, for the panel's heading. */
export function dryRunVerdict(view: DryRunView): "valid" | "invalid" {
  return view.valid ? "valid" : "invalid";
}

/** What the filter did with the sample payload. */
export function filterVerdict(view: DryRunView): "not_evaluated" | "passes" | "filtered" {
  if (!view.filter.evaluated) {
    return "not_evaluated";
  }
  return view.filter.matched === true ? "passes" : "filtered";
}
