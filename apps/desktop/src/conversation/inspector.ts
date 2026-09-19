/**
 * Per-turn inspector (APP-004): what `GET /v1/runs/{id}/timeline` says about each turn —
 * the model route that answered, the context sources with their trust labels, the tools
 * used with their effect status, the question asked, and the attachments the trigger
 * carried. This is a decoder over the server's projection; it invents nothing.
 */

export interface InspectedSource {
  readonly kind: string;
  readonly ref: string;
  readonly trustLevel: string;
  readonly tokens: number | null;
}

export interface InspectedTool {
  readonly toolName: string;
  readonly status: string;
  readonly effectId: string | null;
  readonly effectClass: string | null;
  readonly effectStatus: string | null;
}

export interface InspectedQuestion {
  readonly id: string;
  readonly prompt: string;
  readonly status: string;
  readonly answer: unknown;
}

export interface InspectedTurn {
  readonly turnId: string;
  readonly seq: number;
  readonly inputKind: string;
  readonly status: string;
  /** `provider/model_id`, or null when no gateway record exists for the turn. */
  readonly modelRoute: string | null;
  readonly chosenBy: string | null;
  readonly stopReason: string | null;
  readonly latencyMs: number | null;
  readonly sources: readonly InspectedSource[];
  readonly tools: readonly InspectedTool[];
  readonly question: InspectedQuestion | null;
}

export interface RunInspection {
  readonly runId: string;
  readonly workNodeId: string;
  readonly status: string;
  readonly triggerKind: string;
  readonly triggerMessageId: string | null;
  readonly attachments: readonly string[];
  readonly turns: readonly InspectedTurn[];
}

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

/** Decode the timeline projection. */
export function decodeTimeline(payload: unknown): RunInspection {
  const root = record(payload);
  const run = record(root["run"]);
  const runId = text(run["id"]);
  if (!runId) {
    throw new Error("timeline has no run");
  }
  const trigger = record(root["trigger_message"]);
  const turns: InspectedTurn[] = [];
  for (const raw of list(root["turns"])) {
    const turn = record(raw);
    const turnId = text(turn["id"]);
    if (!turnId) {
      continue;
    }
    let modelRoute: string | null = null;
    let chosenBy: string | null = null;
    let stopReason: string | null = null;
    let latencyMs: number | null = null;
    const tools: InspectedTool[] = [];
    let question: InspectedQuestion | null = null;
    for (const rawStep of list(turn["steps"])) {
      const step = record(rawStep);
      const call = record(step["model_call"]);
      const route = record(call["route"]);
      if (text(route["provider"]) && text(route["model_id"]) && modelRoute === null) {
        modelRoute = `${text(route["provider"]) ?? ""}/${text(route["model_id"]) ?? ""}`;
        chosenBy = text(route["chosen_by"]);
        stopReason = text(call["stop_reason"]);
        latencyMs = number(call["latency_ms"]);
      }
      for (const rawTool of list(step["tool_calls"])) {
        const tool = record(rawTool);
        tools.push({
          toolName: text(tool["tool_name"]) ?? "?",
          status: text(tool["status"]) ?? "?",
          effectId: text(tool["effect_id"]),
          effectClass: text(tool["effect_class"]),
          effectStatus: text(tool["effect_status"]),
        });
      }
      const asked = record(step["question"]);
      if (text(asked["id"]) && question === null) {
        question = {
          id: text(asked["id"]) ?? "",
          prompt: text(asked["prompt"]) ?? "",
          status: text(asked["status"]) ?? "open",
          answer: record(asked["answer"])["value"] ?? null,
        };
      }
    }
    const context = record(turn["context"]);
    const sources: InspectedSource[] = list(context["sources"]).map((rawSource) => {
      const source = record(rawSource);
      const trust = source["trust_level"];
      return {
        kind: text(source["kind"]) ?? "?",
        ref: text(source["source"]) ?? text(record(source["source"])["ref"]) ?? "",
        trustLevel: typeof trust === "number" ? String(trust) : (text(trust) ?? "unknown"),
        tokens: number(source["tokens"]),
      };
    });
    turns.push({
      turnId,
      seq: number(turn["seq"]) ?? turns.length + 1,
      inputKind: text(record(turn["input"])["kind"]) ?? "?",
      status: text(turn["status"]) ?? "?",
      modelRoute,
      chosenBy,
      stopReason,
      latencyMs,
      sources,
      tools,
      question,
    });
  }
  return {
    runId,
    workNodeId: text(run["work_node_id"]) ?? "",
    status: text(run["status"]) ?? "?",
    triggerKind: text(record(run["trigger"])["kind"]) ?? "?",
    triggerMessageId: text(trigger["message_id"]),
    attachments: list(trigger["attachments"]).filter((item): item is string => typeof item === "string"),
    turns,
  };
}
