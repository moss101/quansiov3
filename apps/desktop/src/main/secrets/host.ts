/**
 * The password manager and browser-data import channels (ADP-009, DOMAIN.md §13.9 A, B).
 *
 * One object that answers the `passwords.*` and `import.*` channels. It owns no authority and imports no Electron: `app.ts` supplies the
 * encrypted file, the native questions and the server call. Three rules hold for every channel, and each has a test:
 *
 * * **Main window only.** `ipc-trust.ts` lists every channel here for the main window and no other: a pop-out, a quick-entry window and the
 *   indicator reach none of them.
 * * **No password in any answer.** A list, a suggestion and a save answer with metadata (`id`, origin, username, times); the only way to a
 *   password is `PasswordVault.password`, called by the fill path inside main. There is no channel that returns one, and no agent tool.
 * * **Every payload is validated** against a schema (`schema.ts`) before a handler runs, and the answers are data: `{ok, result}` or
 *   `{ok: false, error}`. A refusal carries a §15 code and, where it helps, `details.reason`; it never carries a value.
 */

import type { SecretChannel } from "../../preload/bridge.js";
import type { WindowHandler } from "../ipc-trust.js";
import * as s from "../schema.js";
import type { ManagedWindow } from "../windows.js";
import type { LocalSecretsConfig } from "./config.js";
import type { PasswordFiller } from "./fill.js";
import type { BrowserImportService } from "./import.js";
import { originOf } from "./origins.js";
import { type PasswordVault, VaultError } from "./vault.js";

type Handlers = Readonly<Record<SecretChannel, WindowHandler>>;

interface Refusal {
  readonly ok: false;
  readonly error: { readonly code: string; readonly message: string; readonly retryable: false; readonly details: Readonly<Record<string, unknown>> | null };
}

function refuse(code: string, message: string, reason?: string): Refusal {
  return { ok: false, error: { code, message, retryable: false, details: reason === undefined ? null : { reason } } };
}

const done = <T>(result: T): { readonly ok: true; readonly result: T } => ({ ok: true, result });

const VAULT_CODES: Readonly<Record<string, string>> = {
  unavailable: "ADAPTER_UNAVAILABLE",
  locked: "CONFLICT_STATE",
  invalid_origin: "VALIDATION_SCHEMA",
  empty: "VALIDATION_SCHEMA",
  too_long: "VALIDATION_SCHEMA",
  too_many: "VALIDATION_SCHEMA",
  not_found: "NOT_FOUND",
};

export interface SecretsHostOptions {
  readonly config: LocalSecretsConfig;
  readonly vault: PasswordVault;
  readonly filler: PasswordFiller;
  readonly importer: BrowserImportService;
}

const SESSION_ID = s.string({ min: 30, max: 30, pattern: /^bsn_[0-9A-HJKMNP-TV-Z]{26}$/ });
const ENTRY_ID = s.string({ min: 35, max: 35, pattern: /^pw_[0-9a-f]{32}$/ });
const FAMILY = s.string({ min: 1, max: 16, pattern: /^[a-z]+$/ });
const PROFILE = s.string({ min: 1, max: 32, pattern: /^(Default|Profile \d{1,3})$/ });

export class SecretsHost {
  readonly #o: SecretsHostOptions;

  constructor(options: SecretsHostOptions) {
    this.#o = options;
  }

  handlers(): Handlers {
    const o = this.#o;
    const max = o.config.fieldMaxChars;
    const guarded =
      <T>(schema: s.Schema<T>, run: (window: ManagedWindow, input: T) => unknown): WindowHandler =>
      (window, payload) => {
        if (!o.config.enabled) {
          return Promise.resolve(refuse("POLICY_DENIED", "the password manager and imports are switched off"));
        }
        const parsed = s.safeParse(schema, payload);
        if (!parsed.ok) {
          return Promise.resolve(refuse("VALIDATION_SCHEMA", parsed.error.message));
        }
        return Promise.resolve().then(() => run(window, parsed.value)).catch((error: unknown) => {
          if (error instanceof VaultError) {
            return refuse(VAULT_CODES[error.reason] ?? "INTERNAL", error.message, error.reason);
          }
          throw error;
        });
      };

    const NOTHING = s.empty();
    const SAVE = s.object<{ origin: string; username: string; password: string }>({
      origin: s.string({ min: 1, max: 2048 }),
      username: s.string({ min: 1, max }),
      password: s.string({ min: 1, max }),
    });
    const SUGGEST = s.object<{ pageUrl: string }>({ pageUrl: s.string({ min: 1, max: 2048 }) });
    const ID = s.object<{ id: string }>({ id: ENTRY_ID });
    const FILL = s.object<{ id: string; sessionId: string }>({ id: ENTRY_ID, sessionId: SESSION_ID });
    const PROFILE_REQUEST = s.object<{ family: string; profile: string }>({ family: FAMILY, profile: PROFILE });
    const COOKIES_REQUEST = s.object<{ family: string; profile: string; domains: readonly string[] }>({
      family: FAMILY,
      profile: PROFILE,
      domains: s.array(s.string({ min: 1, max: 253 }), 50),
    });

    return {
      "passwords.status": guarded(NOTHING, () =>
        done({ available: o.vault.available(), locked: o.vault.locked(), count: o.vault.count(), max: o.config.passwordsMax }),
      ),
      "passwords.list": guarded(NOTHING, () => done({ entries: o.vault.list() })),
      "passwords.suggest": guarded(SUGGEST, (_window, input) => {
        const origin = originOf(input.pageUrl);
        return done({ origin, entries: origin === null ? [] : o.vault.suggest(origin) });
      }),
      // The password is in the payload, in this one direction, and is not in any answer.
      "passwords.save": guarded(SAVE, (_window, input) => done({ entry: o.vault.save(input) })),
      "passwords.remove": guarded(ID, (_window, input) => done({ removed: o.vault.remove(input.id) })),
      "passwords.fill": guarded(FILL, (window, input) => o.filler.fill(window, input)),
      "import.scan": guarded(NOTHING, () => o.importer.scan()),
      "import.logins": guarded(PROFILE_REQUEST, (window, input) => o.importer.importLogins(window, input)),
      "import.cookies": guarded(COOKIES_REQUEST, (window, input) => o.importer.stageCookies(window, input)),
    };
  }
}
