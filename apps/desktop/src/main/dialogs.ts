/**
 * The words and buttons of the two questions the shell asks (ADP-004): quitting and restarting for an update while
 * work may be running. They are built here, apart from Electron's `dialog`, so a test can hold them to what the
 * contract says: the wording tells the person the work is *paused* (a quit does not destroy a Run), the default
 * button is the safe one, closing the dialog is "do not interrupt", and an unreadable status says so.
 */

import { t } from "../i18n/strings.js";
import type { UpdateChoice } from "./update-gate.js";
import { describeWork, type WorkStatus } from "./work-status.js";

type Busy = Exclude<WorkStatus, { kind: "idle" }>;

export interface MessageBox {
  readonly title: string;
  readonly message: string;
  readonly detail: string;
  readonly buttons: readonly string[];
  /** The button Return chooses: always the one that interrupts nothing. */
  readonly defaultId: number;
  /** The button Escape (or closing the window) chooses: also interrupts nothing. */
  readonly cancelId: number;
}

export function quitDialog(status: Busy): MessageBox {
  return {
    title: t("quitTitle"),
    message: describeWork(status),
    detail: status.kind === "unknown" ? t("quitUnknownNote") : t("quitPauseNote"),
    buttons: [t("quitStay"), t("quitConfirm")],
    defaultId: 0,
    cancelId: 0,
  };
}

/** Whether the person chose to quit (the second button). */
export function quitChosen(response: number): boolean {
  return response === 1;
}

export function updateDialog(status: Busy): MessageBox {
  return {
    title: t("updateRestartTitle"),
    message: describeWork(status),
    detail: t("updateRestartNote"),
    buttons: [t("updateLater"), t("updateWhenIdle"), t("updateRestartAnyway")],
    defaultId: 0,
    cancelId: 0,
  };
}

export function updateChoiceOf(response: number): UpdateChoice {
  return response === 2 ? "install" : response === 1 ? "when_idle" : "later";
}
