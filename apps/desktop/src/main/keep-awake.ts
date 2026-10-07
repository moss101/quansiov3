/**
 * Keep-awake (ADP-004, DOMAIN.md §13.6 E).
 *
 * The machine is held awake only while all three are true: at least one Turn is active, the machine is on AC
 * power, and the person has not turned keep-awake off. The moment any of them stops being true the blocker is
 * released, so work on battery never drains the battery by itself and an idle app never holds the machine up.
 * The decision is {@link shouldHold}; {@link KeepAwake} applies it to a power-save blocker and holds at most one.
 */

/** The slice of Electron's `powerSaveBlocker` the shell uses. */
export interface PowerBlockerAdapter {
  start(): number;
  stop(id: number): void;
}

export interface KeepAwakeInputs {
  /** A Turn is executing (the activity list has a running item). */
  readonly running: boolean;
  readonly onAcPower: boolean;
  readonly enabled: boolean;
}

/** Whether the blocker should be held for these inputs. */
export function shouldHold(inputs: KeepAwakeInputs): boolean {
  return inputs.running && inputs.onAcPower && inputs.enabled;
}

export class KeepAwake {
  readonly #adapter: PowerBlockerAdapter;
  #held: number | null = null;
  #inputs: KeepAwakeInputs = { running: false, onAcPower: true, enabled: true };

  constructor(adapter: PowerBlockerAdapter) {
    this.#adapter = adapter;
  }

  /** Change some of the inputs and bring the blocker in line. */
  update(change: Partial<KeepAwakeInputs>): void {
    this.#inputs = { ...this.#inputs, ...change };
    const hold = shouldHold(this.#inputs);
    if (hold && this.#held === null) {
      this.#held = this.#adapter.start();
    } else if (!hold && this.#held !== null) {
      const id = this.#held;
      this.#held = null;
      this.#adapter.stop(id);
    }
  }

  /** Whether the machine is being held awake now. */
  get holding(): boolean {
    return this.#held !== null;
  }

  get inputs(): KeepAwakeInputs {
    return this.#inputs;
  }

  /** Release the blocker (quitting). */
  dispose(): void {
    this.update({ running: false });
  }
}
