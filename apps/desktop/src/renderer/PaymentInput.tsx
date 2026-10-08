/**
 * The person's one-time payment values (ADP-009, DOMAIN.md 13.9 C).
 *
 * A form on the live session: the person types a card and an address for the site the session is on, and the page sends them
 * to main in one call. Main sends them to the runtime, which seals each for one use. This component keeps the values in
 * uncontrolled inputs it clears the moment the request is sent, shows only field names and expiry times in the answer, and
 * has no way to read a value back, because no channel returns one. The agent never sees the form or its contents.
 */

import { useRef, useState, type ReactElement, type SyntheticEvent } from "react";

import { t, type StringKey } from "../i18n/strings.js";

interface PaymentInputProps {
  /** The URL of the page the session is on; only its origin is used. */
  readonly pageUrl: string | null;
}

const FIELDS: readonly { readonly name: string; readonly label: StringKey; readonly numeric: boolean }[] = [
  { name: "card_number", label: "paymentCardNumber", numeric: true },
  { name: "cvc", label: "paymentCvc", numeric: true },
  { name: "expiry_month", label: "paymentExpiryMonth", numeric: true },
  { name: "expiry_year", label: "paymentExpiryYear", numeric: true },
  { name: "card_holder_name", label: "paymentHolder", numeric: false },
  { name: "address_line", label: "paymentAddress", numeric: false },
  { name: "city", label: "paymentCity", numeric: false },
  { name: "region", label: "paymentRegion", numeric: false },
  { name: "postal_code", label: "paymentPostal", numeric: false },
  { name: "country", label: "paymentCountry", numeric: false },
];

/** The origin of a page URL when it is one a payment may be supplied for (https, or loopback http for a local test page). */
export function originOfPage(pageUrl: string | null): string | null {
  if (pageUrl === null) {
    return null;
  }
  try {
    const url = new URL(pageUrl);
    const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
    return url.protocol === "https:" || (url.protocol === "http:" && loopback) ? url.origin : null;
  } catch {
    return null;
  }
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: { readonly supplied?: readonly { readonly field: string; readonly expiresAt: string }[] };
  readonly error?: { readonly code: string; readonly message: string };
}

export function PaymentInput({ pageUrl }: PaymentInputProps): ReactElement {
  const origin = originOfPage(pageUrl);
  const form = useRef<HTMLFormElement | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  async function submit(event: SyntheticEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (origin === null || form.current === null) {
      return;
    }
    const fields: Record<string, string> = {};
    for (const field of FIELDS) {
      const input = form.current.elements.namedItem(field.name);
      if (input instanceof HTMLInputElement && input.value !== "") {
        fields[field.name] = input.value;
      }
    }
    if (Object.keys(fields).length === 0) {
      setNotice(t("paymentNothing"));
      return;
    }
    // The values leave the form before the call is awaited: whatever happens next, the page no longer holds them.
    form.current.reset();
    try {
      const settled = (await window.quansio.supplyPayment({ origin, fields })) as SettledPayload;
      if (!settled.ok) {
        setNotice(`${t("refused")} ${settled.error?.code ?? "INTERNAL"}: ${settled.error?.message ?? ""}`);
        return;
      }
      const first = settled.result?.supplied?.[0];
      setNotice(`${t("paymentSupplied")} ${origin}${first ? ` · ${t("paymentExpires")} ${first.expiresAt}` : ""}`);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }

  return (
    <section className="live-payment" aria-label={t("paymentTitle")}>
      <h3>{t("paymentTitle")}</h3>
      <p className="stage">{t("paymentLead")}</p>
      {origin === null ? (
        <p className="stage">{t("paymentNoOrigin")}</p>
      ) : (
        <>
          <p className="stage">
            {t("paymentOrigin")} <span className="live-untrusted">{origin}</span>
          </p>
          <form
            ref={form}
            autoComplete="off"
            onSubmit={(event) => {
              void submit(event);
            }}
          >
            {FIELDS.map((field) => (
              <label key={field.name}>
                {t(field.label)}
                <input name={field.name} type="text" inputMode={field.numeric ? "numeric" : "text"} autoComplete="off" spellCheck={false} maxLength={128} />
              </label>
            ))}
            <button type="submit">{t("paymentSupply")}</button>
          </form>
        </>
      )}
      {notice ? <p className="notice">{notice}</p> : null}
    </section>
  );
}
