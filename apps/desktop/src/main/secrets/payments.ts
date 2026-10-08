/**
 * The person's side of a payment-field flow (ADP-009, DOMAIN.md 13.9 C).
 *
 * To let the agent check out, a person types the card and address values into the desktop, once, for one origin. They travel
 * main -> server as the `SupplyPaymentInput` command and nowhere else: not the model's context, not a thread, not the command
 * log (the server digests the params without their values), not this process's logs and not the answer, which names fields
 * and expiry times only. There is no wallet and no saved card; the server seals each value for one use and overwrites it when
 * the agent's `browser.fill_payment_field` takes it or when its time runs out.
 *
 * Everything here is validation and a single invoke. A JavaScript string cannot be wiped, so the only defence on this side is
 * to hold the values for the length of one call, to never put one in an error message and to give the page no way to read
 * them back.
 */

import type { LocalSecretsConfig } from "./config.js";

/** The fields a person may supply, in the order the tool's schema lists them (a test holds this to the runtime's list). */
export const PAYMENT_FIELDS = [
  "card_number",
  "cvc",
  "expiry",
  "expiry_month",
  "expiry_year",
  "card_holder_name",
  "address_line",
  "city",
  "region",
  "postal_code",
  "country",
] as const;

export type PaymentFieldName = (typeof PAYMENT_FIELDS)[number];

/** A refusal that carries no value: its message names a field or a rule, never what was typed. */
export class PaymentSupplyError extends Error {
  readonly code: "VALIDATION_SCHEMA" | "POLICY_DENIED";

  constructor(code: "VALIDATION_SCHEMA" | "POLICY_DENIED", message: string) {
    super(message);
    this.name = "PaymentSupplyError";
    this.code = code;
  }
}

/** What a valid request comes to. */
export interface ParsedSupply {
  readonly origin: string;
  readonly fields: Readonly<Record<PaymentFieldName, string>>;
  readonly ttlSeconds: number;
}

function refuse(message: string): never {
  throw new PaymentSupplyError("VALIDATION_SCHEMA", message);
}

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);

/** The origin a request is for: `https://host[:port]`, or plain `http` on a loopback host (a local test page). */
export function paymentOrigin(raw: unknown): string {
  if (typeof raw !== "string" || raw.length === 0 || raw.length > 2048) {
    return refuse("origin is required");
  }
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return refuse("origin is not a URL");
  }
  const secure = url.protocol === "https:";
  if (!secure && !(url.protocol === "http:" && LOOPBACK.has(url.hostname))) {
    return refuse("payment values are for an https origin");
  }
  if (url.username !== "" || url.password !== "" || url.origin !== raw.replace(/\/$/, "")) {
    return refuse("origin is an origin: a scheme, a host and a port, and nothing else");
  }
  return url.origin;
}

/** Validate a request from the page. Throws {@link PaymentSupplyError}; no message holds a value. */
export function parsePaymentSupply(raw: unknown, config: LocalSecretsConfig): ParsedSupply {
  if (!config.enabled) {
    throw new PaymentSupplyError("POLICY_DENIED", "payment inputs are switched off");
  }
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    return refuse("a payment supply is an object");
  }
  const body = raw as Record<string, unknown>;
  for (const key of Object.keys(body)) {
    if (key !== "origin" && key !== "fields") {
      return refuse(`${key} is not a payment supply key`);
    }
  }
  const origin = paymentOrigin(body["origin"]);
  const given = body["fields"];
  if (given === null || typeof given !== "object" || Array.isArray(given)) {
    return refuse("fields is an object of payment field to value");
  }
  const entries = Object.entries(given as Record<string, unknown>);
  if (entries.length === 0 || entries.length > config.paymentMaxFields) {
    return refuse(`a supply carries 1 to ${String(config.paymentMaxFields)} fields`);
  }
  const fields: Partial<Record<PaymentFieldName, string>> = {};
  for (const [name, value] of entries) {
    if (!(PAYMENT_FIELDS as readonly string[]).includes(name)) {
      return refuse(`${name} is not a payment field`);
    }
    if (typeof value !== "string") {
      return refuse(`the value for ${name} is text`);
    }
    if (value.length === 0 || Array.from(value).length > config.paymentValueMaxChars) {
      return refuse(`the value for ${name} is 1 to ${String(config.paymentValueMaxChars)} characters`);
    }
    fields[name as PaymentFieldName] = value;
  }
  return { origin, fields: fields as Record<PaymentFieldName, string>, ttlSeconds: Math.floor(config.paymentTtlMs / 1000) };
}

/** What the server answers, and what the page may see of it: fields and times, never a value. */
export interface PaymentSupplied {
  readonly origin: string;
  readonly supplied: readonly { readonly field: string; readonly expiresAt: string }[];
}

/** The slice of the server client the supply uses. */
export interface PaymentInvoker {
  invoke(name: string, commandId: string, params: Record<string, unknown>): Promise<{ readonly result: unknown }>;
}

export interface PaymentSupplierOptions {
  readonly config: LocalSecretsConfig | null;
  readonly server: PaymentInvoker;
  readonly workspaceId: () => string;
  readonly newCommandId: () => string;
}

/** Sends a person's payment values to the server for one checkout. */
export class PaymentSupplier {
  readonly #options: PaymentSupplierOptions;

  constructor(options: PaymentSupplierOptions) {
    this.#options = options;
  }

  /** Validate, send `SupplyPaymentInput`, and answer with names and times. A server refusal propagates as it is. */
  async supply(raw: unknown): Promise<PaymentSupplied> {
    const config = this.#options.config;
    if (config === null) {
      throw new PaymentSupplyError("POLICY_DENIED", "payment inputs are not configured in this build");
    }
    const parsed = parsePaymentSupply(raw, config);
    const response = await this.#options.server.invoke("SupplyPaymentInput", this.#options.newCommandId(), {
      workspace_id: this.#options.workspaceId(),
      origin: parsed.origin,
      fields: { ...parsed.fields },
      ttl_seconds: parsed.ttlSeconds,
    });
    return decodeSupplied(response.result, parsed.origin);
  }
}

function decodeSupplied(result: unknown, origin: string): PaymentSupplied {
  const body = result !== null && typeof result === "object" ? (result as Record<string, unknown>) : {};
  const list = Array.isArray(body["supplied"]) ? (body["supplied"] as unknown[]) : [];
  const supplied = list.flatMap((entry) => {
    const item = entry !== null && typeof entry === "object" ? (entry as Record<string, unknown>) : {};
    const field = item["field"];
    const expiresAt = item["expires_at"];
    return typeof field === "string" && typeof expiresAt === "string" ? [{ field, expiresAt }] : [];
  });
  return { origin, supplied };
}
