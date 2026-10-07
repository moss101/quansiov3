/**
 * The grammar of a keyboard binding (ADP-004, DOMAIN.md §13.6 D).
 *
 * A binding is a lower-case string `[mod+][ctrl+][alt+][shift+]<key>` with the modifiers in exactly that order,
 * at most `maxLength` characters. `mod` is Command on macOS and Control elsewhere, so it never appears with
 * `ctrl`. The key is a single letter or digit, a function key or a name from {@link KEY_NAMES}; punctuation is
 * named (`plus`, `comma`) so `+` never needs escaping. One spelling per binding means two bindings are equal
 * exactly when their strings are, which is what conflict detection compares.
 *
 * Pure and shared: the main process validates every binding it stores or registers, and the renderer builds
 * one from a key event while the person is recording.
 */

/** Names accepted for keys that are not a single letter or digit; the right side is Electron's accelerator key. */
export const KEY_NAMES: Readonly<Record<string, string>> = {
  space: "Space",
  enter: "Return",
  tab: "Tab",
  escape: "Escape",
  backspace: "Backspace",
  delete: "Delete",
  insert: "Insert",
  up: "Up",
  down: "Down",
  left: "Left",
  right: "Right",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
  plus: "Plus",
  minus: "-",
  equal: "=",
  comma: ",",
  period: ".",
  slash: "/",
  backslash: "\\",
  semicolon: ";",
  quote: "'",
  backquote: "`",
  bracketleft: "[",
  bracketright: "]",
};

export const MODIFIER_ORDER = ["mod", "ctrl", "alt", "shift"] as const;
export type Modifier = (typeof MODIFIER_ORDER)[number];

/** A parsed binding. */
export interface Binding {
  readonly modifiers: readonly Modifier[];
  readonly key: string;
}

const FUNCTION_KEY = /^f([1-9]|1\d|2[0-4])$/;

/** Whether `key` is a key the grammar knows. */
export function isKnownKey(key: string): boolean {
  return /^[a-z0-9]$/.test(key) || FUNCTION_KEY.test(key) || Object.hasOwn(KEY_NAMES, key);
}

export type BindingProblem =
  | "empty"
  | "too_long"
  | "not_lower_case"
  | "bad_modifier_order"
  | "mod_with_ctrl"
  | "unknown_key"
  | "needs_modifier";

/** Why `text` is not a binding, or `null` when it is one. */
export function bindingProblem(text: string, maxLength: number): BindingProblem | null {
  if (text.length === 0) {
    return "empty";
  }
  if (text.length > maxLength) {
    return "too_long";
  }
  if (text !== text.toLowerCase() || /\s/.test(text)) {
    return "not_lower_case";
  }
  const parts = text.split("+");
  const key = parts[parts.length - 1] ?? "";
  const modifiers = parts.slice(0, -1);
  // `plus` is the name of the + key, so a trailing empty part (a literal "+") is never a key.
  if (modifiers.includes("mod") && modifiers.includes("ctrl")) {
    return "mod_with_ctrl";
  }
  let last = -1;
  for (const modifier of modifiers) {
    // An unknown modifier has no position (-1), which is never past the last one: refused with the rest.
    const position = MODIFIER_ORDER.indexOf(modifier as Modifier);
    if (position <= last) {
      return "bad_modifier_order";
    }
    last = position;
  }
  if (!isKnownKey(key)) {
    return "unknown_key";
  }
  if (modifiers.length === 0 && !FUNCTION_KEY.test(key)) {
    return "needs_modifier";
  }
  return null;
}

/** Parse a binding; `null` for anything {@link bindingProblem} refuses. */
export function parseBinding(text: string, maxLength: number): Binding | null {
  if (bindingProblem(text, maxLength) !== null) {
    return null;
  }
  const parts = text.split("+");
  return { modifiers: parts.slice(0, -1) as Modifier[], key: parts[parts.length - 1] ?? "" };
}

/** The platform spelling for a modifier in Electron's accelerator syntax. */
function acceleratorModifier(modifier: Modifier, platform: string): string {
  switch (modifier) {
    case "mod":
      return platform === "darwin" ? "Command" : "Control";
    case "ctrl":
      return "Control";
    case "alt":
      return platform === "darwin" ? "Option" : "Alt";
    case "shift":
      return "Shift";
  }
}

/** The Electron accelerator for a valid binding (`mod+alt+q` is `Command+Option+Q` on macOS). */
export function toAccelerator(binding: Binding, platform: string): string {
  const key = Object.hasOwn(KEY_NAMES, binding.key) ? (KEY_NAMES[binding.key] ?? binding.key) : binding.key.toUpperCase();
  return [...binding.modifiers.map((modifier) => acceleratorModifier(modifier, platform)), key].join("+");
}

/** The slice of a DOM `KeyboardEvent` a binding is built from. */
export interface KeyEventLike {
  readonly code: string;
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
}

const CODE_NAMES: Readonly<Record<string, string>> = {
  Space: "space",
  Enter: "enter",
  Tab: "tab",
  Escape: "escape",
  Backspace: "backspace",
  Delete: "delete",
  Insert: "insert",
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
  Home: "home",
  End: "end",
  PageUp: "pageup",
  PageDown: "pagedown",
  Equal: "equal",
  Minus: "minus",
  Comma: "comma",
  Period: "period",
  Slash: "slash",
  Backslash: "backslash",
  Semicolon: "semicolon",
  Quote: "quote",
  Backquote: "backquote",
  BracketLeft: "bracketleft",
  BracketRight: "bracketright",
};

/**
 * A binding from a key event, or `null` while only modifiers are down or the key is not one the grammar has.
 * The physical key (`code`) is used, not the character, so Option on macOS does not turn `q` into another glyph.
 * On macOS Command is `mod` and Control stays `ctrl`; elsewhere Control is `mod` and the Windows key is refused.
 */
export function bindingFromKeyEvent(event: KeyEventLike, platform: string, maxLength: number): string | null {
  let key: string | undefined;
  const letter = /^Key([A-Z])$/.exec(event.code);
  const digit = /^Digit([0-9])$/.exec(event.code);
  if (letter?.[1]) {
    key = letter[1].toLowerCase();
  } else if (digit?.[1]) {
    key = digit[1];
  } else if (/^F([1-9]|1\d|2[0-4])$/.test(event.code)) {
    key = event.code.toLowerCase();
  } else {
    key = CODE_NAMES[event.code];
  }
  if (key === undefined) {
    return null;
  }
  const mac = platform === "darwin";
  if (!mac && event.metaKey) {
    return null;
  }
  const modifiers: Modifier[] = [];
  if (mac ? event.metaKey : event.ctrlKey) {
    modifiers.push("mod");
  }
  if (mac && event.ctrlKey) {
    modifiers.push("ctrl");
  }
  if (event.altKey) {
    modifiers.push("alt");
  }
  if (event.shiftKey) {
    modifiers.push("shift");
  }
  const text = [...modifiers, key].join("+");
  return bindingProblem(text, maxLength) === null ? text : null;
}

/** How a binding reads to a person on this platform: `mod+alt+q` is `⌘⌥Q` on macOS and `Ctrl+Alt+Q` elsewhere. */
export function displayBinding(text: string, platform: string): string {
  const parts = text.split("+");
  const key = parts[parts.length - 1] ?? "";
  const names = (modifier: string): string => {
    if (platform === "darwin") {
      return { mod: "⌘", ctrl: "⌃", alt: "⌥", shift: "⇧" }[modifier] ?? modifier;
    }
    return { mod: "Ctrl", ctrl: "Ctrl", alt: "Alt", shift: "Shift" }[modifier] ?? modifier;
  };
  const shown = Object.hasOwn(KEY_NAMES, key) ? (KEY_NAMES[key] ?? key) : key.toUpperCase();
  const prefix = parts.slice(0, -1).map(names);
  return platform === "darwin" ? [...prefix, shown].join("") : [...prefix, shown].join("+");
}
