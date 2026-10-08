/**
 * A value that has no rendering (ADP-009, DOMAIN.md §13.9).
 *
 * A password, a cookie value or a card number held in main is wrapped in a {@link Secret}. It prints as a redacted marker
 * (`String(secret)`, a template literal, `JSON.stringify`, `console.log` and `util.inspect` all say `[redacted]`), it can not be
 * enumerated or cloned into a log line by accident, and its bytes live in a `Buffer` that {@link Secret.wipe} overwrites. The only
 * way to the characters is {@link Secret.expose}, which names the act; the caller is then at the boundary that needed them (the
 * insertion into a page, the re-encryption into the vault) and nowhere else.
 *
 * JavaScript strings are immutable and may be copied by the engine, so this narrows exposure rather than eliminating it: what it
 * guarantees is that the secret never reaches a log, an IPC reply or a serialized record by being formatted into one, and that the
 * buffer a key or a value was read into is zeroed once it is done with.
 */

import { inspect } from "node:util";

const REDACTED = "[redacted]";

export class Secret {
  #bytes: Buffer;
  #wiped = false;

  constructor(value: string | Buffer) {
    this.#bytes = typeof value === "string" ? Buffer.from(value, "utf8") : Buffer.from(value);
  }

  /** The characters. The caller is at the boundary that needs them. */
  expose(): string {
    if (this.#wiped) {
      throw new Error("the secret was wiped");
    }
    return this.#bytes.toString("utf8");
  }

  /** The length in bytes (never the value). */
  get length(): number {
    return this.#wiped ? 0 : this.#bytes.length;
  }

  get wiped(): boolean {
    return this.#wiped;
  }

  /** Overwrite the bytes. Idempotent. */
  wipe(): void {
    this.#bytes.fill(0);
    this.#wiped = true;
  }

  toString(): string {
    return REDACTED;
  }

  toJSON(): string {
    return REDACTED;
  }

  [inspect.custom](): string {
    return REDACTED;
  }
}

/** Overwrite a buffer in place (a derived key, a keychain answer). Returns it so a caller can chain. */
export function zero(buffer: Buffer): Buffer {
  buffer.fill(0);
  return buffer;
}

/** Whether `buffer` holds only zero bytes: what a test asks of a key buffer after use. */
export function isZeroed(buffer: Buffer): boolean {
  return buffer.every((byte) => byte === 0);
}
