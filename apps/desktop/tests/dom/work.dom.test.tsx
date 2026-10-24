// @vitest-environment jsdom
/** APP-005 on a page: the Work view over the real preload API, answering from a table of graph projections. */

import { afterEach, describe, expect, it } from "vitest";

import { ApiError } from "../../src/client/server.js";
import { Work } from "../../src/renderer/Work.js";
import { choose, click, mount, text, type, type Rig } from "./rig.js";

const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const OBJ = "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA0";
const A = "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA1";
const B = "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA2";

function node(id: string, kind: string, title: string, status: string, parent: string | null, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id, kind, title, status, parent_id: parent, created_by: {}, completion_contract: {}, priority: 1, revision: 2, origin: "user", run: null, open_questions: [], ...extra,
  };
}

function wire(revision: number, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    workspace_id: WORKSPACE,
    revision,
    nodes: [
      node(OBJ, "objective", "Ship", "in_progress", null, {
        completion_contract: { deterministic_checks: [{ kind: "artifact_exists" }], semantic_verification: { required: true, rubric_id: "rub_1" }, human_signoff_required: true },
      }),
      node(A, "task", "gather", "done", OBJ, { run: { id: "run_a", agent_thread_id: "ath_1", status: "SUCCEEDED", generation: 1 } }),
      node(B, "task", "write", "ready", OBJ, { revision: 3, run: { id: "run_b", agent_thread_id: "ath_1", status: "WAITING_CHILD", generation: 1 } }),
    ],
    edges: [{ id: "we_1", from_node_id: B, to_node_id: A, kind: "depends_on", revision: 3 }],
    agents: [
      { id: "ath_1", kind: "teammate", parent_id: null, work_node_id: OBJ, status: "ACTIVE" },
      { id: "ath_2", kind: "worker", parent_id: "ath_1", work_node_id: B, status: "ACTIVE" },
    ],
    handoffs: [{ id: "age_1", parent_agent_thread_id: "ath_1", child_agent_thread_id: "ath_2", work_node_id: B, delegated_at: "2026-09-20T00:00:00Z", joined_at: null }],
    blockers: [
      { node_id: B, kind: "run", ref: "run_b", status: "WAITING_CHILD", question: { question_id: "q_1", prompt: "which branch?" } },
      { node_id: B, kind: "prerequisite", ref: A, status: null, question: null },
    ],
    ...extra,
  };
}

const conflictError = new ApiError(409, { code: "CONFLICT_REVISION", message: "moved", correlation_id: "c", retryable: false });

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

describe("the Work view", () => {
  it("lays out the plan tree, dependencies, blockers, agents, handoffs and a node's contract", async () => {
    const inspected: string[] = [];
    rig = await mount(<Work workspaceId={WORKSPACE} onInspectRun={(id) => { inspected.push(id); }} />, { "work.graph": wire(3) });
    const page = text(rig);
    expect(rig.container.querySelector("[data-revision='3']")).not.toBeNull();
    expect(page).toContain("gather");
    expect(rig.container.querySelector(`[data-node='${B}'] .deps`)?.textContent).toContain("gather");
    expect(rig.container.querySelector(".blocker-run")?.textContent).toContain("which branch?");
    expect(rig.container.querySelector(".blocker-prerequisite")?.textContent).toContain("gather");
    expect(rig.container.querySelector("[data-agent=ath_2]")?.textContent).toContain("write");
    expect(rig.container.querySelector("[data-handoff=age_1]")?.textContent).toContain("in flight");
    await click(rig, "WAITING_CHILD");
    expect(inspected).toEqual(["run_b"]);
    await click(rig, "Ship");
    const detail = rig.container.querySelector(".detail")?.textContent ?? "";
    expect(detail).toContain("1");
    expect(detail).toContain("semantic");
  });

  it("shows the loading notice when the graph cannot be read", async () => {
    rig = await mount(<Work workspaceId={WORKSPACE} />, { "work.graph": () => Promise.reject(new Error("graph down")) });
    expect(text(rig)).toContain("graph down");
  });

  it("collects intents into a draft and proposes them against the base revision", async () => {
    rig = await mount(<Work workspaceId={WORKSPACE} />, { "work.graph": wire(3), "work.propose": { ok: true } });
    await click(rig, "write");
    await click(rig, "→ cancelled");
    expect(rig.container.querySelector(".draft")?.getAttribute("data-base-revision")).toBe("3");
    await click(rig, "Propose");
    const proposed = rig.calls.filter((call) => call.channel === "work.propose");
    expect(proposed).toHaveLength(1);
    expect(JSON.stringify(proposed[0]?.payload)).toContain(B);
    expect(rig.container.querySelector(".draft")).toBeNull();
  });

  it("removes and discards draft intents", async () => {
    rig = await mount(<Work workspaceId={WORKSPACE} />, { "work.graph": wire(3) });
    await click(rig, "write");
    await click(rig, "→ cancelled");
    await click(rig, "Remove");
    expect(rig.container.querySelector(".draft")).toBeNull();
    await click(rig, "→ cancelled");
    await click(rig, "Discard");
    expect(rig.container.querySelector(".draft")).toBeNull();
    expect(rig.calls.some((call) => call.channel === "work.propose")).toBe(false);
  });

  it("drafts a new node under the selected one and proposes it", async () => {
    let graphReads = 0;
    rig = await mount(<Work workspaceId={WORKSPACE} />, {
      "work.graph": () => {
        graphReads += 1;
        return graphReads > 1
          ? wire(4, { nodes: [...(wire(3)["nodes"] as unknown[]), node("wn_01J8Z3K6F1N8VQ2X5W9Y0AAAA3", "task", "review", "draft", B)] })
          : wire(3);
      },
      "work.propose": { ok: true },
    });
    await click(rig, "write");
    await type(rig, "New node title", "review");
    await click(rig, "Add under");
    expect(rig.container.querySelector(".draft")?.textContent).toContain("review");
    const draftButtons = rig.container.querySelectorAll(".draft li");
    expect(draftButtons.length).toBe(1);
    await click(rig, "Propose");
    expect(rig.calls.filter((call) => call.channel === "work.propose")).toHaveLength(1);
    expect(rig.container.querySelector(".draft")).toBeNull();
  });

  it("adds a dependency edge between existing nodes and refuses an empty choice", async () => {
    rig = await mount(<Work workspaceId={WORKSPACE} />, { "work.graph": wire(3), "work.propose": { ok: true } });
    await click(rig, "Ship");
    const add = [...rig.container.querySelectorAll<HTMLButtonElement>(".add-edge button")][0];
    expect(add?.disabled).toBe(true);
    await choose(rig, ".add-edge select", A);
    await click(rig, "Add dependency");
    expect(rig.container.querySelector(".draft")?.textContent).toContain(`${OBJ} depends_on ${A}`);
  });

  it("creates an objective and reloads the graph", async () => {
    rig = await mount(<Work workspaceId={WORKSPACE} />, { "work.graph": wire(3), "work.objective": { ok: true } });
    const reads = (): number => rig.calls.filter((call) => call.channel === "work.graph").length;
    const before = reads();
    await type(rig, "New objective", "Launch");
    await click(rig, "Create objective");
    expect(rig.calls.some((call) => JSON.stringify(call.payload).includes("Launch"))).toBe(true);
    expect(reads()).toBeGreaterThan(before);
  });

  it("shows a refusal as a notice and keeps the draft", async () => {
    rig = await mount(<Work workspaceId={WORKSPACE} />, { "work.graph": wire(3), "work.propose": () => Promise.reject(new Error("refused by policy")) });
    await click(rig, "write");
    await click(rig, "→ cancelled");
    await click(rig, "Propose");
    expect(rig.container.querySelector(".notice")?.textContent).toContain("refused by policy");
    expect(rig.container.querySelector(".draft")).not.toBeNull();
  });

  it("shows a conflict when the server moved, lists dropped intents, and rebases or discards", async () => {
    let moved = false;
    rig = await mount(<Work workspaceId={WORKSPACE} />, {
      "work.graph": () => (moved ? wire(5, { nodes: [node(OBJ, "objective", "Ship", "in_progress", null), node(A, "task", "gather", "done", OBJ)] }) : wire(3)),
      "work.propose": () => Promise.reject(conflictError),
    });
    await click(rig, "write");
    await click(rig, "→ cancelled");
    moved = true;
    await click(rig, "Propose");
    const dialog = rig.container.querySelector("[role=alertdialog]");
    expect(dialog?.getAttribute("data-server-revision")).toBe("5");
    expect(dialog?.textContent).toContain("no longer exists");
    await click(rig, "Keep rebased draft");
    expect(rig.container.querySelector("[role=alertdialog]")).toBeNull();
    expect(rig.container.querySelector(".draft")).toBeNull();
  });

  it("discards a draft from the conflict dialog", async () => {
    let moved = false;
    rig = await mount(<Work workspaceId={WORKSPACE} />, {
      "work.graph": () => (moved ? wire(4) : wire(3)),
      "work.propose": () => Promise.reject(conflictError),
    });
    await click(rig, "write");
    await click(rig, "→ cancelled");
    moved = true;
    await click(rig, "Propose");
    expect(rig.container.querySelector("[role=alertdialog]")).not.toBeNull();
    await click(rig, "Keep rebased draft");
    expect(rig.container.querySelector(".draft")).not.toBeNull();
    await click(rig, "Propose");
    await click(rig, /Discard/);
    expect(rig.container.querySelector(".draft")).toBeNull();
  });

  it("reloads when a work event arrives on the stream and ignores unrelated frames", async () => {
    rig = await mount(<Work workspaceId={WORKSPACE} />, { "work.graph": wire(3) });
    const reads = (): number => rig.calls.filter((call) => call.channel === "work.graph").length;
    const before = reads();
    await rig.push("stream.frame", { kind: "event", cursor: "c", event: { type: "thread.message_posted", workspace_id: WORKSPACE, payload: {} } });
    expect(reads()).toBe(before);
    await rig.push("stream.frame", "not a frame");
    expect(reads()).toBe(before);
    await rig.push("stream.frame", { kind: "event", cursor: "c", event: { type: "work.node_changed", workspace_id: WORKSPACE, payload: {} } });
    expect(reads()).toBeGreaterThan(before);
  });
});
