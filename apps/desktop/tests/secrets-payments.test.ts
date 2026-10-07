/**
 * The person's side of the payment-field flow (ADP-009, DOMAIN.md 13.9 C): validation that never repeats a value, the one
 * channel the values travel on, and the structure that gives the page and the agent no way to read one back.
 *
 * Test values only: a payment provider's published test card number and a made-up security code and name.
 */

import { mkdtempSync, readFileSync, readdirSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

import { DesktopBridge } from "../src/main/ipc.js";
import { loadLocalSecretsConfig } from "../src/main/secrets/config.js";
import { PAYMENT_FIELDS, PaymentSupplier, PaymentSupplyError, parsePaymentSupply, paymentOrigin } from "../src/main/secrets/payments.js";
import { PRELOAD_ALLOWLIST } from "../src/preload/bridge.js";
import { SessionFile } from "../src/main/session-file.js";
import { UpdateHooks } from "../src/main/updates.js";

const ROOT = join(__dirname, "..", "..", "..");
const CONFIG_PATH = join(ROOT, "config", "local-secrets.yaml");
const CONFIG = loadLocalSecretsConfig(CONFIG_PATH);
const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const TEST_CARD = "4242424242424242";
const TEST_CVC = "6803";
const ORIGIN = "https://shop.example";

describe("validating a supply", () => {
  it("accepts a card and an address for an https origin and carries the configured lifetime", () => {
    const parsed = parsePaymentSupply({ origin: ORIGIN, fields: { card_number: TEST_CARD, cvc: TEST_CVC, city: "Springfield" } }, CONFIG);
    expect(parsed.origin).toBe(ORIGIN);
    expect(parsed.fields).toEqual({ card_number: TEST_CARD, cvc: TEST_CVC, city: "Springfield" });
    expect(parsed.ttlSeconds).toBe(300);
  });

  it("accepts plain http only for a loopback host, a local test page", () => {
    expect(paymentOrigin("http://127.0.0.1:8080")).toBe("http://127.0.0.1:8080");
    expect(paymentOrigin("http://localhost:3000")).toBe("http://localhost:3000");
    for (const origin of ["http://shop.example", "ftp://shop.example", "https://user:pw@shop.example", "https://shop.example/checkout", "https://shop.example?x=1", "shop.example", "", 5, null]) {
      expect(() => paymentOrigin(origin), String(origin)).toThrow(PaymentSupplyError);
    }
  });

  it("refuses what breaks a rule, and no refusal repeats a value", () => {
    const bad: unknown[] = [
      null,
      [],
      { origin: ORIGIN },
      { origin: ORIGIN, fields: {} },
      { origin: ORIGIN, fields: { card_number: TEST_CARD, workspace_id: "ws_other" } },
      { origin: ORIGIN, fields: { password: TEST_CVC } },
      { origin: ORIGIN, fields: { card_number: 4242424242424242 } },
      { origin: ORIGIN, fields: { card_number: "" } },
      { origin: ORIGIN, fields: { card_number: TEST_CARD.repeat(10) } },
      { origin: ORIGIN, fields: Object.fromEntries(PAYMENT_FIELDS.map((name) => [name, TEST_CVC])), ttl_seconds: 999999 },
      { origin: "http://shop.example", fields: { card_number: TEST_CARD } },
      { origin: ORIGIN, fields: { card_number: TEST_CARD }, workspace_id: "ws_other" },
    ];
    for (const request of bad) {
      let message = "";
      try {
        parsePaymentSupply(request, CONFIG);
      } catch (error) {
        expect(error).toBeInstanceOf(PaymentSupplyError);
        message = (error as Error).message;
      }
      expect(message, JSON.stringify(request)).not.toBe("");
      expect(message).not.toContain(TEST_CARD);
      expect(message).not.toContain(TEST_CVC);
    }
  });

  it("is refused whole when the switch is off", () => {
    expect(() => parsePaymentSupply({ origin: ORIGIN, fields: { card_number: TEST_CARD } }, { ...CONFIG, enabled: false })).toThrow(/switched off/);
  });

  it("names exactly the fields the runtime and the tool's schema name", () => {
    const rust = readFileSync(join(ROOT, "crates", "machine", "src", "secrets", "payment.rs"), "utf8");
    const block = /pub const PAYMENT_FIELDS: \[&str; \d+\] = \[([^\]]*)\]/.exec(rust)?.[1] ?? "";
    expect([...block.matchAll(/"([a-z_]+)"/g)].map((match) => match[1])).toEqual([...PAYMENT_FIELDS]);
    const tools = readFileSync(join(ROOT, "config", "tools.yaml"), "utf8");
    const entry = tools.slice(tools.indexOf("name: browser.fill_payment_field"));
    const enumBlock = /enum:\s*\[([^\]]*)\]/.exec(entry)?.[1] ?? "";
    expect(enumBlock.split(",").map((name) => name.trim())).toEqual([...PAYMENT_FIELDS]);
  });
});

describe("the supplier", () => {
  function supplier(answer: () => Promise<{ readonly result: unknown }>, config = CONFIG): { supplier: PaymentSupplier; calls: { name: string; commandId: string; params: Record<string, unknown> }[] } {
    const calls: { name: string; commandId: string; params: Record<string, unknown> }[] = [];
    return {
      calls,
      supplier: new PaymentSupplier({
        config,
        server: {
          invoke: (name, commandId, params) => {
            calls.push({ name, commandId, params });
            return answer();
          },
        },
        workspaceId: () => WORKSPACE,
        newCommandId: () => "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      }),
    };
  }

  it("sends one SupplyPaymentInput for main's workspace and answers with names and times only", async () => {
    const { supplier: target, calls } = supplier(() => Promise.resolve({ result: { workspace_id: WORKSPACE, origin: ORIGIN, supplied: [{ field: "card_number", expires_at: "2026-10-07T12:05:00Z" }] } }));
    const answer = await target.supply({ origin: ORIGIN, fields: { card_number: TEST_CARD } });
    expect(calls).toHaveLength(1);
    expect(calls[0]?.name).toBe("SupplyPaymentInput");
    expect(calls[0]?.params).toEqual({ workspace_id: WORKSPACE, origin: ORIGIN, fields: { card_number: TEST_CARD }, ttl_seconds: 300 });
    expect(answer).toEqual({ origin: ORIGIN, supplied: [{ field: "card_number", expiresAt: "2026-10-07T12:05:00Z" }] });
    expect(JSON.stringify(answer)).not.toContain(TEST_CARD);
  });

  it("does not echo a value a server answer carried by mistake", async () => {
    const { supplier: target } = supplier(() => Promise.resolve({ result: { supplied: [{ field: "card_number", expires_at: "t", value: TEST_CARD }], echo: TEST_CARD } }));
    expect(JSON.stringify(await target.supply({ origin: ORIGIN, fields: { card_number: TEST_CARD } }))).not.toContain(TEST_CARD);
  });

  it("contacts no server when the request is invalid, switched off or unconfigured", async () => {
    const { supplier: target, calls } = supplier(() => Promise.resolve({ result: {} }));
    await expect(target.supply({ origin: "http://shop.example", fields: { card_number: TEST_CARD } })).rejects.toBeInstanceOf(PaymentSupplyError);
    const off = supplier(() => Promise.resolve({ result: {} }), { ...CONFIG, enabled: false });
    await expect(off.supplier.supply({ origin: ORIGIN, fields: { card_number: TEST_CARD } })).rejects.toMatchObject({ code: "POLICY_DENIED" });
    const none = new PaymentSupplier({ config: null, server: { invoke: () => Promise.reject(new Error("called")) }, workspaceId: () => WORKSPACE, newCommandId: () => "cmd_x" });
    await expect(none.supply({ origin: ORIGIN, fields: { card_number: TEST_CARD } })).rejects.toMatchObject({ code: "POLICY_DENIED" });
    expect(calls).toEqual([]);
    expect(off.calls).toEqual([]);
  });
});

interface Call {
  readonly method: string;
  readonly url: string;
  readonly body: Record<string, unknown> | null;
}

const dirs: string[] = [];

function bridge(calls: Call[], answer: (call: Call) => unknown, status = 200, configured = true): DesktopBridge {
  const dir = mkdtempSync(join(tmpdir(), "quansio-payment-"));
  dirs.push(dir);
  return new DesktopBridge({
    serverOrigin: "http://127.0.0.1:8080",
    auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
    session: new SessionFile(dir),
    openSocket: () => {
      throw new Error("no socket in this test");
    },
    updates: new UpdateHooks("stable", null, "http://x", false),
    consumer: "desktop:test",
    push: () => undefined,
    ...(configured ? { localSecrets: CONFIG } : {}),
    fetch: (url, init) => {
      const call: Call = { method: init?.method ?? "GET", url, body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null };
      calls.push(call);
      return Promise.resolve(new Response(JSON.stringify(answer(call)), { status, headers: { "content-type": "application/json" } }));
    },
  });
}

describe("the payment channel", () => {
  it("is allowlisted and is the only way a supply reaches the server", async () => {
    expect(PRELOAD_ALLOWLIST).toContain("payment.supply");
    const calls: Call[] = [];
    const handlers = bridge(calls, () => ({ command_id: "c", accepted_at: "now", replayed: false, result: { supplied: [] } })).handlers();
    await expect(handlers["commands.invoke"]({ name: "SupplyPaymentInput", commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params: { fields: { card_number: TEST_CARD } } })).rejects.toThrow(/payment channel/);
    expect(calls).toEqual([]);
  });

  it("sends the command with main's workspace and a lifetime from configuration, whatever the page asks for", async () => {
    const calls: Call[] = [];
    const handlers = bridge(calls, () => ({ command_id: "c", accepted_at: "now", replayed: false, result: { supplied: [{ field: "card_number", expires_at: "2026-10-07T12:05:00Z" }] } })).handlers();
    const settled = await handlers["payment.supply"]({ origin: ORIGIN, fields: { card_number: TEST_CARD, cvc: TEST_CVC } });
    expect(settled).toEqual({ ok: true, result: { origin: ORIGIN, supplied: [{ field: "card_number", expiresAt: "2026-10-07T12:05:00Z" }] } });
    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toMatch(/SupplyPaymentInput$/);
    expect(calls[0]?.body?.["params"]).toEqual({ workspace_id: WORKSPACE, origin: ORIGIN, fields: { card_number: TEST_CARD, cvc: TEST_CVC }, ttl_seconds: 300 });
    // A page that names another workspace or a longer life is refused before anything is sent.
    for (const extra of [{ workspace_id: "ws_other" }, { ttl_seconds: 900 }, { ttlSeconds: 900 }]) {
      expect(await handlers["payment.supply"]({ origin: ORIGIN, fields: { card_number: TEST_CARD }, ...extra })).toMatchObject({ ok: false, error: { code: "VALIDATION_SCHEMA" } });
    }
    expect(calls).toHaveLength(1);
  });

  it("returns a server refusal as data without the values, and an unconfigured build refuses without a call", async () => {
    const refused: Call[] = [];
    const handlers = bridge(refused, () => ({ code: "POLICY_DENIED", message: "payment inputs are switched off for this deployment", correlation_id: "c", retryable: false }), 403).handlers();
    const settled = await handlers["payment.supply"]({ origin: ORIGIN, fields: { card_number: TEST_CARD } });
    expect(settled).toMatchObject({ ok: false, error: { code: "POLICY_DENIED" } });
    expect(JSON.stringify(settled)).not.toContain(TEST_CARD);
    const calls: Call[] = [];
    const bare = bridge(calls, () => ({}), 200, false).handlers();
    expect(await bare["payment.supply"]({ origin: ORIGIN, fields: { card_number: TEST_CARD } })).toMatchObject({ ok: false, error: { code: "POLICY_DENIED" } });
    expect(calls).toEqual([]);
  });
});

function sources(directory: string): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry);
    if (statSync(path).isDirectory()) {
      found.push(...sources(path));
    } else if (/\.(ts|tsx)$/.test(entry)) {
      found.push(path);
    }
  }
  return found;
}

describe("the structure that keeps a payment value out of the page's and the agent's reach", () => {
  const all = sources(join(ROOT, "apps", "desktop", "src"));
  const read = (path: string): string => readFileSync(path, "utf8");

  it("has one renderer file that supplies, and it keeps nothing the person typed", () => {
    const calling = all.filter((path) => read(path).includes("supplyPayment(")).map((path) => relative(ROOT, path));
    expect(calling).toEqual(["apps/desktop/src/renderer/PaymentInput.tsx"]);
    const source = read(join(ROOT, "apps/desktop/src/renderer/PaymentInput.tsx"));
    // Uncontrolled inputs, cleared before the call is awaited; no storage, no log, no state holding a value.
    expect(source).not.toMatch(/localStorage|sessionStorage|indexedDB|console\./);
    expect(source).not.toMatch(/useState<[^>]*(Record|string)\b[^>]*>\(\s*\{/);
    expect(source.indexOf("form.current.reset()")).toBeGreaterThan(0);
    expect(source.indexOf("form.current.reset()")).toBeLessThan(source.indexOf("await window.quansio.supplyPayment"));
  });

  it("has no channel, bridge method or main-process path that reads a supplied value back", () => {
    const channels = PRELOAD_ALLOWLIST.filter((channel) => /payment/i.test(channel));
    expect(channels).toEqual(["payment.supply"]);
    const bridgeSource = read(join(ROOT, "apps/desktop/src/preload/bridge.ts"));
    expect([...bridgeSource.matchAll(/\b(\w*[Pp]ayment\w*):/g)].map((match) => match[1])).toEqual(["supplyPayment"]);
    // Main never logs the request: the supplier is the only file that names a payment value, and it does not log.
    const supplier = read(join(ROOT, "apps/desktop/src/main/secrets/payments.ts"));
    expect(supplier).not.toMatch(/console\.|\blog\(/);
  });
});
