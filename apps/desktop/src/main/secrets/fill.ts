/**
 * Filling a saved login into the page the person is looking at (ADP-009, DOMAIN.md §13.9 A).
 *
 * The plaintext leaves the vault at one place: {@link PageWriter.fill}, called here after the page's origin has been checked
 * **three times, across the asynchronous steps between them**:
 *
 * 1. when the request arrives (the page the person sees, and the frame the fields are in);
 * 2. after the confirmation, when the saved login belongs to another origin than the page (the dialog is a person's pause: the
 *    page may have navigated while it was open);
 * 3. immediately before the text is handed to the writer, after the password was decrypted.
 *
 * The page's identity is `(top origin, frame origin, document revision)`; if any of the three differs from the first reading, or
 * the first reading is older than `fill_max_age_ms`, nothing is inserted (`CONFLICT_STATE`, `origin_changed`). A same-origin fill
 * needs no dialog; a fill of a login saved for another origin is shown to the person with both origins and happens only on their
 * explicit yes (the default button is "Cancel"). No agent tool reads or fills from the vault: this file is reachable only from the
 * `passwords.fill` channel, which is main-window-only and answers `{ok, crossSite}` — never the password.
 */

import type { ManagedWindow } from "../windows.js";
import { sameOrigin } from "./origins.js";
import { type PasswordVault, VaultError } from "./vault.js";
import type { LocalSecretsConfig } from "./config.js";
import { Secret } from "./secret.js";

/** What main knows about the page of one live session. */
export interface PageView {
  readonly sessionId: string;
  /** The top frame's origin, or `null` for a page that has none (`about:blank`). */
  readonly topOrigin: string | null;
  /** The origin of the frame the login fields are in. */
  readonly frameOrigin: string | null;
  /** The document revision the live view last reported (DOMAIN.md §8.4.1 A). */
  readonly documentRevision: string;
}

/** Reads the page of a live session. The production reading is the live view's last browser-session projection. */
export interface PageProbe {
  view(sessionId: string): Promise<PageView | null>;
}

/**
 * Puts text into the login fields of the page. It is the one place a plaintext leaves main. The production binding is the
 * takeover input path (client → server → worker → CDP), which this build does not have; until it does the writer is absent and a
 * fill answers `ADAPTER_UNAVAILABLE` rather than pretending.
 */
export interface PageWriter {
  fill(view: PageView, username: Secret, password: Secret): Promise<{ readonly filled: boolean }>;
}

/** Asks the person. `main` supplies Electron's message box; the default answer is no. */
export interface CrossSiteConfirmation {
  confirm(
    window: ManagedWindow,
    ask: { readonly entryOrigin: string; readonly username: string; readonly pageOrigin: string },
    ttlMs: number,
  ): Promise<boolean>;
}

export type FillAnswer =
  | { readonly ok: true; readonly filled: true; readonly crossSite: boolean }
  | { readonly ok: false; readonly error: { readonly code: string; readonly message: string; readonly details?: { readonly reason: string } } };

function refuse(code: string, message: string, reason?: string): FillAnswer {
  return { ok: false, error: { code, message, ...(reason === undefined ? {} : { details: { reason } }) } };
}

function same(a: PageView, b: PageView): boolean {
  return a.sessionId === b.sessionId && a.topOrigin === b.topOrigin && a.frameOrigin === b.frameOrigin && a.documentRevision === b.documentRevision;
}

export interface FillerOptions {
  readonly vault: PasswordVault;
  readonly config: LocalSecretsConfig;
  readonly probe: PageProbe;
  /** Absent: the takeover input path is not built, and every fill is `ADAPTER_UNAVAILABLE`. */
  readonly writer: PageWriter | null;
  readonly confirmation: CrossSiteConfirmation;
  readonly now?: () => number;
}

export class PasswordFiller {
  readonly #o: FillerOptions;

  constructor(options: FillerOptions) {
    this.#o = options;
  }

  async fill(window: ManagedWindow, request: { readonly id: string; readonly sessionId: string }): Promise<FillAnswer> {
    const now = this.#o.now ?? Date.now;
    const entry = this.#o.vault.get(request.id);
    if (entry === null) {
      return refuse("NOT_FOUND", "no saved login has this id");
    }
    const writer = this.#o.writer;
    if (writer === null) {
      return refuse("ADAPTER_UNAVAILABLE", "this build has no way to type into the managed browser's page", "page_input");
    }
    // 1. The page, as the person sees it now.
    const first = await this.#o.probe.view(request.sessionId);
    const readAt = now();
    if (first === null || first.frameOrigin === null) {
      return refuse("CONFLICT_STATE", "there is no web page to fill", "no_page");
    }
    const crossSite = !sameOrigin(entry.origin, first.frameOrigin) || (first.topOrigin !== null && !sameOrigin(first.topOrigin, first.frameOrigin));
    // 2. A login saved for another origin (or a field in a frame that is not the page) is filled only on the person's yes.
    if (crossSite) {
      const yes = await this.#o.confirmation.confirm(
        window,
        { entryOrigin: entry.origin, username: entry.username, pageOrigin: first.frameOrigin },
        this.#o.config.confirmTtlMs,
      );
      if (!yes) {
        return refuse("POLICY_DENIED", "the person did not confirm filling a login saved for another site", "declined");
      }
      const afterDialog = await this.#o.probe.view(request.sessionId);
      if (afterDialog === null || !same(first, afterDialog)) {
        return refuse("CONFLICT_STATE", "the page changed while the question was open", "origin_changed");
      }
    }
    if (now() - readAt > this.#o.config.fillMaxAgeMs) {
      return refuse("CONFLICT_STATE", "the page was read too long ago to fill", "stale_probe");
    }
    // 3. Decrypt, then look at the page once more, in the same task as the insertion.
    let password: Secret;
    try {
      password = this.#o.vault.password(request.id);
    } catch (error) {
      if (error instanceof VaultError) {
        return refuse("NOT_FOUND", "no saved login has this id");
      }
      throw error;
    }
    const username = new Secret(entry.username);
    try {
      const last = await this.#o.probe.view(request.sessionId);
      if (last === null || !same(first, last)) {
        return refuse("CONFLICT_STATE", "the page changed before the text was inserted", "origin_changed");
      }
      const written = await writer.fill(last, username, password);
      return written.filled ? { ok: true, filled: true, crossSite } : refuse("CONFLICT_STATE", "the page did not take the login", "not_filled");
    } finally {
      password.wipe();
      username.wipe();
    }
  }
}
