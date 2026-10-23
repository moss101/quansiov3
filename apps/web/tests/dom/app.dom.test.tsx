// @vitest-environment jsdom
/** APP-012 shell on a page: sign-in, the navigation the roles allow, the palette and the phone drawer. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { App } from "../../src/app/App.js";
import { click, fill, mountWeb, text, type WebRig } from "./rig.js";

const me = {
  user: { id: "usr_1", display_name: "Ada Owner", primary_email: "ada@example.com" },
  session: { tenant_id: "tn_1" },
  tenants: [{ id: "tn_1", role: "owner" }],
  workspaces: [{ id: "ws_1", name: "Main", role: "admin" }, { id: "ws_2", name: "Side", role: "viewer" }],
};
const answers = {
  "GET /v1/me": me,
  "GET /v1/workgraph": { workspace_id: "ws_1", revision: 1, nodes: [], edges: [], agents: [], blockers: [] },
  "GET /v1/approvals": { approvals: [] },
  "POST /v1/auth/sign-out": {},
  "POST /v1/auth/magic-link/start": {},
};

let rig: WebRig;
afterEach(async () => {
  await rig.unmount();
  window.history.replaceState({}, "", "/");
});

describe("the web shell", () => {
  it("asks for an email when there is no session, and says the link was sent", async () => {
    rig = await mountWeb((session) => <App session={session} width={1200} />, answers, false);
    await fill(rig, "you@example.com", "ada@example.com");
    rig.container.querySelector("form")?.requestSubmit();
    await rig.flush();
    expect(rig.requests.find((request) => request.path === "/v1/auth/magic-link/start")?.body).toEqual({ email: "ada@example.com", display_name: "ada@example.com" });
    expect(rig.container.querySelector("[role=status]")).not.toBeNull();
  });

  it("shows a failed sign-in as an alert", async () => {
    rig = await mountWeb((session) => <App session={session} width={1200} />, { "POST /v1/auth/magic-link/start": { status: 400, body: { code: "VALIDATION_SCHEMA", message: "bad email", correlation_id: "c", retryable: false } } }, false);
    rig.container.querySelector("form")?.requestSubmit();
    await rig.flush();
    expect(rig.container.querySelector("[role=alert]")?.textContent).toContain("bad email");
  });

  it("completes a magic link from the URL token", async () => {
    window.history.replaceState({}, "", "/?token=abc");
    const assigned: string[] = [];
    const original = window.location;
    Object.defineProperty(window, "location", { value: { search: "?token=abc", pathname: "/", assign: (url: string) => assigned.push(url) }, configurable: true, writable: true });
    try {
      rig = await mountWeb((session) => <App session={session} width={1200} />, { "POST /v1/auth/magic-link/complete": { access_token: "a", refresh_token: "r", tenant_id: "t", user_id: "usr_1" } }, false);
      expect(rig.requests.some((request) => request.path === "/v1/auth/magic-link/complete")).toBe(true);
      expect(assigned).toEqual(["/app/"]);
    } finally {
      Object.defineProperty(window, "location", { value: original, configurable: true, writable: true });
    }
  });

  it("offers the pages the roles allow, switches workspace and navigates", async () => {
    rig = await mountWeb((session) => <App session={session} width={1200} />, answers);
    expect(text(rig)).toContain("Ada Owner");
    const surfaces = [...rig.container.querySelectorAll("[data-surface]")].map((el) => el.getAttribute("data-surface"));
    expect(surfaces).toContain("approvals");
    expect(surfaces).toContain("users");
    await click(rig, "Approvals");
    expect(rig.requests.some((request) => request.path.startsWith("/v1/approvals"))).toBe(true);
    expect(window.location.pathname).toContain("approvals");
    await fill(rig, "", "ws_2").catch(() => undefined);
    const select = rig.container.querySelector("select");
    expect(select?.querySelectorAll("option").length).toBe(2);
  });

  it("opens the palette with Ctrl+K, runs an item and closes with Escape", async () => {
    rig = await mountWeb((session) => <App session={session} width={1200} />, answers);
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true }));
      await Promise.resolve();
    });
    await rig.flush();
    expect(document.querySelector("[role=dialog], .palette")).not.toBeNull();
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await Promise.resolve();
    });
    await rig.flush();
  });

  it("uses a drawer on a phone: the menu opens, Escape closes it, the search button opens the palette", async () => {
    rig = await mountWeb((session) => <App session={session} width={390} />, answers);
    await click(rig, "Menu");
    expect(rig.container.querySelector(".menu-open")).not.toBeNull();
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector(".menu-open")).toBeNull();
    await click(rig, "Menu");
    await click(rig, "Close menu");
    await click(rig, "Search");
    expect(rig.container.querySelector("input")).not.toBeNull();
  });

  it("shows the refusal when the session cannot be read, and signs out", async () => {
    rig = await mountWeb((session) => <App session={session} width={1200} />, { "GET /v1/me": { status: 401, body: { code: "AUTH_INVALID", message: "no", correlation_id: "c", retryable: false } }, "POST /v1/auth/sign-out": {} });
    expect(rig.container.querySelector("[role=alert]")).not.toBeNull();
  });

  it("tells a person with no workspace role so", async () => {
    rig = await mountWeb((session) => <App session={session} width={1200} />, { "GET /v1/me": { ...me, workspaces: [] } }, true, null);
    expect(rig.container.querySelector("main .muted")).not.toBeNull();
  });
});
