/**
 * The application menu (ADP-004, DOMAIN.md §13.6 D) as data.
 *
 * `buildMenu` turns the current state (platform, the person's bindings, which kind of window is focused, whether
 * a shortcut is being recorded) into a platform-neutral description; `app.ts` maps it onto an Electron menu. The
 * menu is rebuilt whenever a binding changes, a window gains focus or recording starts or ends, so its
 * accelerators are always the bindings in force. While a binding is recorded the rebindable accelerators are
 * dropped (the key the person presses must reach the page), and the fixed ones come from the same table the
 * conflict check reads (`FIXED_BINDINGS`), so the menu and the check cannot disagree.
 */

import { t } from "../i18n/strings.js";
import { parseBinding, toAccelerator } from "../shortcut-grammar.js";
import type { ShellCommand } from "../shell-command.js";
import { FIXED_BINDINGS, type WindowShortcutId } from "./shortcuts.js";
import type { WindowType } from "./windows.js";

export type { ShellCommand };

/** What choosing a menu item does. */
export type MenuAction = { readonly kind: "command"; readonly command: ShellCommand } | { readonly kind: "check-for-updates" } | { readonly kind: "quit" };

export type MenuRole = "hide" | "hideOthers" | "unhide" | "services" | "about" | "minimize" | "close" | "zoom" | "front" | "undo" | "redo" | "cut" | "copy" | "paste" | "selectAll" | "zoomIn" | "zoomOut" | "resetZoom" | "togglefullscreen";

export interface MenuItem {
  readonly label?: string;
  readonly role?: MenuRole;
  readonly accelerator?: string;
  readonly enabled?: boolean;
  readonly separator?: true;
  readonly action?: MenuAction;
  readonly submenu?: readonly MenuItem[];
}

export interface MenuState {
  readonly platform: string;
  /** The window shortcut bindings in force (all null while one is being recorded). */
  readonly bindings: Readonly<Record<WindowShortcutId, string | null>>;
  /** The kind of window that has focus; `null` when none of the shell's windows does. */
  readonly focused: WindowType | null;
  readonly maxBindingLength: number;
  /** A binding is being recorded: the menu is inert (no role, no accelerator), so the key reaches the page. */
  readonly paused?: boolean;
}

const SEPARATOR: MenuItem = { separator: true };

function accelerator(binding: string | null | undefined, state: MenuState): Pick<MenuItem, "accelerator"> {
  if (!binding || state.paused === true) {
    return {};
  }
  const parsed = parseBinding(binding, state.maxBindingLength);
  return parsed ? { accelerator: toAccelerator(parsed, state.platform) } : {};
}

function fixed(name: keyof typeof FIXED_BINDINGS, state: MenuState): Pick<MenuItem, "accelerator"> {
  return accelerator(FIXED_BINDINGS[name], state);
}

/** Whether the document-level commands act in the focused window (a quick-entry or indicator window has none). */
function documentWindow(focused: WindowType | null): boolean {
  return focused === null || focused === "main" || focused === "popout";
}

export function buildMenu(state: MenuState): readonly MenuItem[] {
  if (state.paused === true) {
    // A role carries its own default accelerator (Command+C for "copy") which the platform would still honour, so while a
    // binding is recorded the menu is one inert entry: no role, no accelerator, nothing a key can reach.
    return [{ label: t("menuApp"), submenu: [{ label: t("shortcutRecording"), enabled: false }] }];
  }
  const mac = state.platform === "darwin";
  const documents = documentWindow(state.focused);
  const command = (label: string, id: WindowShortcutId, kind: ShellCommand["kind"], enabled: boolean): MenuItem => ({
    label,
    enabled,
    ...accelerator(state.bindings[id], state),
    action: { kind: "command", command: { kind } as ShellCommand },
  });
  const settings = command(t("menuSettings"), "open_settings", "open-settings", documents);
  const updates: MenuItem = { label: t("menuCheckForUpdates"), action: { kind: "check-for-updates" } };
  const newThread = command(t("menuNewThread"), "new_thread", "new-thread", documents);
  const popOut = command(t("menuPopOut"), "popout_thread", "popout-current-thread", state.focused === "main");
  const find = command(t("menuFind"), "find", "find", state.focused === "main");

  const app: MenuItem = {
    label: t("menuApp"),
    submenu: [{ role: "about", label: t("menuApp") }, SEPARATOR, settings, updates, SEPARATOR, { role: "services" }, SEPARATOR, { role: "hide", ...fixed("hide", state) }, { role: "hideOthers" }, { role: "unhide" }, SEPARATOR, { label: t("trayQuit"), ...fixed("quit", state), action: { kind: "quit" } }],
  };
  const file: MenuItem = {
    label: t("menuFile"),
    submenu: mac
      ? [newThread, popOut, SEPARATOR, { role: "close", ...fixed("close_window", state) }]
      : [newThread, popOut, SEPARATOR, settings, updates, SEPARATOR, { label: t("trayQuit"), ...fixed("quit", state), action: { kind: "quit" } }],
  };
  const edit: MenuItem = {
    label: t("menuEdit"),
    submenu: [
      { role: "undo", ...fixed("undo", state) },
      { role: "redo", ...fixed("redo", state) },
      SEPARATOR,
      { role: "cut", ...fixed("cut", state) },
      { role: "copy", ...fixed("copy", state) },
      { role: "paste", ...fixed("paste", state) },
      { role: "selectAll", ...fixed("select_all", state) },
      SEPARATOR,
      find,
    ],
  };
  const view: MenuItem = {
    label: t("menuView"),
    submenu: [
      { role: "zoomIn", label: t("menuZoomIn"), ...fixed("zoom_in", state) },
      { role: "zoomOut", label: t("menuZoomOut"), ...fixed("zoom_out", state) },
      { role: "resetZoom", label: t("menuZoomReset"), ...fixed("zoom_reset", state) },
      SEPARATOR,
      { role: "togglefullscreen" },
    ],
  };
  const window: MenuItem = {
    label: t("menuWindow"),
    submenu: mac ? [{ role: "minimize", ...fixed("minimize", state) }, { role: "zoom" }, SEPARATOR, { role: "front" }] : [{ role: "minimize", ...fixed("minimize", state) }, { role: "close", ...fixed("close_window", state) }],
  };
  const help: MenuItem = { label: t("menuHelp"), submenu: mac ? [] : [{ role: "about", label: t("menuApp") }] };
  return mac ? [app, file, edit, view, window, help] : [file, edit, view, window, help];
}

/** Every accelerator in a menu (the conflict check's view of what the menu uses). */
export function acceleratorsOf(items: readonly MenuItem[]): string[] {
  return items.flatMap((item) => [...(item.accelerator ? [item.accelerator] : []), ...acceleratorsOf(item.submenu ?? [])]);
}
