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

export { newCommandId } from "../client/ids.js";
