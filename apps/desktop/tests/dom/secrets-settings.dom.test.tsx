// @vitest-environment jsdom
/** ADP-009 on a page: the password manager and browser import. A password goes out once and is never shown back. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { SecretsSettings } from "../../src/renderer/SecretsSettings.js";
import { click, mount, text, type Rig } from "./rig.js";

const ok = (result: Record<string, unknown> = {}): Record<string, unknown> => ({ ok: true, result });
const refused = (code: string, message: string, details: Record<string, unknown> = {}): Record<string, unknown> => ({ ok: false, error: { code, message, details } });

const LOGINS = ok({ entries: [{ id: "pw_1", origin: "https://example.com", username: "ada" }, { id: "pw_2", origin: "https://other.test", username: "bob" }] });
const SCAN = ok({
  profiles: [
    { family: "chrome", familyLabel: "Chrome", profile: "Default", displayName: "Ada", logins: 3, cookies: { total: 5, domains: [{ domain: "example.com", count: 3 }, { domain: "other.test", count: 2 }] } },
    { family: "firefox", profile: "p1", logins: 0, cookies: null },
  ],
});

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

async function fillIn(name: string, value: string): Promise<void> {
  const input = rig.container.querySelector<HTMLInputElement>(`input[name=${name}]`);
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
    await Promise.resolve();
  });
}

describe("the Secrets settings", () => {
  it("lists saved logins as origin and user name only, and removes one", async () => {
    rig = await mount(<SecretsSettings />, { "passwords.status": ok({ available: true, locked: false, count: 2, max: 100 }), "passwords.list": LOGINS, "passwords.remove": ok() });
    expect([...rig.container.querySelectorAll("li")].map((item) => item.textContent)).toEqual(["ada · https://example.com Remove", "bob · https://other.test Remove"]);
    expect(rig.container.querySelector(".notice")).toBeNull();
    await click(rig, "Remove");
    expect(sent("passwords.remove")[0]?.payload).toEqual({ id: "pw_1" });
  });

  it("says when nothing is saved, when the keychain is unavailable, and when the vault is locked", async () => {
    rig = await mount(<SecretsSettings />, { "passwords.status": ok({ available: false, locked: true }), "passwords.list": ok({ entries: [] }) });
    const page = text(rig);
    expect(page).toContain("None saved.");
    expect(page).toContain("cannot keep passwords encrypted");
    expect(page).toContain("could not be read on this computer");
  });

  it("sends a typed login once and clears the password from the form before the answer", async () => {
    rig = await mount(<SecretsSettings />, { "passwords.status": ok({ available: true }), "passwords.list": LOGINS, "passwords.save": ok() });
    await fillIn("origin", "https://new.test");
    await fillIn("username", "cy");
    await fillIn("password", "s3cret");
    const form = rig.container.querySelector("form") as HTMLFormElement;
    await act(async () => {
      form.requestSubmit();
      await Promise.resolve();
    });
    await rig.flush();
    expect(sent("passwords.save")[0]?.payload).toEqual({ origin: "https://new.test", username: "cy", password: "s3cret" });
    expect(rig.container.querySelector<HTMLInputElement>("input[name=password]")?.value).toBe("");
    expect(text(rig)).toContain("Saved.");
    expect(text(rig)).not.toContain("s3cret");
  });

  it("shows a refused save with its reason", async () => {
    rig = await mount(<SecretsSettings />, { "passwords.status": ok({ available: true }), "passwords.list": LOGINS, "passwords.save": refused("VALIDATION_SCHEMA", "bad origin", { reason: "origin" }) });
    await act(async () => {
      (rig.container.querySelector("form") as HTMLFormElement).requestSubmit();
      await Promise.resolve();
    });
    await rig.flush();
    expect(rig.container.querySelector(".notice")?.textContent).toBe("VALIDATION_SCHEMA (origin): bad origin");
  });

  it("shows a refused removal", async () => {
    rig = await mount(<SecretsSettings />, { "passwords.status": ok({ available: true }), "passwords.list": LOGINS, "passwords.remove": refused("NOT_FOUND", "gone") });
    await click(rig, "Remove");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("NOT_FOUND: gone");
  });

  it("scans for browsers, imports logins for a profile, and shows the counts", async () => {
    rig = await mount(<SecretsSettings />, { "passwords.status": ok({ available: true }), "passwords.list": LOGINS, "import.scan": SCAN, "import.logins": ok({ imported: 2, skipped: 1 }) });
    await click(rig, "Look for browsers");
    const profile = rig.container.querySelector("[data-profile='chrome/Default']");
    expect(profile?.textContent).toContain("Chrome");
    expect(profile?.textContent).toContain("3 saved logins.");
    expect(rig.container.querySelector("[data-profile='firefox/p1']")?.textContent).not.toContain("saved logins");
    await click(rig, "Import logins");
    expect(sent("import.logins")[0]?.payload).toEqual({ family: "chrome", profile: "Default" });
    expect(rig.container.querySelector(".notice")?.textContent).toBe("Imported 2 logins; 1 skipped.");
  });

  it("stages only the cookie domains the person ticked", async () => {
    rig = await mount(<SecretsSettings />, {
      "passwords.status": ok({ available: true }), "passwords.list": LOGINS, "import.scan": SCAN,
      "import.cookies": ok({ cookieCount: 3, domains: ["example.com"], expiresAt: "2026-10-01T00:00:00Z" }),
    });
    await click(rig, "Look for browsers");
    const stage = (): HTMLButtonElement => [...rig.container.querySelectorAll("button")].find((button) => button.textContent === "Stage cookies…") as HTMLButtonElement;
    expect(stage().disabled).toBe(true);
    await click(rig, "example.com");
    await click(rig, "other.test");
    await click(rig, "other.test");
    expect(stage().disabled).toBe(false);
    await click(rig, "Stage cookies");
    expect(sent("import.cookies")[0]?.payload).toEqual({ family: "chrome", profile: "Default", domains: ["example.com"] });
    expect(rig.container.querySelector(".notice")?.textContent).toContain("Staged 3 cookies for example.com");
  });

  it("shows a refused scan, an empty scan, and a refused staging", async () => {
    rig = await mount(<SecretsSettings />, { "passwords.status": ok({ available: true }), "passwords.list": LOGINS, "import.scan": refused("PERMISSION_DENIED", "declined") });
    await click(rig, "Look for browsers");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("PERMISSION_DENIED: declined");
    rig.answer("import.scan", ok({ profiles: [] }));
    await click(rig, "Look for browsers");
    expect(text(rig)).toContain("No supported browser was found.");
    rig.answer("import.scan", SCAN);
    rig.answer("import.cookies", refused("PERMISSION_DENIED", "no"));
    await click(rig, "Look for browsers");
    await click(rig, "example.com");
    await click(rig, "Stage cookies");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("PERMISSION_DENIED: no");
  });
});
