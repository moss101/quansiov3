// @vitest-environment jsdom
/** APP-002 on a page: Onboarding walks sign-in, execution target and security defaults, each step one bridge call. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { Onboarding } from "../../src/renderer/Onboarding.js";
import { click, mount, text, type, type Rig } from "./rig.js";

const SIGNED_OUT = { kind: "signed-out", persistent: true };
const SIGNED_IN = { kind: "signed-in", userId: "usr_1", tenantId: "tn_1", workspaceId: "ws_1", onboarded: true };

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

describe("the Onboarding view", () => {
  it("asks for an email, offers only the providers the server offers, and sends a sign-in link", async () => {
    rig = await mount(<Onboarding onReady={() => undefined} />, { "session.state": SIGNED_OUT, "session.me": { auth_methods_offered: { oauth: ["github"] } }, "auth.magicLink.start": {} });
    const send = (): HTMLButtonElement => [...rig.container.querySelectorAll("button")].find((button) => button.textContent === "Email me a sign-in link") as HTMLButtonElement;
    expect(send().disabled).toBe(true);
    await type(rig, "you@example.com", " ada@example.com ");
    await type(rig, "Your name", "Ada");
    expect(send().disabled).toBe(false);
    const form = rig.container.querySelector("form") as HTMLFormElement;
    await act(async () => {
      form.requestSubmit();
      await Promise.resolve();
    });
    await rig.flush();
    expect(sent("auth.magicLink.start")[0]?.payload).toEqual({ email: "ada@example.com", displayName: "Ada" });
    expect(text(rig)).toContain("Check your inbox");
    expect(text(rig)).toContain("ada@example.com");
  });

  it("completes the link only with a qml_ token", async () => {
    rig = await mount(<Onboarding onReady={() => undefined} />, { "session.state": { kind: "link-sent", email: "ada@example.com", expiresAt: "x", delivery: "email" }, "auth.magicLink.complete": {} });
    const next = (): HTMLButtonElement => [...rig.container.querySelectorAll("button")].find((button) => button.textContent === "Continue") as HTMLButtonElement;
    await type(rig, "sign-in token", "abc");
    expect(next().disabled).toBe(true);
    await type(rig, "sign-in token", "qml_secret ");
    expect(next().disabled).toBe(false);
    await click(rig, "Continue");
    expect(sent("auth.magicLink.complete")[0]?.payload).toEqual({ token: "qml_secret" });
  });

  it("starts the chosen OAuth provider, and disables the ones not offered", async () => {
    rig = await mount(<Onboarding onReady={() => undefined} />, { "session.state": SIGNED_OUT });
    await click(rig, "Continue with GitHub");
    expect(sent("auth.oauth.start")[0]?.payload).toEqual({ provider: "github" });
  });

  it("shows a failed sign-in as a notice", async () => {
    // eslint-disable-next-line @typescript-eslint/prefer-promise-reject-errors -- a non-Error rejection is the case under test
    rig = await mount(<Onboarding onReady={() => undefined} />, { "session.state": SIGNED_OUT, "auth.magicLink.start": () => Promise.reject(new Error("mail refused")), "auth.oauth.start": () => Promise.reject("odd") });
    await type(rig, "you@example.com", "ada@example.com");
    await act(async () => {
      (rig.container.querySelector("form") as HTMLFormElement).requestSubmit();
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector(".notice")?.textContent).toBe("mail refused");
    await click(rig, "Continue with Google");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("The command was refused.");
  });

  it("walks the execution target and the security defaults, then reports ready", async () => {
    let ready = 0;
    rig = await mount(<Onboarding onReady={() => { ready += 1; }} />, { "session.state": SIGNED_OUT, "session.me": { auth_methods_offered: { oauth: [] } }, "settings.updateWorkspace": {} });
    await rig.push("session.state", SIGNED_IN);
    expect(text(rig)).toContain("Where should your work run?");
    await click(rig, "local_capsule");
    expect(sent("settings.updateWorkspace")[0]?.payload).toEqual({ workspaceId: "ws_1", settings: { execution_target: "local_capsule" } });
    const page = text(rig);
    expect(page).toContain("Security defaults");
    expect(page).toContain("telemetry: false");
    expect(page).toContain("unsigned_updates: false");
    expect(ready).toBe(0);
    await click(rig, "Confirm");
    expect(sent("settings.updateWorkspace")[1]?.payload).toEqual({
      workspaceId: "ws_1", settings: { telemetry: false, clipboard_to_model: false, unsigned_updates: false, share_analytics: false },
    });
    expect(ready).toBeGreaterThan(0);
  });

  it("stays on the step and shows the refusal when a setting is refused", async () => {
    rig = await mount(<Onboarding onReady={() => undefined} />, { "session.state": SIGNED_IN, "settings.updateWorkspace": () => Promise.reject(new Error("admins only")) });
    await click(rig, "cloud");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("admins only");
    expect(text(rig)).toContain("Where should your work run?");
  });

  it("is ready at once for a session that needs no onboarding step, and returns to sign-in when signed out", async () => {
    let ready = 0;
    rig = await mount(<Onboarding onReady={() => { ready += 1; }} />, { "session.state": { ...SIGNED_IN, onboarded: false } });
    expect(ready).toBeGreaterThan(0);
    await rig.push("session.state", SIGNED_OUT);
    expect(text(rig)).toContain("Email me a sign-in link");
  });
});
