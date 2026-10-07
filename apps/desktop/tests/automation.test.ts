/**
 * APP-010 unit tests: routine drafts and commands, the fire summary that makes a duplicate
 * visible, the notification center kept current from user-scoped events, preference
 * toggles, and the native-notification decision.
 */

import { describe, expect, it } from "vitest";

import {
  applyNotificationEvent,
  createRoutineParams,
  decodeNotifications,
  decodeRoutines,
  desktopNotice,
  draftProblem,
  inAppItems,
  preferenceParams,
  routineControls,
  summarizeFires,
  toggleChannel,
  type FireView,
} from "../src/index.js";

const ME = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";

function fire(overrides: Partial<FireView>): FireView {
  return { id: "rfi_1", fireKey: "routine:rtn_1:2026-09-23T10:00:00+00:00", source: "schedule", status: "fired", attempts: 1, windowAt: "2026-09-23T10:00:00Z", runId: "run_1", threadId: "thr_1", lastError: null, payloadDigest: null, ...overrides };
}

describe("routines", () => {
  it("checks a draft before sending it", () => {
    const base = { title: "Digest", prompt: "summarize", trigger: { kind: "interval" as const, spec: "30m" } };
    expect(draftProblem(base)).toBeNull();
    expect(draftProblem({ ...base, title: " " })).toMatch(/title/);
    expect(draftProblem({ ...base, prompt: "" })).toMatch(/prompt/);
    expect(draftProblem({ ...base, trigger: { kind: "interval", spec: "soon" } })).toMatch(/interval/);
    expect(draftProblem({ ...base, trigger: { kind: "cron", spec: "0 9 * *" } })).toMatch(/five fields/);
    expect(draftProblem({ ...base, trigger: { kind: "cron", spec: "0 9 * * 1-5" } })).toBeNull();
    expect(draftProblem({ ...base, trigger: { kind: "event", spec: "artifact.created" } })).toBeNull();
    expect(draftProblem({ ...base, trigger: { kind: "event", spec: "thread.*" } })).toBeNull();
    expect(draftProblem({ ...base, trigger: { kind: "event", spec: "routine.fired" } })).toMatch(/cannot listen/);
    expect(createRoutineParams("ws_1", { ...base, absencePolicy: "catch_up_once" })).toEqual({
      workspace_id: "ws_1",
      title: "Digest",
      prompt: "summarize",
      trigger: { kind: "interval", spec: "30m", timezone: "UTC" },
      absence_policy: "catch_up_once",
    });
    expect(() => createRoutineParams("ws_1", { ...base, title: "" })).toThrow();
  });

  it("decodes the projection and offers the controls its state allows", () => {
    const [routine] = decodeRoutines({
      items: [
        {
          id: "rtn_1",
          objective_template: { title: "Digest", prompt: "summarize" },
          trigger: { kind: "cron", spec: "0 9 * * *", timezone: "+02:00" },
          absence_policy: "queue",
          status: "paused",
          owner_user_id: ME,
          next_due_at: "2026-09-24T07:00:00Z",
          fires: [{ id: "rfi_1", fire_key: "k", source: "manual", status: "fired", attempts: 1, run_id: "run_1" }],
        },
      ],
    });
    expect(routine?.trigger).toEqual({ kind: "cron", spec: "0 9 * * *", timezone: "+02:00" });
    expect(routine?.absencePolicy).toBe("queue");
    expect(routine?.fires[0]?.source).toBe("manual");
    expect(routine ? routineControls(routine) : []).toEqual(["ResumeRoutine", "DeleteRoutine"]);
    expect(routine ? routineControls({ ...routine, status: "active" }) : []).toEqual(["TriggerRoutineNow", "PauseRoutine", "DeleteRoutine"]);
    expect(routine ? routineControls({ ...routine, status: "deleted" }) : ["x"]).toEqual([]);
  });

  it("summarizes fires and makes a duplicate window or run visible", () => {
    const clean = [fire({}), fire({ id: "rfi_2", windowAt: "2026-09-23T11:00:00Z", runId: "run_2" }), fire({ id: "rfi_3", status: "pending", runId: null, windowAt: "2026-09-23T12:00:00Z" })];
    expect(summarizeFires(clean)).toEqual({ fired: 2, pending: 1, failed: 0, duplicateRuns: 0, duplicateWindows: 0 });
    const twice = [...clean, fire({ id: "rfi_4", runId: "run_1" })];
    expect(summarizeFires(twice).duplicateRuns).toBe(1);
    expect(summarizeFires(twice).duplicateWindows).toBe(1);
  });
});

describe("notifications", () => {
  const center = decodeNotifications({
    items: [
      { id: "ntf_1", kind: "mention", title: "You were mentioned in launch", ref: { thread_id: "thr_1" }, channels: ["in_app", "desktop"], channels_delivered: ["in_app", "desktop"], read_at: null },
      { id: "ntf_2", kind: "completed", title: "Routine ran", channels: ["email"], channels_delivered: [], read_at: null },
    ],
    preferences: [{ kind: "mention", channels: ["in_app", "desktop"], chosen: true }],
  });

  it("decodes the center; the in-app list shows only what the person chose to see in the app", () => {
    expect(center.unread).toBe(2);
    expect(inAppItems(center).map((item) => item.id)).toEqual(["ntf_1"]);
    expect(center.preferences[0]).toEqual({ kind: "mention", channels: ["in_app", "desktop"], chosen: true });
  });

  it("applies the person's own events and ignores anyone else's", () => {
    const created = applyNotificationEvent(center, { type: "notification.created", payload: { notification_id: "ntf_3", user_id: ME, kind: "needs_answer", title: "A question needs your answer", channels: ["in_app"] } }, ME);
    expect(created.items[0]?.id).toBe("ntf_3");
    expect(created.unread).toBe(3);
    expect(applyNotificationEvent(created, { type: "notification.created", payload: { notification_id: "ntf_3", user_id: ME } }, ME)).toBe(created);
    expect(applyNotificationEvent(center, { type: "notification.created", payload: { notification_id: "ntf_9", user_id: "usr_other" } }, ME)).toBe(center);
    const read = applyNotificationEvent(created, { type: "notification.read", payload: { notification_id: "ntf_1", user_id: ME } }, ME);
    expect(read.unread).toBe(2);
    expect(read.items.find((item) => item.id === "ntf_1")?.readAt).not.toBeNull();
  });

  it("builds preferences from toggles and refuses unknown kinds and channels", () => {
    expect(toggleChannel(["in_app"], "email", true)).toEqual(["in_app", "email"]);
    expect(toggleChannel(["email", "in_app", "desktop"], "desktop", false)).toEqual(["in_app", "email"]);
    expect(toggleChannel([], "desktop", true)).toEqual(["desktop"]);
    expect(preferenceParams("ws_1", "mention", ["in_app", "in_app"])).toEqual({ workspace_id: "ws_1", kind: "mention", channels: ["in_app"] });
    expect(() => preferenceParams("ws_1", "gossip", [])).toThrow();
    expect(() => preferenceParams("ws_1", "mention", ["sms"])).toThrow();
  });

  it("asks for a native notification only when the event selects the desktop channel", () => {
    expect(desktopNotice({ type: "notification.created", payload: { notification_id: "ntf_1", desktop: true, title: "You were mentioned", kind: "mention" } })).toEqual({ notificationId: "ntf_1", title: "You were mentioned", body: "mention", ref: {} });
    expect(desktopNotice({ type: "notification.created", payload: { notification_id: "ntf_1", desktop: false } })).toBeNull();
    expect(desktopNotice({ type: "notification.read", payload: { notification_id: "ntf_1", desktop: true } })).toBeNull();
  });
});
