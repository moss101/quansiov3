/**
 * Quit confirmation (ADP-004, DOMAIN.md §13.6 E).
 *
 * Quitting while a Turn is executing or an action is in flight asks first, in words that match what quitting does
 * here: the Run is not destroyed, it pauses and continues from the server when the app is opened again. The person
 * chooses to quit or to keep working. Idle quits silently. A status that cannot be read is not "idle": the person
 * is asked, with a note that Quansio could not check. Quitting is never silently allowed while work is known to run.
 *
 * `QuitGate` is the one place that decides; `app.ts` calls it from `before-quit` (which it cancels until the
 * answer is in) and from the last window closing on Windows and Linux.
 */

import type { WorkProbe, WorkStatus } from "./work-status.js";

export interface QuitGateOptions {
  readonly probe: WorkProbe;
  /** Ask the person; `true` means quit. Only called when work is running or unknown. */
  readonly ask: (status: Exclude<WorkStatus, { kind: "idle" }>) => Promise<boolean>;
}

export class QuitGate {
  readonly #options: QuitGateOptions;
  #permitted = false;
  #asking: Promise<boolean> | null = null;

  constructor(options: QuitGateOptions) {
    this.#options = options;
  }

  /** Whether a quit is already allowed (the person confirmed, or a flow that asked for itself said so). */
  get permitted(): boolean {
    return this.#permitted;
  }

  /** Allow the quit that is about to happen without asking again (an update that was already confirmed). */
  allow(): void {
    this.#permitted = true;
  }

  /** Withdraw a permission (the quit it was for did not happen). */
  revoke(): void {
    this.#permitted = false;
  }

  /**
   * Decide whether quitting may go ahead: `true` to quit. A second request while a question is open shares its
   * answer rather than stacking another dialog.
   */
  confirm(): Promise<boolean> {
    if (this.#permitted) {
      return Promise.resolve(true);
    }
    if (this.#asking) {
      return this.#asking;
    }
    const decision = (async (): Promise<boolean> => {
      let status: WorkStatus;
      try {
        status = await this.#options.probe();
      } catch {
        status = { kind: "unknown" };
      }
      if (status.kind === "idle") {
        this.#permitted = true;
        return true;
      }
      const quit = await this.#options.ask(status);
      this.#permitted = quit;
      return quit;
    })().finally(() => {
      this.#asking = null;
    });
    this.#asking = decision;
    return decision;
  }
}
