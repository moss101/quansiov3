/**
 * The questions the password manager and the import ask (ADP-009, DOMAIN.md §13.9 A, B).
 *
 * Built here, apart from Electron's `dialog`, so a test can hold them to what the contract says: the default button and the
 * Escape button are **Cancel** (a dialog that is closed, ignored or dismissed fills nothing and imports nothing), and the words
 * name both origins or the browser, the profile and the counts, with no value in them.
 */

import { fill, t } from "../../i18n/strings.js";
import type { MessageBox } from "../dialogs.js";

export interface CrossSiteAsk {
  readonly entryOrigin: string;
  readonly username: string;
  readonly pageOrigin: string;
}

export function crossSiteDialog(ask: CrossSiteAsk): MessageBox {
  return {
    title: t("fillCrossSiteTitle"),
    message: fill("fillCrossSiteMessage", { username: ask.username, entryOrigin: ask.entryOrigin, pageOrigin: ask.pageOrigin }),
    detail: t("fillCrossSiteDetail"),
    buttons: [t("secretsCancel"), t("fillCrossSiteConfirm")],
    defaultId: 0,
    cancelId: 0,
  };
}

export interface ImportAsk {
  readonly kind: "logins" | "cookies";
  readonly browser: string;
  readonly profile: string;
  readonly count: number;
  /** The domains a cookie import covers (a few, already truncated). */
  readonly domains: readonly string[];
  readonly moreDomains: number;
}

export function importDialog(ask: ImportAsk): MessageBox {
  if (ask.kind === "logins") {
    return {
      title: t("importLoginsTitle"),
      message: fill("importLoginsMessage", { count: ask.count, browser: ask.browser, profile: ask.profile }),
      detail: t("importLoginsDetail"),
      buttons: [t("secretsCancel"), t("importLoginsConfirm")],
      defaultId: 0,
      cancelId: 0,
    };
  }
  const listed = ask.moreDomains > 0 ? `${ask.domains.join(", ")} ${fill("importMoreDomains", { count: ask.moreDomains })}` : ask.domains.join(", ");
  return {
    title: t("importCookiesTitle"),
    message: fill("importCookiesMessage", { count: ask.count, domains: listed, browser: ask.browser, profile: ask.profile }),
    detail: t("importCookiesDetail"),
    buttons: [t("secretsCancel"), t("importCookiesConfirm")],
    defaultId: 0,
    cancelId: 0,
  };
}

/** Whether the person chose the second button, the only one that proceeds. */
export function proceeds(response: number): boolean {
  return response === 1;
}
