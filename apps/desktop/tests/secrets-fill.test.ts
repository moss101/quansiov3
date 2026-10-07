/**
 * Filling a saved login into the page the person is looking at (ADP-009, DOMAIN.md §13.9 A), through the real guarded IPC.
 *
 * The properties: a fill is the person's act on the main window and nothing else's; the password reaches one place (the page writer)
 * and is wiped after it; a login saved for another origin is filled only on an explicit yes, with Cancel as the default; and the
 * page's identity is read three times across the asynchronous steps, so a page that changes while a question is open, or before the
 * insertion, receives nothing.
 *
 * Test values only; the cipher, the question and the page are the rig's.
 */

import { afterEach, describe, expect, it } from "vitest";

import { PasswordFiller, type PageProbe, type PageView } from "../src/main/secrets/fill.js";
import { PasswordVault, VAULT_FILE } from "../src/main/secrets/vault.js";
import { Secret } from "../src/main/secrets/secret.js";
import { join } from "node:path";

import { SECRETS_CONFIG, makeRig, testCipher, type Rig } from "./secrets-rig.js";

const SESSION = "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const PASSWORD = "c0rrect-h0rse-battery-staple";
const rigs: Rig[] = [];

afterEach(() => {
  for (const rig of rigs.splice(0)) {
    rig.cleanup();
  }
});

function rig(options: Parameters<typeof makeRig>[0] = { writer: "present" }): Rig {
  const made = makeRig(options);
  rigs.push(made);
  return made;
}

async function saved(r: Rig, origin = "https://example.com", username = "ada"): Promise<string> {
  const answer = await r.call("passwords.save", r.main, { origin, username, password: PASSWORD });
  expect(answer.ok, JSON.stringify(answer)).toBe(true);
  return (answer.result?.["entry"] as { id: string }).id;
}

describe("filling a login", () => {
  it("fills the page's own origin with no question, hands the writer the password once and wipes it after", async () => {
    const r = rig();
    const id = await saved(r);
    const answer = await r.call("passwords.fill", r.main, { id, sessionId: SESSION });
    expect(answer).toMatchObject({ ok: true, filled: true, crossSite: false });
    expect(r.asked).toEqual([]);
    expect(r.written).toEqual([{ username: "ada", password: PASSWORD }]);
    // The reply carries no password.
    expect(JSON.stringify(answer)).not.toContain(PASSWORD);
    expect(JSON.stringify(answer)).not.toContain("ada");
  });

  it("is the main window's alone: a pop-out, quick entry and the indicator reach nothing", async () => {
    const r = rig();
    const id = await saved(r);
    for (const window of [r.popout, r.quick, r.indicator]) {
      const refused = await r.call("passwords.fill", window, { id, sessionId: SESSION });
      expect(refused).toMatchObject({ ok: false, error: { code: "SCOPE_FORBIDDEN" } });
    }
    expect(r.written).toEqual([]);
  });

  it("asks before filling a login saved for another site, with Cancel as the default", async () => {
    const r = rig();
    const id = await saved(r, "https://example.com");
    r.page.topOrigin = "https://evil.test";
    r.page.frameOrigin = "https://evil.test";
    // Nobody answers: the default button is Cancel.
    const declined = await r.call("passwords.fill", r.main, { id, sessionId: SESSION });
    expect(declined).toMatchObject({ ok: false, error: { code: "POLICY_DENIED", details: { reason: "declined" } } });
    expect(r.asked).toHaveLength(1);
    expect(r.asked[0]?.message).toContain("https://example.com");
    expect(r.asked[0]?.message).toContain("https://evil.test");
    expect(r.asked[0]?.defaultId).toBe(0);
    expect(r.written).toEqual([]);
    // The person says yes.
    r.answers.push(1);
    const filled = await r.call("passwords.fill", r.main, { id, sessionId: SESSION });
    expect(filled).toMatchObject({ ok: true, filled: true, crossSite: true });
    expect(r.written).toHaveLength(1);
  });

  it("gives nothing to a page that changed while the question was open", async () => {
    const r = rig();
    const id = await saved(r, "https://example.com");
    r.page.topOrigin = "https://other.test";
    r.page.frameOrigin = "https://other.test";
    r.answers.push(1);
    // While the person reads the question the page navigates somewhere else.
    r.during = () => {
      r.page.topOrigin = "https://evil.test";
      r.page.frameOrigin = "https://evil.test";
    };
    const answer = await r.call("passwords.fill", r.main, { id, sessionId: SESSION });
    expect(answer).toMatchObject({ ok: false, error: { code: "CONFLICT_STATE", details: { reason: "origin_changed" } } });
    expect(r.written).toEqual([]);
    // A new document at the same origin is a changed page too.
    r.during = () => {
      r.page.revision = "rev-2";
    };
    r.answers.push(1);
    expect(await r.call("passwords.fill", r.main, { id, sessionId: SESSION })).toMatchObject({ ok: false, error: { details: { reason: "origin_changed" } } });
    expect(r.written).toEqual([]);
  });

  it("answers ADAPTER_UNAVAILABLE where this build has no way to type into the page, and asks nothing", async () => {
    const r = rig({ writer: "absent" });
    const id = await saved(r);
    expect(await r.call("passwords.fill", r.main, { id, sessionId: SESSION })).toMatchObject({ ok: false, error: { code: "ADAPTER_UNAVAILABLE" } });
    expect(r.asked).toEqual([]);
  });

  it("refuses an unknown login, a page with no origin and a malformed request", async () => {
    const r = rig();
    const id = await saved(r);
    expect(await r.call("passwords.fill", r.main, { id: "pw_".padEnd(35, "0"), sessionId: SESSION })).toMatchObject({ ok: false, error: { code: "NOT_FOUND" } });
    r.page.topOrigin = null;
    r.page.frameOrigin = null;
    expect(await r.call("passwords.fill", r.main, { id, sessionId: SESSION })).toMatchObject({ ok: false, error: { code: "CONFLICT_STATE", details: { reason: "no_page" } } });
    for (const bad of [{}, { id }, { id, sessionId: "run_x" }, { id: "../x", sessionId: SESSION }, { id, sessionId: SESSION, extra: 1 }]) {
      expect(await r.call("passwords.fill", r.main, bad)).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    }
    expect(r.written).toEqual([]);
  });
});

describe("the filler on its own", () => {
  function filler(views: (PageView | null)[], clock: { now: number }, writes: string[]): { filler: PasswordFiller; vault: PasswordVault; path: string } {
    const dir = join(rigs[0]?.userData ?? "", "direct");
    const vault = new PasswordVault({ path: join(dir, VAULT_FILE), cipher: testCipher(), config: SECRETS_CONFIG });
    const probe: PageProbe = { view: () => Promise.resolve(views.length > 1 ? (views.shift() ?? null) : (views[0] ?? null)) };
    return {
      vault,
      path: dir,
      filler: new PasswordFiller({
        vault,
        config: SECRETS_CONFIG,
        probe,
        writer: {
          fill: (_view, username: Secret, password: Secret) => {
            writes.push(`${username.expose()}:${password.expose()}`);
            return Promise.resolve({ filled: true });
          },
        },
        confirmation: { confirm: () => Promise.resolve(true) },
        now: () => clock.now,
      }),
    };
  }

  const view = (over: Partial<PageView> = {}): PageView => ({ sessionId: SESSION, topOrigin: "https://example.com", frameOrigin: "https://example.com", documentRevision: "r1", ...over });

  it("treats a login field in a frame of another origin as cross-site even on the person's own site", async () => {
    rig();
    const writes: string[] = [];
    const clock = { now: 1_000 };
    const { vault } = filler([view({ frameOrigin: "https://ads.test" })], clock, writes);
    const saved = vault.save({ origin: "https://example.com", username: "ada", password: PASSWORD });
    const main = rigs[0]?.main;
    if (main === undefined) {
      throw new Error("no rig");
    }
    const asked: string[] = [];
    const g = new PasswordFiller({
      vault,
      config: SECRETS_CONFIG,
      probe: { view: () => Promise.resolve(view({ frameOrigin: "https://ads.test" })) },
      writer: { fill: () => Promise.resolve({ filled: true }) },
      confirmation: {
        confirm: (_window, ask) => {
          asked.push(`${ask.entryOrigin}->${ask.pageOrigin}`);
          return Promise.resolve(false);
        },
      },
    });
    expect(await g.fill(main, { id: saved.id, sessionId: SESSION })).toMatchObject({ ok: false, error: { code: "POLICY_DENIED" } });
    expect(asked).toEqual(["https://example.com->https://ads.test"]);
  });

  it("refuses a page that was read too long ago, and a page that changed between the last two readings", async () => {
    rig();
    const main = rigs[0]?.main;
    if (main === undefined) {
      throw new Error("no rig");
    }
    const clock = { now: 1_000 };
    const writes: string[] = [];
    // Stale: the confirmation takes longer than fill_max_age_ms.
    const { vault } = filler([view()], clock, writes);
    const login = vault.save({ origin: "https://other.test", username: "ada", password: PASSWORD });
    const slow = new PasswordFiller({
      vault,
      config: SECRETS_CONFIG,
      probe: { view: () => Promise.resolve(view()) },
      writer: { fill: () => Promise.resolve({ filled: true }) },
      confirmation: {
        confirm: () => {
          clock.now += SECRETS_CONFIG.fillMaxAgeMs + 1;
          return Promise.resolve(true);
        },
      },
      now: () => clock.now,
    });
    expect(await slow.fill(main, { id: login.id, sessionId: SESSION })).toMatchObject({ ok: false, error: { details: { reason: "stale_probe" } } });

    // The last reading differs from the first: the document changed after the password was decrypted.
    const readings = [view(), view({ documentRevision: "r2" })];
    const own = vault.save({ origin: "https://example.com", username: "grace", password: PASSWORD });
    const racing = new PasswordFiller({
      vault,
      config: SECRETS_CONFIG,
      probe: { view: () => Promise.resolve(readings.shift() ?? null) },
      writer: { fill: () => Promise.resolve({ filled: true }) },
      confirmation: { confirm: () => Promise.resolve(true) },
      now: () => clock.now,
    });
    expect(await racing.fill(main, { id: own.id, sessionId: SESSION })).toMatchObject({ ok: false, error: { details: { reason: "origin_changed" } } });
    expect(writes).toEqual([]);
  });
});
