/**
 * Inbound-hook filters (ADP-007, DOMAIN.md §13.8 A): the typed condition groups a webhook trigger
 * carries, edited as text and sent as typed JSON.
 *
 * A delivery passes when any group holds (OR) and a group holds when every condition in it does
 * (AND). The server owns the evaluation and checks the same bounds again (`hooks.rs`
 * `validate_filters`); this module is the client half: it edits the groups immutably, says what is
 * wrong with them before they are sent, and turns them into the wire shape. Values are typed and
 * never coerced: `5` is a number, `"5"` is a string, `true` is a boolean, `null` is null.
 */

export const HOOK_OPS = ["eq", "ne", "in", "exists", "prefix", "contains", "gt", "gte", "lt", "lte"] as const;
export type HookOp = (typeof HOOK_OPS)[number];

export const MAX_FILTER_GROUPS = 8;
export const MAX_GROUP_CONDITIONS = 8;
export const MAX_PATH_BYTES = 128;
export const MAX_PATH_SEGMENTS = 8;
export const MAX_IN_VALUES = 32;
export const MAX_VALUE_BYTES = 256;

export type Scalar = string | number | boolean | null;

/** A condition as the server stores it: `{path, op, value}`. */
export interface HookConditionWire {
  readonly path: string;
  readonly op: HookOp;
  readonly value: Scalar | readonly Scalar[];
}

/** A condition as the person edits it: the value is text until the draft is checked. */
export interface FilterCondition {
  readonly path: string;
  readonly op: HookOp;
  readonly text: string;
}

export type FilterGroup = readonly FilterCondition[];
export type FilterDraft = readonly FilterGroup[];

/** What kind of value an operator takes, which decides the control that edits it. */
export type ValueKind = "scalar" | "list" | "boolean" | "text" | "number";

export function valueKind(op: HookOp): ValueKind {
  switch (op) {
    case "eq":
    case "ne":
      return "scalar";
    case "in":
      return "list";
    case "exists":
      return "boolean";
    case "prefix":
    case "contains":
      return "text";
    case "gt":
    case "gte":
    case "lt":
    case "lte":
      return "number";
  }
}

export function isHookOp(value: unknown): value is HookOp {
  return typeof value === "string" && (HOOK_OPS as readonly string[]).includes(value);
}

const encoder = new TextEncoder();

function byteLength(text: string): number {
  return encoder.encode(text).length;
}

// ---- literals: typed, never coerced ----

/**
 * One typed value from text. A JSON number, `true`, `false`, `null` or a double-quoted string is
 * that type; anything else is the string as written (so quoting is how a string that looks like
 * another type is written).
 */
export function parseLiteral(text: string): Scalar {
  const trimmed = text.trim();
  if (trimmed === "true") {
    return true;
  }
  if (trimmed === "false") {
    return false;
  }
  if (trimmed === "null") {
    return null;
  }
  if (/^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?$/.test(trimmed)) {
    return Number(trimmed);
  }
  if (trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"')) {
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (typeof parsed === "string") {
        return parsed;
      }
    } catch {
      // Not a JSON string: it is the text as written.
    }
  }
  return trimmed;
}

/** The text that parses back to `value`: quoted only when the bare text would be read as another type. */
export function literalText(value: Scalar, inList = false): string {
  if (typeof value === "string") {
    const bare = value !== "" && parseLiteral(value) === value && value === value.trim() && !(inList && value.includes(","));
    return bare ? value : JSON.stringify(value);
  }
  return value === null ? "null" : String(value);
}

function splitList(text: string): string[] {
  // A comma separates values; a comma inside a double-quoted string does not.
  const parts: string[] = [];
  let current = "";
  let quoted = false;
  let escaped = false;
  for (const char of text) {
    if (escaped) {
      current += char;
      escaped = false;
    } else if (char === "\\" && quoted) {
      current += char;
      escaped = true;
    } else if (char === '"') {
      current += char;
      quoted = !quoted;
    } else if (char === "," && !quoted) {
      parts.push(current);
      current = "";
    } else {
      current += char;
    }
  }
  parts.push(current);
  return parts.map((part) => part.trim()).filter((part) => part.length > 0);
}

// ---- editing: every function returns a new draft ----

export function emptyCondition(): FilterCondition {
  return { path: "", op: "eq", text: "" };
}

/** A new group holding one empty condition. */
export function addGroup(draft: FilterDraft): FilterDraft {
  return draft.length >= MAX_FILTER_GROUPS ? draft : [...draft, [emptyCondition()]];
}

export function removeGroup(draft: FilterDraft, group: number): FilterDraft {
  return draft.filter((_, index) => index !== group);
}

export function addCondition(draft: FilterDraft, group: number): FilterDraft {
  return draft.map((conditions, index) => (index === group && conditions.length < MAX_GROUP_CONDITIONS ? [...conditions, emptyCondition()] : conditions));
}

/** Remove a condition; a group left with none is removed too. */
export function removeCondition(draft: FilterDraft, group: number, condition: number): FilterDraft {
  return draft
    .map((conditions, index) => (index === group ? conditions.filter((_, at) => at !== condition) : conditions))
    .filter((conditions) => conditions.length > 0);
}

/** Change one condition; changing the operator to another kind of value resets the text the old kind could not hold. */
export function updateCondition(draft: FilterDraft, group: number, condition: number, change: Partial<FilterCondition>): FilterDraft {
  return draft.map((conditions, index) =>
    index === group
      ? conditions.map((existing, at) => {
          if (at !== condition) {
            return existing;
          }
          const next = { ...existing, ...change };
          if (change.op !== undefined && change.op !== existing.op && valueKind(change.op) !== valueKind(existing.op)) {
            return { ...next, text: valueKind(change.op) === "boolean" ? "true" : "" };
          }
          return next;
        })
      : conditions,
  );
}

// ---- checking and the wire shape ----

function conditionWire(condition: FilterCondition, at: string): HookConditionWire | string {
  const path = condition.path.trim();
  if (!path.startsWith("/") || byteLength(path) > MAX_PATH_BYTES || path.split("/").length > MAX_PATH_SEGMENTS + 1) {
    return `${at}: a path is a JSON Pointer such as /action or /repository/name (at most ${String(MAX_PATH_SEGMENTS)} segments, ${String(MAX_PATH_BYTES)} bytes)`;
  }
  const scalarOk = (value: Scalar): boolean => typeof value !== "string" || byteLength(value) <= MAX_VALUE_BYTES;
  const text = condition.text;
  switch (valueKind(condition.op)) {
    case "scalar": {
      const value = parseLiteral(text);
      if (text.trim() === "" || !scalarOk(value)) {
        return `${at}: ${condition.op} needs a value of at most ${String(MAX_VALUE_BYTES)} bytes`;
      }
      return { path, op: condition.op, value };
    }
    case "list": {
      const values = splitList(text).map(parseLiteral);
      if (values.length === 0 || values.length > MAX_IN_VALUES || !values.every(scalarOk)) {
        return `${at}: in needs between 1 and ${String(MAX_IN_VALUES)} values, separated by commas`;
      }
      return { path, op: condition.op, value: values };
    }
    case "boolean": {
      const value = text.trim();
      return value === "true" || value === "false" ? { path, op: condition.op, value: value === "true" } : `${at}: exists is true or false`;
    }
    case "text": {
      const value = text.trim() === "" ? "" : text;
      return value === "" || byteLength(value) > MAX_VALUE_BYTES ? `${at}: ${condition.op} needs text of at most ${String(MAX_VALUE_BYTES)} bytes` : { path, op: condition.op, value };
    }
    case "number": {
      const value = parseLiteral(text);
      return typeof value === "number" && Number.isFinite(value) ? { path, op: condition.op, value } : `${at}: ${condition.op} needs a number`;
    }
  }
}

/** What is wrong with the filters, or `null`; the server checks the same bounds again. */
export function filterProblem(draft: FilterDraft): string | null {
  if (draft.length > MAX_FILTER_GROUPS) {
    return `filters have at most ${String(MAX_FILTER_GROUPS)} groups`;
  }
  for (const [groupIndex, group] of draft.entries()) {
    const name = `group ${String(groupIndex + 1)}`;
    if (group.length === 0) {
      return `${name} has no conditions`;
    }
    if (group.length > MAX_GROUP_CONDITIONS) {
      return `${name} has more than ${String(MAX_GROUP_CONDITIONS)} conditions`;
    }
    for (const [conditionIndex, condition] of group.entries()) {
      const result = conditionWire(condition, `${name}, condition ${String(conditionIndex + 1)}`);
      if (typeof result === "string") {
        return result;
      }
    }
  }
  return null;
}

/** The groups as the server wants them. Throws on a draft `filterProblem` refuses. */
export function filtersToWire(draft: FilterDraft): HookConditionWire[][] {
  const problem = filterProblem(draft);
  if (problem) {
    throw new Error(problem);
  }
  return draft.map((group, groupIndex) =>
    group.map((condition, conditionIndex) => {
      const result = conditionWire(condition, `group ${String(groupIndex + 1)}, condition ${String(conditionIndex + 1)}`);
      if (typeof result === "string") {
        throw new Error(result);
      }
      return result;
    }),
  );
}

function scalarOf(value: unknown): Scalar {
  return typeof value === "string" || typeof value === "number" || typeof value === "boolean" ? value : null;
}

function conditionOf(raw: unknown): FilterCondition | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return null;
  }
  const source = raw as Record<string, unknown>;
  const op = source["op"];
  const path = source["path"];
  if (!isHookOp(op) || typeof path !== "string") {
    return null;
  }
  const value = source["value"];
  switch (valueKind(op)) {
    case "list":
      return { path, op, text: (Array.isArray(value) ? value : []).map((entry) => literalText(scalarOf(entry), true)).join(", ") };
    case "boolean":
      return { path, op, text: value === false ? "false" : "true" };
    case "text":
      return { path, op, text: typeof value === "string" ? value : "" };
    case "number":
      return { path, op, text: typeof value === "number" ? String(value) : "" };
    case "scalar":
      return { path, op, text: literalText(scalarOf(value)) };
  }
}

/** Decode a stored trigger's `filters` for editing. Anything that is not a condition is dropped. */
export function filtersFromWire(raw: unknown): FilterDraft {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw
    .map((group): FilterGroup => (Array.isArray(group) ? group.map(conditionOf).filter((entry): entry is FilterCondition => entry !== null) : []))
    .filter((group) => group.length > 0);
}

/** The decoded wire shape of a stored trigger's filters, kept as the server sent them. */
export function wireFiltersOf(raw: unknown): HookConditionWire[][] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw
    .map((group): HookConditionWire[] =>
      Array.isArray(group)
        ? group.flatMap((entry): HookConditionWire[] => {
            const condition = conditionOf(entry);
            if (!condition) {
              return [];
            }
            const wire = conditionWire(condition, "stored");
            return typeof wire === "string" ? [] : [wire];
          })
        : [],
    )
    .filter((group) => group.length > 0);
}

/** A one-line reading of the filters for the routine list: `/action = opened, /n > 3 OR …`. */
export function describeFilters(wire: readonly (readonly HookConditionWire[])[]): string {
  if (wire.length === 0) {
    return "";
  }
  const symbol: Readonly<Record<HookOp, string>> = { eq: "=", ne: "≠", in: "in", exists: "exists", prefix: "starts with", contains: "contains", gt: ">", gte: "≥", lt: "<", lte: "≤" };
  const show = (value: HookConditionWire["value"]): string => (Array.isArray(value) ? `[${(value as readonly Scalar[]).map((entry) => literalText(entry, true)).join(", ")}]` : literalText(value as Scalar));
  return wire.map((group) => group.map((condition) => `${condition.path} ${symbol[condition.op]} ${show(condition.value)}`).join(" and ")).join(" or ");
}
