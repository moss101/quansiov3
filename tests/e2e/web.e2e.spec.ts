/**
 * QA-008 web journeys: the built web app served by the real server, driven in the installed Google
 * Chrome through `playwright-core` (`harness.ts`). The web app is the remote review / approval /
 * administration surface (DOSSIER.md §15); these are its people's workflows end to end.
 *
 * Support matrix (§21.1): Chrome runs here; Edge, Firefox and Safari are reported `BLOCKED_EXTERNAL`
 * (not installed / not drivable without changing a system setting).
 */

import type { Browser, Page } from "playwright-core";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  APPROVER,
  auditAccessibility,
  chromeBlocked,
  emailOf,
  launchChrome,
  OWNER,
  settle,
  Stack,
  VIEWER,
  waitFor,
  walkKeyboard,
} from "./harness.js";

const blocked = chromeBlocked();

describe.skipIf(blocked !== null)("web journeys (Chrome, real server)", () => {
  let stack: Stack;
  let browser: Browser;

  beforeAll(async () => {
    stack = await Stack.start("web");
    browser = await launchChrome();
  }, 180_000);

  afterAll(async () => {
    await browser.close();
    await stack.drop();
  }, 60_000);

  /** Sign in the way a person does: ask for the link, then open it. */
  async function signIn(email: string): Promise<Page> {
    const page = await browser.newPage();
    await page.goto(`${stack.origin}/app/`);
    await page.getByRole("textbox").first().fill(email);
    const mark = stack.mark();
    await page.getByRole("button", { name: /sign-in link/i }).click();
    await page.getByRole("status").waitFor();
    const token = await stack.magicLinkToken(email, mark);
    await page.goto(`${stack.origin}/app/?token=${token}`);
    await page.getByRole("navigation").waitFor({ timeout: 30_000 });
    return page;
  }

  const nav = (page: Page, name: string) => page.getByRole("navigation").getByRole("button", { name, exact: true });

  it("signs in from the emailed link and reviews the work", async () => {
    const page = await signIn(emailOf(OWNER));
    try {
      const review = page.getByRole("region", { name: "Work review" });
      await review.getByText("e2e seed").first().waitFor();
      expect(await nav(page, "Users").count()).toBe(1);
    } finally {
      await page.close();
    }
  }, 120_000);

  it("lets an approver decide a request remotely, bound to its preview", async () => {
    const owner = await stack.session(emailOf(OWNER));
    const posted = await stack.command(owner.access_token, "PostMessage", { workspace_id: owner.workspace_id, content: "write the summary [[scenario:approval]]" });
    const page = await signIn(emailOf(APPROVER));
    try {
      await nav(page, "Approvals").click();
      const approvals = page.getByRole("region", { name: "Approvals" });
      await waitFor(async () => (await approvals.getByRole("button", { name: "Review" }).count()) > 0, "the request in the approvals page", 30_000);
      await approvals.getByRole("button", { name: "Review" }).first().click();
      const dialog = approvals.getByRole("dialog");
      await dialog.waitFor();
      // The preview names the exact payload the decision is bound to, as the server records it.
      const pending = (await stack.get(owner.access_token, `/v1/approvals?run_id=${String(posted["run_id"])}`))["approvals"] as { params_digest: string }[];
      expect((await dialog.locator("code").first().textContent())?.trim()).toBe(pending[0]?.params_digest);
      expect(await dialog.getByText("untrusted origin", { exact: false }).count()).toBeGreaterThan(0);
      await dialog.getByRole("button", { name: "Approve", exact: true }).click();
      await waitFor(async () => {
        const decided = (await stack.get(owner.access_token, `/v1/approvals?run_id=${String(posted["run_id"])}`))["approvals"] as { status: string }[];
        return decided[0]?.status === "granted";
      }, "the grant to be recorded", 30_000);
      const audit = JSON.stringify(await stack.get(owner.access_token, "/v1/audit"));
      expect(audit).toContain(APPROVER);
    } finally {
      await page.close();
    }
  }, 120_000);

  it("administers members and the change is audited", async () => {
    const page = await signIn(emailOf(OWNER));
    try {
      await nav(page, "Users").click();
      const users = page.getByRole("region", { name: "Users" });
      await users.getByRole("textbox").first().fill("qa008-invitee@example.com");
      await users.getByRole("button", { name: "Invite" }).click();
      await waitFor(async () => (await users.getByText("qa008-invitee@example.com").count()) > 0, "the invited member in the table", 30_000);
      await nav(page, "Audit & privacy").click();
      const audit = page.getByRole("region", { name: "Audit & privacy" });
      await waitFor(async () => (await audit.getByText(/invite/i).count()) > 0, "the invitation in the audit log", 30_000);
    } finally {
      await page.close();
    }
  }, 120_000);

  it("offers a viewer no administration and the server agrees", async () => {
    const page = await signIn(emailOf(VIEWER));
    try {
      for (const admin of ["Users", "Policies", "Audit & privacy", "Connectors"]) {
        expect(await nav(page, admin).count(), admin).toBe(0);
      }
      // The palette offers only what the navigation offers.
      await page.getByRole("button", { name: "Search", exact: true }).click();
      const palette = page.getByRole("dialog", { name: "Go to" });
      await palette.getByRole("combobox", { name: "Go to" }).fill("users");
      expect(await palette.getByRole("option").count()).toBe(0);
      await palette.getByRole("combobox", { name: "Go to" }).fill("approvals");
      expect(await palette.getByRole("option").count()).toBe(1);
      await page.keyboard.press("Escape");
      const viewer = await stack.session(emailOf(VIEWER));
      const members = await fetch(`${stack.origin}/v1/members`, { headers: { authorization: `Bearer ${viewer.access_token}` } });
      expect(members.status).toBe(403);
    } finally {
      await page.close();
    }
  }, 120_000);

  it("has no accessibility blockers on any page, works by keyboard, and fits a phone", async () => {
    const page = await signIn(emailOf(OWNER));
    try {
      const failures: string[] = [];
      await nav(page, "Users").waitFor();
      const pages = await page.getByRole("navigation").getByRole("button").allTextContents();
      expect(pages.length).toBeGreaterThanOrEqual(8);
      const urls: string[] = [];
      for (const name of pages) {
        await nav(page, name).click();
        await settle(page);
        urls.push(page.url());
        const audit = await auditAccessibility(page);
        failures.push(...audit.violations.map((violation) => `${name}: ${violation}`));
        const keyboard = await walkKeyboard(page);
        failures.push(...keyboard.unreachable.map((control) => `${name}: not reachable by keyboard: ${control}`));
        failures.push(...keyboard.invisibleFocus.map((control) => `${name}: focus not visible on ${control}`));
      }
      // On a tablet the navigation is an icon rail: each page is still reachable by its own name.
      await page.setViewportSize({ width: 900, height: 800 });
      for (const name of pages) {
        if ((await nav(page, name).count()) !== 1) {
          failures.push(`rail: no page button named exactly "${name}"`);
        }
      }
      await page.setViewportSize({ width: 375, height: 812 });
      for (const url of urls) {
        await page.goto(url);
        await page.getByRole("main").waitFor();
        await settle(page);
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
        if (overflow > 1) {
          failures.push(`${url}: ${String(overflow)}px of horizontal scroll at 375px`);
        }
      }
      expect(failures).toEqual([]);
    } finally {
      await page.close();
    }
  }, 300_000);

  it("jumps to a page from the command palette and closes it with Escape", async () => {
    const page = await signIn(emailOf(OWNER));
    try {
      await nav(page, "Users").waitFor();
      await page.keyboard.press(process.platform === "darwin" ? "Meta+K" : "Control+K");
      const palette = page.getByRole("dialog", { name: "Go to" });
      await palette.waitFor();
      await palette.getByRole("combobox", { name: "Go to" }).fill("audit");
      expect(await palette.getByRole("option", { selected: true }).textContent()).toContain("Audit & privacy");
      await page.keyboard.press("Enter");
      await page.getByRole("region", { name: "Audit & privacy" }).waitFor({ timeout: 15_000 });
      expect(await palette.count()).toBe(0);
      expect(new URL(page.url()).pathname).toBe("/app/audit-and-privacy");
      // The search button opens the same palette; Escape leaves the page where it was.
      await page.getByRole("button", { name: "Search", exact: true }).click();
      await palette.waitFor();
      await page.keyboard.press("Escape");
      await waitFor(async () => (await palette.count()) === 0, "the palette to close");
      expect(await page.getByRole("region", { name: "Audit & privacy" }).count()).toBe(1);
    } finally {
      await page.close();
    }
  }, 120_000);

  it("follows the person's theme preference over the system's", async () => {
    const owner = await stack.session(emailOf(OWNER));
    const prefer = (theme: string) => stack.command(owner.access_token, "UpdateUserSettings", { workspace_id: owner.workspace_id, settings: { theme } });
    const background = (page: Page): Promise<string> => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
    await prefer("dark");
    const page = await signIn(emailOf(OWNER));
    try {
      await page.emulateMedia({ colorScheme: "light" });
      await waitFor(async () => (await background(page)) === "rgb(7, 7, 7)", "the page dark on a light system");
      await prefer("light");
      await page.emulateMedia({ colorScheme: "dark" });
      await page.reload();
      await page.getByRole("navigation").waitFor();
      await waitFor(async () => (await background(page)) === "rgb(252, 252, 252)", "the page light on a dark system");
      await prefer("system");
      await page.reload();
      await page.getByRole("navigation").waitFor();
      await waitFor(async () => (await page.evaluate(() => document.documentElement.dataset["theme"] ?? null)) === null && (await background(page)) === "rgb(7, 7, 7)", "the page to follow the system again");
    } finally {
      await prefer("system");
      await page.close();
    }
  }, 120_000);

  it("signs out", async () => {
    const page = await signIn(emailOf(OWNER));
    try {
      await page.getByRole("button", { name: "Sign out" }).click();
      await page.getByRole("button", { name: /sign-in link/i }).waitFor({ timeout: 15_000 });
    } finally {
      await page.close();
    }
  }, 120_000);
  it("the accessibility checks find what they look for (a control with no name, hidden focus)", async () => {
    const page = await signIn(emailOf(OWNER));
    try {
      await page.evaluate(() => {
        const unnamed = document.createElement("button");
        document.querySelector("main")?.append(unnamed);
        const hidden = document.createElement("button");
        hidden.textContent = "no ring";
        hidden.style.outline = "none";
        document.querySelector("main")?.append(hidden);
        const skipped = document.createElement("input");
        skipped.setAttribute("tabindex", "3");
        skipped.setAttribute("aria-label", "jumps the queue");
        document.querySelector("main")?.append(skipped);
      });
      const audit = await auditAccessibility(page);
      expect(audit.violations).toContain("a button has no accessible name");
      expect(audit.violations.some((violation) => violation.includes("positive tabindex"))).toBe(true);
      const keyboard = await walkKeyboard(page);
      expect(keyboard.invisibleFocus.some((control) => control.includes("no ring"))).toBe(true);
    } finally {
      await page.close();
    }
  }, 120_000);
});

if (blocked !== null) {
  it.skip(blocked, () => undefined);
}
