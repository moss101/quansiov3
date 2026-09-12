/**
 * Desktop shell entry barrel. Canonical owner: `apps/desktop` (APP-003, APP-004).
 * The Electron main/preload/renderer wiring is added by APP-003; this package
 * currently declares the surface contract only.
 */
export type { SurfaceDescriptor, SurfaceId } from "./surfaces.js";
export { DESKTOP_SURFACES, surface } from "./surfaces.js";
