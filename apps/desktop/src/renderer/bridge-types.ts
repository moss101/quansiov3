/**
 * The renderer's view of the preload bridge (APP-003): `window.quansio` and nothing else.
 */

import type { PreloadApi } from "../preload/bridge.js";

declare global {
  interface Window {
    readonly quansio: PreloadApi;
  }
}

export type { PreloadApi };

/** ULID-shaped command ids minted in the renderer; the server dedupes on them. */
export function newCommandId(now: number = Date.now(), random: () => number = Math.random): string {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  const digit = (index: number): string => alphabet.charAt(index);
  let time = "";
  let remaining = now;
  for (let index = 0; index < 10; index += 1) {
    time = digit(remaining % 32) + time;
    remaining = Math.floor(remaining / 32);
  }
  let tail = "";
  for (let index = 0; index < 16; index += 1) {
    tail += digit(Math.floor(random() * 32));
  }
  return `cmd_${time}${tail}`;
}
