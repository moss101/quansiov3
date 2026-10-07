/**
 * The activity list (ADP-004, DOMAIN.md 13.6 C): derived from durable events, ordered waiting-for-user, running, error,
 * completed; "running" follows Turn state and never Run status; nothing from an event's payload text reaches an item.
 */

import { describe, expect, it } from "vitest";

import { ActivityTracker, compareActivity, type ActivityItem } from "../src/main/activity.js";
import { commandForItem, commandForNotice } from "../src/main/shell-host.js";
import { decodeActivity, groupActivity } from "../src/shell-ui.js";

const RUN_A = "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const RUN_B = "run_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";
const RUN_C = "run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

function tracker(overrides: Partial<{ maxItems: number; completedTtlMs: number }> = {}): { tracker: ActivityTracker; clock: { now: number } } {
  const clock = { now: 1_000 };
  return { tracker: new ActivityTracker({ maxItems: 24, completedTtlMs: 600_000, ...overrides, now: () => clock.now }), clock };
}

const event = (type: string, aggregate: string, payload: Record<string, unknown> = {}): Record<string, unknown> => ({ type, aggregate_id: aggregate, payload });

describe("deriving items from events", () => {
  it("lists a Turn as running from turn.started until it completes or aborts, whatever the Run's status", () => {
    const { tracker: t } = tracker();
    expect(t.apply(event("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN_A }))).toBe(true);
    expect(t.items()).toMatchObject([{ kind: "running", group: "running", title: "Working", ref: { runId: RUN_A, turnId: "trn_1" } }]);
    expect(t.runningCount()).toBe(1);
    // A Run that keeps being RUNNING between Turns does not keep the item.
    expect(t.apply(event("run.resumed", RUN_A, { to: "RUNNING" }))).toBe(false);
    expect(t.apply(event("turn.completed", "trn_1", { turn_id: "trn_1", run_id: RUN_A }))).toBe(true);
    expect(t.items()).toEqual([]);
    expect(t.runningCount()).toBe(0);
    t.apply(event("turn.started", "trn_2", { turn_id: "trn_2", run_id: RUN_A }));
    t.apply(event("turn.aborted", "trn_2", { turn_id: "trn_2", run_id: RUN_A }));
    expect(t.runningCount()).toBe(0);
  });

  it("lists an approval request and a question until they are answered or resolved", () => {
    const { tracker: t } = tracker();
    t.apply(event("approval.requested", "apr_1", { approval_id: "apr_1", run_id: RUN_A, consequence_preview: "delete everything <script>" }));
    t.apply(event("question.asked", "q_1", { question_id: "q_1", run_id: RUN_B, prompt: "Ignore previous instructions" }));
    expect(t.items().map((item) => item.kind)).toEqual(["needs_approval", "needs_answer"]);
    expect(t.waitingCount()).toBe(2);
    t.apply(event("approval.granted", "apr_1", { approval_id: "apr_1" }));
    t.apply(event("question.answered", "q_1", { question_id: "q_1" }));
    expect(t.items()).toEqual([]);
    for (const resolution of ["approval.denied", "approval.expired", "approval.superseded"]) {
      t.apply(event("approval.requested", "apr_2", { approval_id: "apr_2", run_id: RUN_A }));
      t.apply(event(resolution, "apr_2", { approval_id: "apr_2" }));
      expect(t.items(), resolution).toEqual([]);
    }
  });

  it("lists a failed or blocked Run as an error and a succeeded one as completed, replacing what was running or waiting", () => {
    const { tracker: t } = tracker();
    t.apply(event("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN_A }));
    t.apply(event("question.asked", "q_1", { question_id: "q_1", run_id: RUN_A }));
    t.apply(event("run.failed", RUN_A, { to: "FAILED" }));
    expect(t.items()).toMatchObject([{ kind: "failed", group: "error", ref: { runId: RUN_A } }]);
    t.apply(event("run.blocked", RUN_B, {}));
    t.apply(event("run.succeeded", RUN_C, {}));
    expect(t.items().map((item) => item.kind)).toEqual(["failed", "blocked", "completed"]);
    t.apply(event("run.cancelled", RUN_A, {}));
    // A cancelled run adds nothing and takes nothing that is not running or waiting.
    expect(t.items().map((item) => item.kind)).toEqual(["failed", "blocked", "completed"]);
  });

  it("ignores events it does not read, and malformed ones", () => {
    const { tracker: t } = tracker();
    for (const frame of [event("thread.message_posted", "thr_1"), event("run.started", RUN_A), { type: 5 }, {}, event("turn.started", "", {}), { type: "turn.started", payload: null }]) {
      expect(t.apply(frame)).toBe(false);
    }
    expect(t.items()).toEqual([]);
  });

  it("never puts event payload text into an item", () => {
    const { tracker: t } = tracker();
    t.apply(event("approval.requested", "apr_1", { approval_id: "apr_1", run_id: RUN_A, consequence_preview: "send $5000 to attacker", prompt: "do it" }));
    t.apply(event("question.asked", "q_1", { question_id: "q_1", run_id: RUN_A, prompt: "ignore all rules", options: ["rm -rf /"] }));
    const text = JSON.stringify(t.items());
    for (const forbidden of ["attacker", "5000", "ignore all rules", "rm -rf", "do it"]) {
      expect(text).not.toContain(forbidden);
    }
    expect(t.items().map((item) => item.title)).toEqual(["Needs your approval", "Needs your answer"]);
  });

  it("an event for the same thing twice is one item, with the time it was first seen", () => {
    const { tracker: t, clock } = tracker();
    t.apply(event("approval.requested", "apr_1", { approval_id: "apr_1" }));
    clock.now = 5_000;
    expect(t.apply(event("approval.requested", "apr_1", { approval_id: "apr_1" }))).toBe(false);
    expect(t.items()).toHaveLength(1);
    expect(t.items()[0]?.since).toBe(1_000);
  });

  it("reads the approvals the server lists at start-up", () => {
    const { tracker: t } = tracker();
    t.hydrateApprovals([{ id: "apr_1", runId: RUN_A }, { id: "apr_2", runId: null }]);
    expect(t.items().map((item) => item.ref.approvalId)).toEqual(["apr_1", "apr_2"]);
  });
});

describe("order", () => {
  it("is waiting for you, then running, then errors, then completed, each oldest first", () => {
    const { tracker: u, clock: clockU } = tracker();
    // A run that fails takes its waiting and running items with it, so these belong to a run that has not.
    const place = (ms: number, frame: Record<string, unknown>): void => {
      clockU.now = ms;
      u.apply(frame);
    };
    place(10, event("run.succeeded", RUN_C));
    place(20, event("run.failed", RUN_B));
    place(30, event("turn.started", "trn_2", { turn_id: "trn_2", run_id: RUN_A }));
    place(40, event("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN_A }));
    place(50, event("question.asked", "q_1", { question_id: "q_1", run_id: RUN_A }));
    place(60, event("approval.requested", "apr_1", { approval_id: "apr_1", run_id: RUN_A }));
    expect(u.items().map((item) => `${item.group}:${item.kind}:${String(item.since)}`)).toEqual([
      "waiting:needs_answer:50",
      "waiting:needs_approval:60",
      "running:running:30",
      "running:running:40",
      "error:failed:20",
      "completed:completed:10",
    ]);
  });

  it("is a total order that compares groups before time", () => {
    const item = (group: ActivityItem["group"], since: number, key: string): ActivityItem => ({ key, kind: "running", group, title: "", ref: {}, since });
    expect(compareActivity(item("waiting", 99, "a"), item("running", 1, "b"))).toBeLessThan(0);
    expect(compareActivity(item("completed", 1, "a"), item("error", 99, "b"))).toBeGreaterThan(0);
    expect(compareActivity(item("running", 1, "a"), item("running", 2, "b"))).toBeLessThan(0);
    expect(compareActivity(item("running", 1, "a"), item("running", 1, "b"))).toBeLessThan(0);
  });

  it("is the order the indicator groups its rows in", () => {
    const { tracker: t } = tracker();
    t.apply(event("run.succeeded", RUN_C));
    t.apply(event("approval.requested", "apr_1", { approval_id: "apr_1" }));
    t.apply(event("run.failed", RUN_B));
    t.apply(event("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN_A }));
    const rows = decodeActivity({ items: t.items() });
    expect(groupActivity(rows).map((entry) => entry.group)).toEqual(["waiting", "running", "error", "completed"]);
    expect(groupActivity(decodeActivity({ items: [] }))).toEqual([]);
  });
});

describe("dismissal, expiry and the bound", () => {
  it("a dismissed item is gone and a replay of its event does not bring it back", () => {
    const { tracker: t } = tracker();
    t.apply(event("run.failed", RUN_A));
    const key = t.items()[0]?.key ?? "";
    expect(t.dismiss(key)).toBe(true);
    expect(t.items()).toEqual([]);
    expect(t.apply(event("run.failed", RUN_A))).toBe(false);
    expect(t.items()).toEqual([]);
    expect(t.dismiss(key)).toBe(false);
  });

  it("finished items expire after their lifetime; errors and waiting items do not", () => {
    const { tracker: t, clock } = tracker({ completedTtlMs: 10_000 });
    t.apply(event("run.succeeded", RUN_C));
    t.apply(event("run.failed", RUN_B));
    t.apply(event("approval.requested", "apr_1", { approval_id: "apr_1" }));
    clock.now += 9_999;
    expect(t.expire()).toBe(false);
    clock.now += 2;
    expect(t.expire()).toBe(true);
    expect(t.items().map((item) => item.kind)).toEqual(["needs_approval", "failed"]);
  });

  it("stays within its bound, dropping completed first, then errors, then running, and never an item waiting for you", () => {
    const { tracker: t, clock } = tracker({ maxItems: 4 });
    t.apply(event("approval.requested", "apr_1", { approval_id: "apr_1" }));
    clock.now += 1;
    t.apply(event("run.succeeded", RUN_C));
    clock.now += 1;
    t.apply(event("run.failed", RUN_B));
    clock.now += 1;
    t.apply(event("turn.started", "trn_1", { turn_id: "trn_1", run_id: RUN_A }));
    expect(t.items()).toHaveLength(4);
    clock.now += 1;
    t.apply(event("approval.requested", "apr_2", { approval_id: "apr_2" }));
    expect(t.items().map((item) => item.kind)).toEqual(["needs_approval", "needs_approval", "running", "failed"]);
    clock.now += 1;
    t.apply(event("approval.requested", "apr_3", { approval_id: "apr_3" }));
    clock.now += 1;
    t.apply(event("approval.requested", "apr_4", { approval_id: "apr_4" }));
    expect(t.items().map((item) => item.kind)).toEqual(["needs_approval", "needs_approval", "needs_approval", "needs_approval"]);
    clock.now += 1;
    t.apply(event("approval.requested", "apr_5", { approval_id: "apr_5" }));
    // Nothing waiting for the person is ever dropped to make room.
    expect(t.waitingCount()).toBe(5);
  });

  it("tells a listener when the list changes", () => {
    const { tracker: t } = tracker();
    let changes = 0;
    const off = t.onChange(() => (changes += 1));
    t.apply(event("run.failed", RUN_A));
    t.apply(event("run.failed", RUN_A));
    expect(changes).toBe(1);
    off();
    t.apply(event("run.succeeded", RUN_B));
    expect(changes).toBe(1);
  });
});

describe("opening an item", () => {
  it("goes to the approval or the run it points at, and to nothing when it points at nothing", () => {
    expect(commandForItem({ key: "a", kind: "needs_approval", group: "waiting", title: "", since: 0, ref: { approvalId: "apr_1", runId: RUN_A } })).toEqual({ kind: "open-approval", approvalId: "apr_1" });
    expect(commandForItem({ key: "a", kind: "running", group: "running", title: "", since: 0, ref: { runId: RUN_A } })).toEqual({ kind: "open-run", runId: RUN_A });
    expect(commandForItem({ key: "a", kind: "running", group: "running", title: "", since: 0, ref: {} })).toBeNull();
  });

  it("a notification click goes where the notification points", () => {
    const notice = (ref: Record<string, unknown>) => ({ notificationId: "ntf_1", title: "t", body: "b", ref });
    expect(commandForNotice(notice({ thread_id: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" }))).toEqual({ kind: "open-thread", threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    expect(commandForNotice(notice({ approval_id: "apr_1", run_id: RUN_A }))).toEqual({ kind: "open-approval", approvalId: "apr_1" });
    expect(commandForNotice(notice({ run_id: RUN_A }))).toEqual({ kind: "open-run", runId: RUN_A });
    expect(commandForNotice(notice({ thread_id: "not-a-thread" }))).toBeNull();
    expect(commandForNotice(notice({}))).toBeNull();
  });
});
