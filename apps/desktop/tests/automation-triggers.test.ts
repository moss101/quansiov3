/**
 * ADP-007 on the desktop, client half: inbound-hook filters, the secret shown once, dry runs,
 * revision conflicts, time zones and workflow scripts. Pure decoders and state machines, plus the
 * bridge's routine channel (which fills the workspace and refuses what is not a routine command).
 * No server: every shape here is the one DOMAIN.md §13.8 and `crates/server/src/api/routines.rs` name.
 */

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import {
  addCondition,
  addGroup,
  attentionOf,
  budgetOf,
  canSave,
  conflictOf,
  controlParams,
  copyText,
  createRoutineParams,
  decodeDryRun,
  decodeOutline,
  decodeRoutines,
  decodeScriptRuns,
  decodeSettled,
  describeFilters,
  dismissSecret,
  draftOf,
  draftProblem,
  draftsDiffer,
  dryRunDraftParams,
  dryRunRefusal,
  dryRunRoutineParams,
  editDraft,
  fieldErrorsOf,
  filterProblem,
  filterVerdict,
  filterZones,
  filtersFromWire,
  filtersToWire,
  isFixedOffset,
  itemProgress,
  literalText,
  markConflict,
  offsetLabel,
  parseLiteral,
  parseSamplePayload,
  removeCondition,
  removeGroup,
  reviewAfterReload,
  revealFrom,
  rotateHookParams,
  startEdit,
  stepRows,
  supportedZones,
  updateCondition,
  updateRoutineParams,
  valueKind,
  withoutSecret,
  FALLBACK_ZONES,
  NOTHING_SHOWN,
  type FilterDraft,
  type RoutineDraft,
} from "../src/index.js";
import { DesktopBridge } from "../src/main/ipc.js";
import { SessionFile } from "../src/main/session-file.js";
import { UpdateHooks } from "../src/main/updates.js";

const ROUTINE = "rtn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const ENDPOINT = "hk_" + "A".repeat(43);
const SECRET = "whsec_" + "b".repeat(40);
const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";

const condition = (path: string, op: FilterDraft[number][number]["op"], text: string) => ({ path, op, text });

// ------------------------------------------------------------------ filters

describe("typed filter values", () => {
  it("reads a literal by its own type and never coerces", () => {
    expect(parseLiteral("5")).toBe(5);
    expect(parseLiteral(" -2.5e3 ")).toBe(-2500);
    expect(parseLiteral('"5"')).toBe("5");
    expect(parseLiteral("true")).toBe(true);
    expect(parseLiteral('"true"')).toBe("true");
    expect(parseLiteral("null")).toBeNull();
    expect(parseLiteral("opened")).toBe("opened");
    expect(parseLiteral("007")).toBe("007");
    expect(parseLiteral('"unterminated')).toBe('"unterminated');
  });

  it("writes a value as the text that reads back as the same value", () => {
    for (const value of ["opened", "5", "true", "null", "", " padded ", 'say "hi"', 5, 0.5, true, false, null]) {
      expect(parseLiteral(literalText(value))).toBe(value);
    }
    // In a list a comma would split the value, so it is quoted.
    expect(literalText("a,b", true)).toBe('"a,b"');
  });

  it("types each operator's value: scalar, list, boolean, text, number", () => {
    expect(["eq", "ne"].map((op) => valueKind(op as "eq"))).toEqual(["scalar", "scalar"]);
    expect(valueKind("in")).toBe("list");
    expect(valueKind("exists")).toBe("boolean");
    expect(valueKind("prefix")).toBe("text");
    expect(valueKind("contains")).toBe("text");
    expect(["gt", "gte", "lt", "lte"].map((op) => valueKind(op as "gt"))).toEqual(["number", "number", "number", "number"]);
  });
});

describe("filter groups", () => {
  it("builds the typed wire shape: OR of groups, AND within a group", () => {
    const groups: FilterDraft = [
      [condition("/action", "eq", "opened"), condition("/number", "gt", "10"), condition("/draft", "eq", "false")],
      [condition("/labels", "contains", "urgent"), condition("/kind", "in", 'a, "b,c", 3, true, null'), condition("/ref", "exists", "false")],
      [condition("/ref", "prefix", "refs/heads/"), condition("/id", "eq", '"42"')],
    ];
    expect(filterProblem(groups)).toBeNull();
    expect(filtersToWire(groups)).toEqual([
      [
        { path: "/action", op: "eq", value: "opened" },
        { path: "/number", op: "gt", value: 10 },
        { path: "/draft", op: "eq", value: false },
      ],
      [
        { path: "/labels", op: "contains", value: "urgent" },
        { path: "/kind", op: "in", value: ["a", "b,c", 3, true, null] },
        { path: "/ref", op: "exists", value: false },
      ],
      [
        { path: "/ref", op: "prefix", value: "refs/heads/" },
        { path: "/id", op: "eq", value: "42" },
      ],
    ]);
  });

  it("refuses what the server would: paths, values of the wrong type, empty groups and the bounds", () => {
    const one = (entry: ReturnType<typeof condition>): string | null => filterProblem([[entry]]);
    expect(one(condition("action", "eq", "x"))).toMatch(/JSON Pointer/);
    expect(one(condition("/a/b/c/d/e/f/g/h/i", "eq", "x"))).toMatch(/segments/);
    expect(one(condition("/" + "p".repeat(130), "eq", "x"))).toMatch(/bytes/);
    expect(one(condition("/a", "eq", ""))).toMatch(/needs a value/);
    expect(one(condition("/a", "gt", "ten"))).toMatch(/needs a number/);
    expect(one(condition("/a", "gt", "Infinity"))).toMatch(/needs a number/);
    expect(one(condition("/a", "in", " , "))).toMatch(/between 1 and 32/);
    expect(one(condition("/a", "in", Array.from({ length: 33 }, (_, index) => String(index)).join(",")))).toMatch(/between 1 and 32/);
    expect(one(condition("/a", "exists", "maybe"))).toMatch(/true or false/);
    expect(one(condition("/a", "prefix", ""))).toMatch(/needs text/);
    expect(one(condition("/a", "eq", "x".repeat(257)))).toMatch(/256 bytes/);
    expect(filterProblem([[]])).toMatch(/no conditions/);
    expect(() => filtersToWire([[condition("a", "eq", "x")]])).toThrow(/JSON Pointer/);
  });

  it("edits immutably and stops at the bounds", () => {
    let groups: FilterDraft = [];
    for (let count = 0; count < 12; count += 1) {
      groups = addGroup(groups);
    }
    expect(groups).toHaveLength(8);
    let group: FilterDraft = groups.slice(0, 1);
    for (let count = 0; count < 12; count += 1) {
      group = addCondition(group, 0);
    }
    expect(group[0]).toHaveLength(8);
    const first = addGroup([]);
    expect(first).toEqual([[{ path: "", op: "eq", text: "" }]]);
    const edited = updateCondition(first, 0, 0, { path: "/action", text: "opened" });
    expect(edited[0]?.[0]).toEqual({ path: "/action", op: "eq", text: "opened" });
    expect(first[0]?.[0]?.path).toBe("");
    expect(removeCondition(addCondition(edited, 0), 0, 1)).toEqual(edited);
    // A group left with no conditions is removed with its last one.
    expect(removeCondition(edited, 0, 0)).toEqual([]);
    expect(removeGroup(groups, 0)).toHaveLength(7);
  });

  it("resets a value the new operator's kind cannot hold, and keeps one it can", () => {
    const start: FilterDraft = [[condition("/a", "eq", "opened")]];
    expect(updateCondition(start, 0, 0, { op: "ne" })[0]?.[0]?.text).toBe("opened");
    expect(updateCondition(start, 0, 0, { op: "exists" })[0]?.[0]?.text).toBe("true");
    expect(updateCondition(start, 0, 0, { op: "gt" })[0]?.[0]?.text).toBe("");
  });

  it("decodes stored filters for editing and writes them back unchanged", () => {
    const wire = [
      [
        { path: "/action", op: "in", value: ["opened", "5", 5, "a,b"] },
        { path: "/n", op: "lte", value: 3 },
        { path: "/x", op: "exists", value: false },
      ],
    ];
    const draft = filtersFromWire(wire);
    expect(filtersToWire(draft)).toEqual(wire);
    expect(filtersFromWire("nope")).toEqual([]);
    expect(filtersFromWire([[{ path: "/a", op: "nonsense", value: 1 }], []])).toEqual([]);
    expect(describeFilters(filtersToWire(draft))).toBe('/action in [opened, "5", 5, "a,b"] and /n ≤ 3 and /x exists false');
    expect(describeFilters([])).toBe("");
  });
});

// ------------------------------------------------------------------ time zones

describe("time zones", () => {
  it("lists the runtime's IANA zones with UTC first, and falls back to a short list", () => {
    expect(supportedZones({ supportedValuesOf: () => ["Asia/Tokyo", "UTC", "Europe/Paris"] })).toEqual(["UTC", "Asia/Tokyo", "Europe/Paris"]);
    expect(supportedZones({})).toBe(FALLBACK_ZONES);
    expect(supportedZones({ supportedValuesOf: () => [] })).toBe(FALLBACK_ZONES);
    expect(
      supportedZones({
        supportedValuesOf: () => {
          throw new RangeError("invalid key");
        },
      }),
    ).toBe(FALLBACK_ZONES);
    expect(FALLBACK_ZONES[0]).toBe("UTC");
    expect(supportedZones().length).toBeGreaterThan(0);
  });

  it("searches by what is typed, ignoring case, underscores, spaces and slashes", () => {
    const zones = ["UTC", "America/New_York", "America/North_Dakota/Center", "Europe/London", "Asia/Kolkata"];
    expect(filterZones(zones, "new york")).toEqual(["America/New_York"]);
    expect(filterZones(zones, "america/n")).toEqual(["America/New_York", "America/North_Dakota/Center"]);
    expect(filterZones(zones, "kolk")).toEqual(["Asia/Kolkata"]);
    // A zone that starts with the text comes before one that only contains it.
    expect(filterZones(["Europe/London", "London/Fake"], "lon")).toEqual(["London/Fake", "Europe/London"]);
    expect(filterZones(zones, "dak")).toEqual(["America/North_Dakota/Center"]);
    expect(filterZones(zones, "")).toEqual(zones);
    expect(filterZones(zones, "", 2)).toEqual(["UTC", "America/New_York"]);
    expect(filterZones(zones, "zzz")).toEqual([]);
  });

  it("recognises a fixed offset and labels an offset", () => {
    expect(["+02:00", "-05:30", "+14:00"].every(isFixedOffset)).toBe(true);
    expect(["2:00", "+25:00", "UTC", "+02:60"].some(isFixedOffset)).toBe(false);
    expect(offsetLabel(7200)).toBe("+02:00");
    expect(offsetLabel(-19800)).toBe("-05:30");
    expect(offsetLabel(0)).toBe("+00:00");
  });
});

// ------------------------------------------------------------------ the projection

const HOOK_ROUTINE = {
  id: ROUTINE,
  objective_template: { title: "Triage", prompt: "Handle {{payload}}", workflow: { version: 1, max_agent_calls: 40, estimated_agent_calls: 12, steps: [{ id: "collect", kind: "agent" }] } },
  trigger: { kind: "webhook", spec: "", timezone: "UTC", filters: [[{ path: "/action", op: "eq", value: "opened" }]] },
  absence_policy: "skip",
  status: "active",
  owner_user_id: USER,
  teammate_id: "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
  budget: { max_cost: 5 },
  notification_prefs: { on_fail: true },
  revision: 4,
  hook: { endpoint_id: ENDPOINT, path: `/v1/hooks/${ENDPOINT}`, secret: "set", deliveries: { accepted: 3, filtered: 2 } },
  script: { max_agent_calls: 40, estimated_agent_calls: 12, steps: [{ id: "collect", kind: "agent" }, { id: "each", kind: "parallel", over: "collect", path: "/items", max_items: 25 }, { id: "summary", kind: "reduce", of: "each" }, { id: "note", kind: "log" }] },
  script_runs: [
    {
      fire_id: "rfi_1",
      run_id: "run_1",
      objective_id: "obj_1",
      status: "waiting",
      calls_used: 40,
      max_agent_calls: 40,
      approved_calls: 0,
      estimated_agent_calls: 12,
      open_gate: { gate: "extend_budget", calls: 60, approval_id: "apr_1" },
      error: null,
      steps: [
        { id: "collect", kind: "agent", status: "done", node_id: "wn_1" },
        { id: "each", kind: "parallel", status: "running", items: { total: 25, finished: 10, failed: 2 } },
        { id: "summary", kind: "reduce", status: "pending", node_id: null },
        { id: "note", kind: "log", status: "pending" },
      ],
    },
  ],
  fires: [{ id: "rfi_1", fire_key: "routine:rtn:hook:d1", source: "hook", status: "fired", attempts: 1, run_id: "run_1", payload_digest: "ab".repeat(32) }],
};

describe("the routine projection", () => {
  const [routine] = decodeRoutines({ items: [HOOK_ROUTINE] });

  it("decodes the revision, the hook's endpoint and deliveries, filters and hook fires", () => {
    expect(routine?.revision).toBe(4);
    expect(routine?.trigger).toEqual({ kind: "webhook", spec: "", timezone: "UTC", filters: [[{ path: "/action", op: "eq", value: "opened" }]] });
    expect(routine?.hook).toEqual({ endpointId: ENDPOINT, path: `/v1/hooks/${ENDPOINT}`, secretSet: true, deliveries: { accepted: 3, filtered: 2 } });
    expect(routine?.fires[0]).toMatchObject({ source: "hook", payloadDigest: "ab".repeat(32) });
    expect(decodeRoutines({ items: [{ ...HOOK_ROUTINE, hook: null, revision: undefined }] })[0]).toMatchObject({ hook: null, revision: 1 });
  });

  it("never holds a secret: the projection has none and the view cannot", () => {
    expect(JSON.stringify(routine)).not.toContain("whsec_");
    expect(Object.keys(routine?.hook ?? {})).not.toContain("secret");
  });

  it("keeps what a full replacement must send back: the script, the budget, the preferences, the teammate", () => {
    expect(routine?.workflow).toEqual(HOOK_ROUTINE.objective_template.workflow);
    expect(routine?.budget).toEqual({ max_cost: 5 });
    expect(routine?.notificationPrefs).toEqual({ on_fail: true });
    const draft = routine ? draftOf(routine) : null;
    expect(draft?.workflow).toEqual(HOOK_ROUTINE.objective_template.workflow);
    const params = draft ? updateRoutineParams(ROUTINE, 4, draft) : {};
    expect(params).toMatchObject({
      routine_id: ROUTINE,
      expected_revision: 4,
      title: "Triage",
      budget: { max_cost: 5 },
      notification_prefs: { on_fail: true },
      teammate_id: "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      workflow: HOOK_ROUTINE.objective_template.workflow,
      trigger: { kind: "webhook", spec: "", timezone: "UTC", filters: [[{ path: "/action", op: "eq", value: "opened" }]] },
    });
  });
});

describe("routine commands", () => {
  const base: RoutineDraft = { title: "Triage", prompt: "Handle {{payload}}", trigger: { kind: "webhook", spec: "" }, filters: [[condition("/action", "eq", "opened")]] };

  it("creates a webhook with typed filters, no spec and a request id", () => {
    expect(draftProblem(base)).toBeNull();
    expect(createRoutineParams("ws_1", base, "create-1")).toEqual({
      workspace_id: "ws_1",
      title: "Triage",
      prompt: "Handle {{payload}}",
      trigger: { kind: "webhook", spec: "", timezone: "UTC", filters: [[{ path: "/action", op: "eq", value: "opened" }]] },
      absence_policy: "skip",
      create_request_id: "create-1",
    });
    expect(createRoutineParams("ws_1", { ...base, filters: [] }).trigger).toEqual({ kind: "webhook", spec: "", timezone: "UTC" });
    expect(draftProblem({ ...base, filters: [[condition("/a", "gt", "ten")]] })).toMatch(/needs a number/);
    expect(() => createRoutineParams("ws_1", { ...base, filters: [[condition("a", "eq", "x")]] })).toThrow(/JSON Pointer/);
  });

  it("sends a zone for a cron trigger, and never the viewer's zone for another kind", () => {
    const cron: RoutineDraft = { title: "t", prompt: "p", trigger: { kind: "cron", spec: "0 9 * * *", timezone: "Europe/London" } };
    expect(createRoutineParams("ws_1", cron).trigger).toEqual({ kind: "cron", spec: "0 9 * * *", timezone: "Europe/London" });
    expect(createRoutineParams("ws_1", { ...cron, trigger: { kind: "interval", spec: "30m", timezone: "Europe/London" } }).trigger).toEqual({ kind: "interval", spec: "30m", timezone: "UTC" });
    // Filters are only for a webhook.
    expect(createRoutineParams("ws_1", { ...cron, filters: [[condition("/a", "eq", "x")]] }).trigger).not.toHaveProperty("filters");
  });

  it("an update is a full replacement under the revision it started from", () => {
    const params = updateRoutineParams(ROUTINE, 7, { title: " New ", prompt: "p", trigger: { kind: "cron", spec: "0 9 * * *", timezone: "Asia/Kolkata" } });
    expect(params).toEqual({
      routine_id: ROUTINE,
      expected_revision: 7,
      title: "New",
      prompt: "p",
      trigger: { kind: "cron", spec: "0 9 * * *", timezone: "Asia/Kolkata" },
      absence_policy: "skip",
      budget: {},
      notification_prefs: {},
    });
    expect(() => updateRoutineParams(ROUTINE, 7, { title: "", prompt: "p", trigger: { kind: "interval", spec: "1h" } })).toThrow(/title/);
  });

  it("names the revision on pause, resume, delete and rotate, but not on run now", () => {
    const view = decodeRoutines({ items: [HOOK_ROUTINE] })[0];
    if (!view) {
      throw new Error("a routine");
    }
    expect(controlParams("PauseRoutine", view)).toEqual({ routine_id: ROUTINE, expected_revision: 4 });
    expect(controlParams("ResumeRoutine", view)).toEqual({ routine_id: ROUTINE, expected_revision: 4 });
    expect(controlParams("DeleteRoutine", view)).toEqual({ routine_id: ROUTINE, expected_revision: 4 });
    expect(controlParams("TriggerRoutineNow", view)).toEqual({ routine_id: ROUTINE });
    expect(rotateHookParams(view)).toEqual({ routine_id: ROUTINE, expected_revision: 4 });
  });

  it("builds a dry run for a saved routine or for a draft, with the sample only for a webhook", () => {
    expect(dryRunRoutineParams(ROUTINE, null)).toEqual({ routine_id: ROUTINE });
    expect(dryRunRoutineParams(ROUTINE, { action: "opened" })).toEqual({ routine_id: ROUTINE, sample_payload: { action: "opened" } });
    const params = dryRunDraftParams("ws_1", base, { action: "opened" });
    expect(params).toEqual({
      workspace_id: "ws_1",
      definition: { title: "Triage", prompt: "Handle {{payload}}", trigger: { kind: "webhook", spec: "", timezone: "UTC", filters: [[{ path: "/action", op: "eq", value: "opened" }]] } },
      sample_payload: { action: "opened" },
    });
    // A draft the server would refuse is still sent: the answer lists every problem at once.
    expect(dryRunDraftParams("ws_1", { title: "", prompt: "", trigger: { kind: "interval", spec: "soon" } }, { a: 1 })).toEqual({
      workspace_id: "ws_1",
      definition: { title: "", prompt: "", trigger: { kind: "interval", spec: "soon", timezone: "UTC" } },
    });
  });

  it("names the fields in which two drafts differ", () => {
    const left: RoutineDraft = { title: "A", prompt: "p", trigger: { kind: "cron", spec: "0 9 * * *", timezone: "UTC" }, absencePolicy: "skip" };
    expect(draftsDiffer(left, { ...left, title: " A " })).toEqual([]);
    expect(draftsDiffer(left, { ...left, title: "B", prompt: "q", trigger: { kind: "cron", spec: "0 8 * * *", timezone: "Asia/Tokyo" }, absencePolicy: "queue", filters: [[condition("/a", "eq", "x")]] })).toEqual([
      "title",
      "prompt",
      "schedule or event",
      "time zone",
      "filters",
      "missed windows",
    ]);
  });
});

// ------------------------------------------------------------------ the secret, once

describe("the signing secret is shown once", () => {
  const created = { ...HOOK_ROUTINE, secret: SECRET, replayed: false };

  it("a create or rotate result makes a reveal with the endpoint and the secret", () => {
    const reveal = revealFrom("created", created);
    expect(reveal).toEqual({ kind: "created", routineId: ROUTINE, path: `/v1/hooks/${ENDPOINT}`, secret: SECRET, replayed: false });
    expect(revealFrom("rotated", created)?.kind).toBe("rotated");
    expect(copyText(reveal, "path")).toBe(`/v1/hooks/${ENDPOINT}`);
    expect(copyText(reveal, "secret")).toBe(SECRET);
  });

  it("a replayed create shows the endpoint and no secret; another trigger kind shows nothing", () => {
    const reveal = revealFrom("created", { ...created, secret: undefined, replayed: true });
    expect(reveal).toMatchObject({ secret: null, replayed: true });
    expect(copyText(reveal, "secret")).toBeNull();
    expect(revealFrom("created", { id: ROUTINE, trigger: { kind: "interval" }, hook: null })).toBeNull();
    expect(revealFrom("created", "nonsense")).toBeNull();
  });

  it("dismissing forgets the secret: the state holds nothing that contains it", () => {
    const shown = revealFrom("created", created);
    expect(JSON.stringify(shown)).toContain(SECRET);
    const after = dismissSecret();
    expect(after).toBe(NOTHING_SHOWN);
    expect(JSON.stringify(after)).not.toContain("whsec_");
    expect(copyText(after, "secret")).toBeNull();
    expect(copyText(after, "path")).toBeNull();
  });

  it("anything that keeps a result around keeps it without the secret", () => {
    const kept = withoutSecret(created) as Record<string, unknown>;
    expect(JSON.stringify(kept)).not.toContain("whsec_");
    expect(kept["secret"]).toBe("shown once");
    expect(withoutSecret({ id: ROUTINE })).toEqual({ id: ROUTINE });
    // The projection (what the list decodes) is shown as `set` and carries none.
    expect(JSON.stringify(decodeRoutines({ items: [created] }))).not.toContain("whsec_");
  });
});

// ------------------------------------------------------------------ refusals and conflicts

describe("refusals", () => {
  it("decodes the settled payload, keeping the typed details", () => {
    expect(decodeSettled({ ok: true, result: { id: ROUTINE } })).toEqual({ ok: true, result: { id: ROUTINE } });
    expect(decodeSettled({ ok: false, error: { code: "RATE_LIMITED", message: "slow down", retryable: true, details: null } })).toEqual({ ok: false, failure: { code: "RATE_LIMITED", message: "slow down", details: null } });
    expect(decodeSettled(null)).toMatchObject({ ok: false, failure: { code: "INTERNAL" } });
  });

  it("recognises a revision conflict and nothing else as one", () => {
    const refused = decodeSettled({ ok: false, error: { code: "CONFLICT_REVISION", message: "moved on", details: { routine_id: ROUTINE, expected_revision: 3, current_revision: 5 } } });
    expect(refused.ok ? null : conflictOf(refused.failure)).toEqual({ routineId: ROUTINE, expectedRevision: 3, currentRevision: 5 });
    expect(conflictOf({ code: "CONFLICT_STATE", message: "paused", details: null })).toBeNull();
    expect(conflictOf({ code: "CONFLICT_REVISION", message: "x", details: null })).toEqual({ routineId: "", expectedRevision: null, currentRevision: null });
  });

  it("puts a time-zone refusal on the zone field, a closer-than-allowed schedule on the trigger, and the rest on the form", () => {
    expect(fieldErrorsOf({ code: "AUTOMATION_TIMEZONE_UNKNOWN", message: "unknown time zone", details: { timezone: "Mars/Olympus" } })).toEqual({ timezone: "unknown time zone (Mars/Olympus)" });
    expect(fieldErrorsOf({ code: "AUTOMATION_LIMIT_EXCEEDED", message: "too often", details: { limit: "min_interval_seconds", allowed: 60, value: 10 } }).trigger).toMatch(/min_interval_seconds: 10, allowed 60/);
    expect(fieldErrorsOf({ code: "AUTOMATION_LIMIT_EXCEEDED", message: "too many calls", details: { limit: "max_agent_calls_ceiling", allowed: 100, value: 500 } }).budget).toMatch(/500/);
    expect(fieldErrorsOf({ code: "AUTOMATION_LIMIT_EXCEEDED", message: "too many", details: { limit: "max_enabled_triggers", allowed: 10, value: 11 } }).form).toMatch(/max_enabled_triggers/);
    expect(fieldErrorsOf({ code: "VALIDATION_SCHEMA", message: "filter group 0, path \"/a\": the value does not fit Gt", details: null })).toHaveProperty("filters");
    expect(fieldErrorsOf({ code: "VALIDATION_SCHEMA", message: "a cron expression has five fields", details: null })).toHaveProperty("trigger");
    expect(fieldErrorsOf({ code: "VALIDATION_SCHEMA", message: "prompt is required", details: null })).toHaveProperty("form");
    expect(fieldErrorsOf({ code: "NOT_FOUND", message: "x", details: null })).toEqual({});
  });
});

describe("an edit that meets a revision conflict", () => {
  const current = decodeRoutines({ items: [{ ...HOOK_ROUTINE, revision: 4, objective_template: { title: "Triage", prompt: "p" }, trigger: { kind: "interval", spec: "1h", timezone: "UTC" }, script: null, script_runs: [] }] })[0];
  const later = decodeRoutines({ items: [{ ...HOOK_ROUTINE, revision: 6, objective_template: { title: "Triage (renamed)", prompt: "p" }, trigger: { kind: "interval", spec: "2h", timezone: "UTC" }, script: null, script_runs: [] }] })[0];
  if (!current || !later) {
    throw new Error("routines");
  }

  it("keeps the person's draft, moves the base only on review, and names what differs", () => {
    const started = startEdit(current, draftOf(current));
    expect(started.baseRevision).toBe(4);
    expect(canSave(started)).toBe(true);
    const mine = editDraft(started, { ...started.draft, title: "Triage (mine)", prompt: "my prompt" });
    const conflicted = markConflict(mine, { routineId: ROUTINE, expectedRevision: 4, currentRevision: 6 });
    // The draft is still there and cannot be re-sent until it has been reviewed.
    expect(conflicted.draft.title).toBe("Triage (mine)");
    expect(conflicted.baseRevision).toBe(4);
    expect(canSave(conflicted)).toBe(false);
    expect(conflicted.conflict).toMatchObject({ currentRevision: 6, reloaded: false });

    const reviewed = reviewAfterReload(conflicted, later, draftOf(later));
    expect(reviewed.draft.title).toBe("Triage (mine)");
    expect(reviewed.draft.prompt).toBe("my prompt");
    expect(reviewed.baseRevision).toBe(6);
    expect(reviewed.conflict).toMatchObject({ currentRevision: 6, reloaded: true, changedFields: ["title", "prompt", "schedule or event"] });
    expect(canSave(reviewed)).toBe(true);
    // The save that follows names the revision that was reviewed.
    expect(updateRoutineParams(reviewed.routineId, reviewed.baseRevision, reviewed.draft)).toMatchObject({ expected_revision: 6, title: "Triage (mine)", prompt: "my prompt" });
  });

  it("a routine that is gone cannot be saved over", () => {
    const conflicted = markConflict(startEdit(current, draftOf(current)), { routineId: ROUTINE, expectedRevision: 4, currentRevision: null });
    const gone = reviewAfterReload(conflicted, null, null);
    expect(gone.conflict).toMatchObject({ currentRevision: null, reloaded: true });
    expect(canSave(gone)).toBe(false);
    expect(gone.draft).toEqual(conflicted.draft);
  });
});

// ------------------------------------------------------------------ the dry run

const DRY_RUN = {
  valid: false,
  problems: [{ code: "VALIDATION_SCHEMA", message: "a cron expression has five fields", at: "trigger", details: null }],
  trigger: { kind: "webhook", timezone: null },
  next_windows: [],
  filter: { evaluated: true, matched: true, matched_group: 1 },
  instruction: {
    blocks: [
      { type: "text", text: "Handle " },
      { type: "untrusted_data", source: "inbound_hook", trust_level: "untrusted_external", text: '{"action":"opened"}' },
    ],
    bytes: 211,
  },
  script: { steps: [{ id: "collect", kind: "agent" }], static_calls: 2, worst_case_calls: 27, estimated_agent_calls: 25, max_agent_calls: 40, approval_threshold_calls: 20, needs_approval: true, may_need_approval: true },
};

describe("the dry run", () => {
  it("decodes what would fire: windows, zone, filter verdict, the rendered plan and the script", () => {
    const view = decodeDryRun({
      ...DRY_RUN,
      valid: true,
      problems: [],
      trigger: { kind: "cron", timezone: { name: "America/New_York", utc_offset_seconds: -18000 } },
      next_windows: ["2026-03-08T07:00:00+00:00", "2026-03-09T06:30:00+00:00"],
    });
    expect(view.valid).toBe(true);
    expect(view.zone).toEqual({ name: "America/New_York", offset: "-05:00" });
    expect(view.nextWindows).toHaveLength(2);
    expect(view.filter).toEqual({ evaluated: true, matched: true, matchedGroup: 1 });
    expect(filterVerdict(view)).toBe("passes");
    expect(view.instruction.blocks.map((block) => block.type)).toEqual(["text", "untrusted_data"]);
    expect(view.instruction.blocks[1]).toMatchObject({ source: "inbound_hook", text: '{"action":"opened"}' });
    expect(view.script).toMatchObject({ staticCalls: 2, estimatedAgentCalls: 25, maxAgentCalls: 40, approvalThresholdCalls: 20, needsApproval: true });
  });

  it("decodes problems and a filter that blocks, and an empty answer as empty", () => {
    const view = decodeDryRun({ ...DRY_RUN, filter: { evaluated: true, matched: false, matched_group: null }, script: null });
    expect(view.valid).toBe(false);
    expect(view.problems).toEqual([{ code: "VALIDATION_SCHEMA", message: "a cron expression has five fields", at: "trigger" }]);
    expect(filterVerdict(view)).toBe("filtered");
    expect(view.script).toBeNull();
    const empty = decodeDryRun(null);
    expect(empty).toMatchObject({ valid: false, problems: [], nextWindows: [], zone: null, script: null });
    expect(filterVerdict(empty)).toBe("not_evaluated");
  });

  it("reads a sample payload: nothing, one JSON object, or a reason", () => {
    expect(parseSamplePayload("  ")).toEqual({ ok: true, value: null });
    expect(parseSamplePayload('{"a": 1}')).toEqual({ ok: true, value: { a: 1 } });
    expect(parseSamplePayload("[1]")).toEqual({ ok: false, message: "a sample payload is one JSON object" });
    expect(parseSamplePayload("{nope")).toEqual({ ok: false, message: "a sample payload is valid JSON" });
    expect(parseSamplePayload(JSON.stringify({ big: "x".repeat(70 * 1024) }))).toEqual({ ok: false, message: "a sample payload is at most 64 KiB" });
  });

  it("tells a rate limit from a refusal", () => {
    expect(dryRunRefusal({ code: "RATE_LIMITED", message: "", details: null })).toBe("rate_limited");
    expect(dryRunRefusal({ code: "VALIDATION_SCHEMA", message: "", details: null })).toBe("refused");
  });
});

// ------------------------------------------------------------------ workflow scripts

describe("workflow scripts", () => {
  const outline = decodeOutline(HOOK_ROUTINE.script);
  const runs = decodeScriptRuns(HOOK_ROUTINE.script_runs);
  if (!outline) {
    throw new Error("an outline");
  }

  it("decodes the outline and each run, including the open gate", () => {
    expect(outline.steps.map((step) => [step.id, step.kind, step.after])).toEqual([
      ["collect", "agent", null],
      ["each", "parallel", "collect"],
      ["summary", "reduce", "each"],
      ["note", "log", null],
    ]);
    expect(outline.steps[1]?.maxItems).toBe(25);
    expect(runs[0]).toMatchObject({ status: "waiting", callsUsed: 40, maxAgentCalls: 40, openGate: { kind: "extend_budget", calls: 60, approvalId: "apr_1" } });
    expect(decodeOutline(null)).toBeNull();
    expect(decodeScriptRuns("nope")).toEqual([]);
  });

  it("lists every step with the status the run reports, and item counts for a parallel step", () => {
    const rows = stepRows(outline, runs[0] ?? null);
    expect(rows.map((row) => [row.id, row.status])).toEqual([["collect", "done"], ["each", "running"], ["summary", "pending"], ["note", "pending"]]);
    expect(itemProgress(rows[1]?.items ?? null)).toEqual({ finished: 10, total: 25, failed: 2, fraction: 0.4 });
    expect(itemProgress(null)).toBeNull();
    // With no run, every step is pending.
    expect(stepRows(outline, null).every((row) => row.status === "pending")).toBe(true);
  });

  it("measures the call budget and calls a run held at its cap paused", () => {
    expect(budgetOf(outline, runs[0] ?? null)).toEqual({ used: 40, max: 40, estimated: 12, fraction: 1, level: "paused" });
    expect(budgetOf(outline, null)).toEqual({ used: 0, max: 40, estimated: 12, fraction: 0, level: "ok" });
    expect(budgetOf(null, null)).toBeNull();
    const run = runs[0];
    if (!run) {
      throw new Error("a run");
    }
    expect(budgetOf(outline, { ...run, openGate: null, callsUsed: 33 })?.level).toBe("near");
    expect(budgetOf(outline, { ...run, openGate: null, callsUsed: 40 })?.level).toBe("exhausted");
    expect(budgetOf(outline, { ...run, openGate: null, callsUsed: 5 })?.level).toBe("ok");
  });

  it("asks for attention on a paused budget and on a fan-out above the threshold, and on nothing else", () => {
    expect(attentionOf(runs[0] ?? null)).toEqual({ kind: "paused_budget", calls: 60, approvalId: "apr_1" });
    const [fanOut] = decodeScriptRuns([{ ...HOOK_ROUTINE.script_runs[0], open_gate: { gate: "fan_out", calls: 30, approval_id: "apr_2" } }]);
    expect(attentionOf(fanOut ?? null)).toEqual({ kind: "needs_approval", calls: 30, approvalId: "apr_2" });
    const [clear] = decodeScriptRuns([{ ...HOOK_ROUTINE.script_runs[0], open_gate: null, status: "running" }]);
    expect(attentionOf(clear ?? null)).toEqual({ kind: "none" });
    expect(attentionOf(null)).toEqual({ kind: "none" });
  });
});

// ------------------------------------------------------------------ the bridge's routine channel

interface Call {
  readonly method: string;
  readonly url: string;
  readonly body: Record<string, unknown> | null;
}

function bridge(calls: Call[], answer: (call: Call) => { status: number; body: unknown }): DesktopBridge {
  return new DesktopBridge({
    serverOrigin: "http://127.0.0.1:8080",
    auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
    session: new SessionFile(mkdtempSync(join(tmpdir(), "quansio-automation-"))),
    openSocket: () => {
      throw new Error("no socket in this test");
    },
    updates: new UpdateHooks("stable", null, "http://x", false),
    consumer: "desktop:test",
    push: () => undefined,
    fetch: (url, init) => {
      const call: Call = { method: init?.method ?? "GET", url, body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null };
      calls.push(call);
      const reply = answer(call);
      return Promise.resolve(new Response(JSON.stringify(reply.body), { status: reply.status, headers: { "content-type": "application/json" } }));
    },
  });
}

const OK = { status: 200, body: { command_id: "c", accepted_at: "now", replayed: false, result: { id: ROUTINE } } };

describe("the routine channel", () => {
  it("dry-runs a draft in the workspace main acts in, and a saved routine by its id", async () => {
    const calls: Call[] = [];
    const handlers = bridge(calls, () => OK).handlers();
    await handlers["routines.command"]({ name: "DryRunRoutine", params: { definition: { title: "t", prompt: "p", trigger: { kind: "interval", spec: "1h" } }, workspace_id: "ws_someone_elses" } });
    await handlers["routines.command"]({ name: "DryRunRoutine", params: { routine_id: ROUTINE, sample_payload: { a: 1 } } });
    expect(calls.map((call) => call.url.split("/").pop())).toEqual(["DryRunRoutine", "DryRunRoutine"]);
    expect(calls[0]?.body?.["params"]).toMatchObject({ workspace_id: WORKSPACE, definition: { title: "t" } });
    expect(calls[1]?.body?.["params"]).toEqual({ routine_id: ROUTINE, sample_payload: { a: 1 } });
  });

  it("rotates a hook's secret with the revision, and refuses a routine id that is not one", async () => {
    const calls: Call[] = [];
    const handlers = bridge(calls, () => OK).handlers();
    await handlers["routines.command"]({ name: "RotateRoutineHook", params: { routine_id: ROUTINE, expected_revision: 4 } });
    expect(calls[0]?.url).toMatch(/RotateRoutineHook$/);
    expect(calls[0]?.body?.["params"]).toEqual({ routine_id: ROUTINE, expected_revision: 4 });
    const refused = (payload: unknown) => handlers["routines.command"](payload);
    expect(await refused({ name: "RotateRoutineHook", params: { routine_id: "run_1" } })).toMatchObject({ ok: false, error: { message: expect.stringMatching(/rtn_/) as string } });
    expect(await refused({ name: "RotateRoutineHook", params: {} })).toMatchObject({ ok: false, error: { message: expect.stringMatching(/rtn_/) as string } });
    expect(await refused({ name: "DryRunRoutine", params: { routine_id: "run_1" } })).toMatchObject({ ok: false, error: { message: expect.stringMatching(/rtn_/) as string } });
    expect(await refused({ name: "DeleteWorkspace", params: { routine_id: ROUTINE } })).toMatchObject({ ok: false, error: { message: expect.stringMatching(/routine command/) as string } });
    // None of the refused calls reached the server.
    expect(calls).toHaveLength(1);
  });

  it("returns a revision conflict with its details, and a rate limit, as data the page branches on", async () => {
    const conflict = bridge([], () => ({ status: 409, body: { code: "CONFLICT_REVISION", message: "the routine is at revision 6", correlation_id: "c", retryable: false, details: { routine_id: ROUTINE, expected_revision: 4, current_revision: 6 } } })).handlers();
    const settled = decodeSettled(await conflict["routines.command"]({ name: "UpdateRoutine", params: { routine_id: ROUTINE, expected_revision: 4, title: "t", prompt: "p", trigger: { kind: "interval", spec: "1h" } } }));
    expect(settled.ok ? null : conflictOf(settled.failure)).toEqual({ routineId: ROUTINE, expectedRevision: 4, currentRevision: 6 });

    const limited = bridge([], () => ({ status: 429, body: { code: "RATE_LIMITED", message: "too many dry runs", correlation_id: "c", retryable: true } })).handlers();
    const refused = decodeSettled(await limited["routines.command"]({ name: "DryRunRoutine", params: { routine_id: ROUTINE } }));
    expect(refused.ok ? null : dryRunRefusal(refused.failure)).toBe("rate_limited");
  });
});
