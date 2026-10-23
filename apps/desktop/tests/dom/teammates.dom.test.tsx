// @vitest-environment jsdom
/** APP-014 on a page: the Teammates view over the real preload API — roster cards, create / edit / archive, refusals. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { Teammates } from "../../src/renderer/Teammates.js";
import { click, mount, text, type, type Rig } from "./rig.js";

const ATLAS = "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const OLD = "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";

function row(id: string, name: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, workspace_id: "ws_1", name, persona: "careful reviewer", standing_instructions: "be brief", default_capability_needs: ["read.internal", "email.send"],
    memory_scope: "workspace", status: "active", availability: "available", version: 3, template_id: "tpl_1", model_route_preference: "fast",
    thread: { id: "thr_1", status: "ACTIVE", generation: 1, runs: 2 },
    active_run: { id: "run_1", status: "RUNNING" },
    attention: { open_questions: 1, pending_approvals: 2, needs_human: true },
    effective_capability: { grants: [{ effect_class: "read.internal", resource: { kind: "fs", selector: "**" } }], policy_grants: 4, narrowed_by_needs: true, needs_not_granted: ["email.send"] },
    ...extra,
  };
}

const roster = {
  workspace_id: "ws_1",
  teammates: [
    row(OLD, "Old", { status: "archived", thread: { id: "thr_2", status: "SUSPENDED", suspended_reason: "teammate archived", runs: 0 }, active_run: null, attention: {} }),
    row(ATLAS, "Atlas"),
  ],
  templates: [{ id: "tpl_1", name: "Reviewer", persona: "templated persona", standing_instructions: "templated rules", default_capability_needs: ["read.internal"], memory_scope: "teammate" }],
};

const ok = { ok: true, result: {} };
let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

async function choose(selector: string, value: string): Promise<void> {
  const select = rig.container.querySelector<HTMLSelectElement>(selector);
  if (!select) {
    throw new Error(`no select ${selector}`);
  }
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(select, value);
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await Promise.resolve();
  });
  await rig.flush();
}

describe("the Teammates view", () => {
  it("shows active teammates first with thread, run, capability narrowing and attention", async () => {
    const inspected: string[] = [];
    rig = await mount(<Teammates workspaceId="ws_1" onInspectRun={(id) => { inspected.push(id); }} />, { "teammates.list": roster });
    const cards = [...rig.container.querySelectorAll("[data-teammate]")];
    expect(cards.map((card) => card.getAttribute("data-teammate"))).toEqual([ATLAS, OLD]);
    const atlas = cards[0]?.textContent ?? "";
    expect(atlas).toContain("thr_1 · ACTIVE");
    expect(atlas).toContain("1 of 4 policy grants");
    expect(atlas).toContain("not granted: email.send");
    expect(atlas).toContain("1 question(s), 2 approval(s)");
    expect(cards[1]?.getAttribute("data-thread-status")).toBe("SUSPENDED");
    expect(cards[1]?.textContent).toContain("teammate archived");
    expect(cards[1]?.querySelector(".actions")).toBeNull();
    expect(rig.container.querySelector(".attention")?.textContent).toContain("Atlas");
    await click(rig, "run_1 · RUNNING");
    expect(inspected).toEqual(["run_1"]);
  });

  it("archives an active teammate through the command, and shows the server's refusal", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster, "teammates.archive": ok });
    await click(rig, /^Archive$/);
    expect(sent("teammates.archive")[0]?.payload).toEqual({ teammateId: ATLAS });
    expect(rig.container.querySelector(".notice")?.textContent).toContain("nothing was deleted");
    rig.answer("teammates.archive", { ok: false, error: { code: "CONFLICT_STATE", message: "already" } });
    await click(rig, /^Archive$/);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("Refused — CONFLICT_STATE: already");
  });

  it("edits a teammate: the form starts from the card and sends the changes", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster, "teammates.update": ok });
    await click(rig, /^Edit$/);
    expect((rig.container.querySelector("input[aria-label=name]") as HTMLInputElement).value).toBe("Atlas");
    await type(rig, "name", "Atlas 2");
    await type(rig, "Persona", "sharper");
    await type(rig, "Capability needs", "read.internal");
    await choose(".editor label select", "teammate");
    await click(rig, /^Saved/);
    const params = sent("teammates.update")[0]?.payload as Record<string, unknown>;
    expect(params).toMatchObject({ teammate_id: ATLAS, name: "Atlas 2", persona: "sharper", memory_scope: "teammate", default_capability_needs: ["read.internal"] });
    expect(rig.container.querySelector(".editor")).toBeNull();
  });

  it("refuses an invalid edit on the page before sending anything", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster, "teammates.update": ok });
    await click(rig, /^Edit$/);
    await type(rig, "Capability needs", "Not An Effect");
    await click(rig, /^Saved/);
    expect(sent("teammates.update")).toHaveLength(0);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("VALIDATION_SCHEMA");
    await click(rig, "Cancel");
    expect(rig.container.querySelector(".editor")).toBeNull();
  });

  it("keeps the editor open when the server refuses a save", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster, "teammates.update": { ok: false, error: { code: "CONFLICT_VERSION", message: "stale" } } });
    await click(rig, /^Edit$/);
    await click(rig, /^Saved/);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("CONFLICT_VERSION");
    expect(rig.container.querySelector(".editor")).not.toBeNull();
  });

  it("creates a teammate from a template, filling the draft from it", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster, "teammates.create": ok });
    await click(rig, "New teammate");
    await choose(".editor label select", "tpl_1");
    expect((rig.container.querySelector("input[aria-label=name]") as HTMLInputElement).value).toBe("Reviewer");
    expect((rig.container.querySelector("textarea[aria-label=Persona]") as HTMLTextAreaElement).value).toBe("templated persona");
    await click(rig, /New teammate/);
    expect(sent("teammates.create")[0]?.payload).toMatchObject({ workspace_id: "ws_1", template_id: "tpl_1", name: "Reviewer", memory_scope: "teammate", default_capability_needs: ["read.internal"] });
    expect(rig.container.querySelector(".notice")?.textContent).toContain("Created");
    expect(rig.container.querySelector(".editor")).toBeNull();
  });

  it("refuses to create a nameless teammate without a template", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster, "teammates.create": ok });
    await click(rig, "New teammate");
    await click(rig, /New teammate/);
    expect(sent("teammates.create")).toHaveLength(0);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("needs a template or a name");
    await click(rig, "Cancel");
    expect(rig.container.querySelector(".editor")).toBeNull();
  });

  it("shows a server refusal of a create and keeps the form", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster, "teammates.create": { ok: false, error: { code: "PERMISSION_DENIED", message: "admins only" } } });
    await click(rig, "New teammate");
    await type(rig, "name", "Nova");
    await click(rig, /New teammate/);
    expect(rig.container.querySelector(".notice")?.textContent).toContain("PERMISSION_DENIED: admins only");
    expect(rig.container.querySelector(".editor")).not.toBeNull();
  });

  it("reloads on agent and run events only, and reports an unreadable roster", async () => {
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": roster });
    const before = sent("teammates.list").length;
    await rig.push("stream.frame", { event: { type: "thread.message_posted" } });
    await rig.push("stream.frame", 7);
    expect(sent("teammates.list").length).toBe(before);
    await rig.push("stream.frame", { event: { type: "agent.updated" } });
    expect(sent("teammates.list").length).toBeGreaterThan(before);
    await rig.unmount();
    rig = await mount(<Teammates workspaceId="ws_1" />, { "teammates.list": () => Promise.reject(new Error("roster down")) });
    expect(text(rig)).toContain("roster down");
  });
});
