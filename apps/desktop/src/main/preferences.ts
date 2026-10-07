/**
 * The person's own shell choices (ADP-004, DOMAIN.md §13.6): shortcut bindings, keep-awake, whether an update
 * installs when idle or on quit, and where the activity indicator sits on each display.
 *
 * A small JSON file under the app's user-data directory, written atomically with owner-only permissions, in the
 * way `session.json` is. It is a client preference, not authority and not a store the server reads (D-019):
 * nothing in it is a credential, and a file that is missing, unreadable or does not match its schema is read as
 * "the person has chosen nothing", never as an error that stops the app.
 */

import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import * as s from "./schema.js";

export interface IndicatorPosition {
  readonly x: number;
  readonly y: number;
}

export interface ShellPreferences {
  readonly version: 1;
  /** Binding by shortcut id. Absent: the default applies. `null`: the person cleared it, so none is registered. */
  readonly shortcuts: Readonly<Record<string, string | null>>;
  /** Keep-awake: `null` until the person chooses, then the configured default applies. */
  readonly keepAwake: boolean | null;
  readonly installWhenIdle: boolean | null;
  readonly installOnQuit: boolean;
  /** The indicator's top-left corner relative to its display, by display key. */
  readonly indicator: Readonly<Record<string, IndicatorPosition>>;
}

export const DEFAULT_PREFERENCES: ShellPreferences = {
  version: 1,
  shortcuts: {},
  keepAwake: null,
  installWhenIdle: null,
  installOnQuit: false,
  indicator: {},
};

const SHORTCUT_ID = /^[a-z][a-z_]{0,31}$/;
const DISPLAY_KEY = /^[A-Za-z0-9:_-]{1,64}$/;

const FILE_SCHEMA = s.object<ShellPreferences>({
  version: s.literal(1),
  shortcuts: s.record(s.nullable(s.string({ min: 1, max: 64 })), SHORTCUT_ID, 16),
  keepAwake: s.nullable(s.boolean()),
  installWhenIdle: s.nullable(s.boolean()),
  installOnQuit: s.boolean(),
  indicator: s.record(
    s.object<IndicatorPosition>({ x: s.integer({ min: -100000, max: 100000 }), y: s.integer({ min: -100000, max: 100000 }) }),
    DISPLAY_KEY,
    16,
  ),
});

/** Parse the file's text; anything that does not match is the defaults. */
export function parsePreferences(text: string | null): ShellPreferences {
  if (text === null) {
    return DEFAULT_PREFERENCES;
  }
  try {
    const parsed = s.safeParse(FILE_SCHEMA, JSON.parse(text) as unknown, "preferences");
    return parsed.ok ? parsed.value : DEFAULT_PREFERENCES;
  } catch {
    return DEFAULT_PREFERENCES;
  }
}

/** The preferences file. */
export class PreferencesFile {
  readonly #path: string;
  #current: ShellPreferences;

  constructor(userData: string, name = "shell-preferences.json") {
    this.#path = join(userData, name);
    let text: string | null = null;
    try {
      text = readFileSync(this.#path, "utf8");
    } catch {
      text = null;
    }
    this.#current = parsePreferences(text);
  }

  current(): ShellPreferences {
    return this.#current;
  }

  /** Apply a change and write it; the in-memory value changes only when the write succeeded. */
  update(change: (previous: ShellPreferences) => ShellPreferences): ShellPreferences {
    const next = change(this.#current);
    mkdirSync(dirname(this.#path), { recursive: true, mode: 0o700 });
    const temporary = `${this.#path}.tmp`;
    writeFileSync(temporary, `${JSON.stringify(next, null, 2)}\n`, { mode: 0o600 });
    renameSync(temporary, this.#path);
    this.#current = next;
    return next;
  }
}
