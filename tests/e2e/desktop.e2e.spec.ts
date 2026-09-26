/**
 * QA-008 desktop journeys: the real Electron app, driven through its rendered UI, against the real
 * server (`harness.ts`). Each journey is a person's workflow end to end — the screens, the keyboard,
 * and what the server records afterwards — not a module in isolation.
 *
 * Support matrix (DOSSIER.md §21.1): macOS 14+ runs here; Windows 11 needs its own host and is
 * reported `BLOCKED_EXTERNAL`.
 */

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  APPROVER,
  auditAccessibility,
  desktopBlocked,
  emailOf,
  launchDesktop,
  OWNER,
  signInDesktop,
  Stack,
  waitFor,
  walkKeyboard,
  type Desktop,
} from "./harness.js";

const blocked = desktopBlocked();

describe.skipIf(blocked !== null)("desktop journeys (real Electron, real server)", () => {
  let stack: Stack;
  let desktop: Desktop;

  beforeAll(async () => {
    stack = await Stack.start("desktop");
    desktop = await launchDesktop(stack.origin);
    // Every journey below starts from the seeded owner signed in, so each stands on its own.
    await signInDesktop(desktop.page, stack, emailOf(OWNER));
  }, 180_000);

  afterAll(async () => {
    await desktop.close();
    await stack.drop();
  }, 60_000);

  const nav = (name: string) => desktop.page.getByRole("navigation", { name: "Surfaces" }).getByRole("button", { name, exact: true });

  it("onboards a new person with nothing but an email address", async () => {
    const fresh = await launchDesktop(stack.origin);
    try {
      const page = fresh.page;
      await signInDesktop(page, stack, "qa008-newcomer@example.com", "new");
      // A first sign-in provisions a personal workspace, then asks where work runs and confirms the
      // security defaults — each choice written to the workspace through the server.
      await page.getByRole("heading", { name: "Where should your work run?" }).waitFor({ timeout: 30_000 });
      await page.getByRole("button", { name: "cloud" }).click();
      await page.getByRole("heading", { name: "Security defaults" }).waitFor({ timeout: 30_000 });
      await page.getByRole("button", { name: "Confirm" }).click();
      await page.getByRole("navigation", { name: "Surfaces" }).waitFor({ state: "visible", timeout: 30_000 });
      await waitFor(async () => (await page.getByText("Connected.").count()) > 0, "the live connection");
      const session = await stack.session("qa008-newcomer@example.com");
      const me = await stack.get(session.access_token, "/v1/me");
      const workspace = (me["workspaces"] as { effective_settings: Record<string, unknown> }[])[0];
      expect(JSON.stringify(workspace?.effective_settings)).toContain("cloud");
    } finally {
      await fresh.close();
    }
  }, 120_000);

  it("chats with a teammate", async () => {
    const page = desktop.page;
    await nav("Chat").click();
    const composer = page.getByRole("textbox", { name: "Message your teammates…" });
    await composer.fill("Summarize the launch checklist");
    await page.getByRole("button", { name: "Send" }).click();
    const conversation = page.getByRole("region", { name: "Conversation" });
    await waitFor(async () => (await conversation.getByText("Summarize the launch checklist").count()) > 0, "the message in the thread");
    await waitFor(async () => (await conversation.getByRole("listitem").count()) >= 2, "a teammate's reply");
    console.log(await conversation.ariaSnapshot());
  }, 120_000);

  it("approves an effect from the preview it was asked about", async () => {
    const page = desktop.page;
    await nav("Chat").click();
    await page.getByRole("textbox", { name: "Message your teammates…" }).fill("write the summary [[scenario:approval]]");
    await page.getByRole("button", { name: "Send" }).click();
    await nav("Approvals").click();
    const trust = page.getByRole("region", { name: "Trust" });
    await waitFor(async () => (await trust.getByRole("heading", { name: "Approvals (1)" }).count()) > 0, "the approval request", 30_000);
    await trust.getByRole("button", { name: /^Approve fs\.write/ }).click();
    const dialog = trust.getByRole("dialog");
    await dialog.waitFor();
    // The preview names the exact payload the approval is bound to, and the server agrees.
    const shown = (await dialog.locator("code").first().textContent())?.trim() ?? "";
    const owner = await stack.session(emailOf(OWNER));
    const pending = (await stack.get(owner.access_token, `/v1/approvals?workspace_id=${owner.workspace_id}&status=requested`))["approvals"] as { params_digest: string; run_id: string }[];
    expect(pending.map((item) => item.params_digest)).toContain(shown);
    await expect(dialog.getByText("untrusted origin", { exact: false }).count()).resolves.toBeGreaterThan(0);
    await dialog.getByRole("button", { name: "Approve", exact: true }).click();
    await waitFor(async () => (await trust.getByRole("heading", { name: "Approvals (0)" }).count()) > 0, "the request to leave the queue", 30_000);
    await waitFor(async () => (await trust.getByText("fs.write.host · SETTLED_SUCCESS").count()) > 0, "the effect to settle", 30_000);
    const run = pending.find((item) => item.params_digest === shown)?.run_id ?? "";
    const decided = (await stack.get(owner.access_token, `/v1/approvals?run_id=${run}`))["approvals"] as { status: string; receipts: { consumed: boolean }[] }[];
    expect(decided[0]?.status).toBe("granted");
    expect(decided[0]?.receipts[0]?.consumed).toBe(true);
  }, 120_000);

  it("steers the plan: adds a task under the objective and moves it to ready", async () => {
    const page = desktop.page;
    await nav("Objectives and plan").click();
    const work = page.getByRole("region", { name: "Work" });
    await work.getByRole("button", { name: /objective e2e seed/ }).click();
    await work.getByRole("textbox", { name: "New node title…" }).fill("Draft the release notes");
    await work.getByRole("button", { name: "Add under" }).click();
    // Edits are drafted against the current revision, then proposed as one plan change.
    await waitFor(async () => (await work.getByText('add task "Draft the release notes"', { exact: false }).count()) > 0, "the drafted edit", 30_000);
    await work.getByRole("button", { name: "Propose" }).click();
    await waitFor(async () => (await work.getByRole("button", { name: /Draft the release notes/ }).count()) > 0, "the new task in the plan", 30_000);
    await waitFor(async () => (await work.getByText(/revision [2-9]/).count()) > 0, "a new plan revision", 30_000);
    const owner = await stack.session(emailOf(OWNER));
    const graph = JSON.stringify(await stack.get(owner.access_token, `/v1/workgraph?workspace_id=${owner.workspace_id}`));
    expect(graph).toContain("Draft the release notes");
  }, 120_000);

  it("takes over the live browser and hands it back", async () => {
    const page = desktop.page;
    await nav("Live browser and terminal").click();
    const live = page.getByRole("region", { name: "Live browser and terminal" });
    await live.getByRole("button", { name: "https://example.com/" }).click();
    await live.getByRole("button", { name: "Take over" }).first().click();
    await waitFor(async () => (await live.getByRole("button", { name: /hand back/i }).count()) > 0, "control to pass to the person", 30_000);
    const owner = await stack.session(emailOf(OWNER));
    const held = JSON.stringify(await stack.get(owner.access_token, "/v1/browser-sessions/bsn_01J8Z3K6F1N8VQ2X5W9Y0E2BS1"));
    expect(held).toMatch(/"(controller|control|held_by)":\s*"user"|user/);
    await live.getByRole("button", { name: /hand back/i }).first().click();
    await waitFor(async () => (await live.getByText("Controlled by agent", { exact: false }).count()) > 0, "control to return to the agent", 30_000);
  }, 120_000);

  it("reviews an artifact's versions and preview", async () => {
    const page = desktop.page;
    const owner = await stack.session(emailOf(OWNER));
    await stack.command(owner.access_token, "CreateArtifact", {
      workspace_id: owner.workspace_id,
      title: "Launch plan.md",
      media_type: "text/markdown",
      bytes_base64: Buffer.from("# Launch plan\n\n- ship the desktop\n").toString("base64"),
    });
    await nav("Files and artifacts").click();
    const artifacts = page.getByRole("region", { name: "Artifacts" });
    await waitFor(async () => (await artifacts.getByRole("button", { name: /Launch plan\.md/ }).count()) > 0, "the artifact in the list", 30_000);
    await artifacts.getByRole("button", { name: /Launch plan\.md/ }).click();
    await waitFor(async () => (await artifacts.getByText("ship the desktop", { exact: false }).count()) > 0, "the preview", 30_000);
    console.log(await artifacts.ariaSnapshot());
  }, 120_000);

  it("starts a group thread with a colleague", async () => {
    const page = desktop.page;
    await nav("Group threads").click();
    const collaboration = page.getByRole("region", { name: "Collaboration" });
    await collaboration.getByRole("textbox", { name: "Title" }).fill("Launch war room");
    await collaboration.getByRole("textbox", { name: "People (usr_… ids, comma separated)" }).fill(APPROVER);
    await collaboration.getByRole("button", { name: "Create" }).click();
    await waitFor(async () => (await collaboration.getByRole("button", { name: /Launch war room/ }).count()) > 0, "the new thread", 30_000);
    const approver = await stack.session(emailOf(APPROVER));
    const threads = JSON.stringify(await stack.get(approver.access_token, `/v1/threads?workspace_id=${approver.workspace_id}`));
    expect(threads).toContain("Launch war room");
  }, 120_000);

  it("creates a routine", async () => {
    const page = desktop.page;
    await nav("Routines and notifications").click();
    const automations = page.getByRole("region", { name: "Automations" });
    await automations.getByRole("textbox", { name: "Title" }).fill("Morning digest");
    await automations.getByRole("textbox", { name: "What should the teammate do?" }).fill("Summarize yesterday's runs");
    await automations.getByRole("button", { name: "Create routine" }).click();
    await waitFor(async () => (await automations.getByText("Morning digest").count()) > 0, "the routine in the list", 30_000);
    const owner = await stack.session(emailOf(OWNER));
    expect(JSON.stringify(await stack.get(owner.access_token, `/v1/routines?workspace_id=${owner.workspace_id}`))).toContain("Morning digest");
  }, 120_000);

  it("changes settings and sees what the change sets in motion", async () => {
    const page = desktop.page;
    await page.getByRole("button", { name: "Settings" }).click();
    const settings = page.getByRole("region", { name: "Settings" });
    await settings.getByLabel("Where work runs").selectOption("private_worker");
    await settings.getByRole("button", { name: "Save workspace settings" }).click();
    await waitFor(async () => (await settings.getByRole("status").textContent())?.includes("re-provisioned") ?? false, "the restart the change causes");
    const owner = await stack.session(emailOf(OWNER));
    const me = await stack.get(owner.access_token, "/v1/me");
    const workspace = (me["workspaces"] as { id: string; effective_settings: Record<string, unknown> }[]).find((item) => item.id === owner.workspace_id);
    expect(workspace?.effective_settings["execution_target"]).toBe("private_worker");
  }, 120_000);

  it("returns a person whose sessions were revoked to sign-in", async () => {
    const colleague = await launchDesktop(stack.origin);
    try {
      await signInDesktop(colleague.page, stack, emailOf(APPROVER));
      const owner = await stack.session(emailOf(OWNER));
      await stack.command(owner.access_token, "RevokeUserSessions", { user_id: APPROVER });
      try {
        await waitFor(async () => (await colleague.page.getByRole("heading", { name: "Sign in" }).count()) > 0, "the revoked person to see sign-in", 30_000);
      } catch (error) {
        console.log("REVOKED UI:\n" + (await colleague.page.locator("body").ariaSnapshot()).slice(0, 800));
        console.log(stack.log.join("").split("\n").filter((line) => /refresh|stream"|\/v1\/stream/.test(line)).slice(-8).join("\n"));
        throw error;
      }
    } finally {
      await colleague.close();
    }
  }, 120_000);

  it("has no accessibility blockers on any surface and every control is keyboard reachable", async () => {
    const page = desktop.page;
    const surfaces = ["Chat", "Objectives and plan", "Teammates", "Approvals", "Live browser and terminal", "Files and artifacts", "Knowledge, memory and skills", "Group threads", "Routines and notifications"];
    const failures: string[] = [];
    for (const surface of [...surfaces, "Settings"]) {
      await (surface === "Settings" ? page.getByRole("button", { name: "Settings" }) : nav(surface)).click();
      await page.waitForTimeout(500);
      const audit = await auditAccessibility(page);
      failures.push(...audit.violations.map((violation) => `${surface}: ${violation}`));
      const keyboard = await walkKeyboard(page);
      failures.push(...keyboard.unreachable.map((control) => `${surface}: not reachable by keyboard: ${control}`));
      failures.push(...keyboard.invisibleFocus.map((control) => `${surface}: focus not visible on ${control}`));
    }
    expect(failures).toEqual([]);
  }, 300_000);

  it("reports the server going away and recovers when it returns", async () => {
    const page = desktop.page;
    await stack.down();
    await waitFor(async () => (await page.getByText("Connected.").count()) === 0, "the connection state to leave Connected", 30_000);
    console.log("offline:", await page.locator("header, main > *").first().ariaSnapshot());
    await stack.up();
    await waitFor(async () => (await page.getByText("Connected.").count()) > 0, "reconnection", 60_000);
  }, 180_000);

});

if (blocked !== null) {
  it.skip(blocked, () => undefined);
}
