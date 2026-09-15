/** Web app barrel. Canonical owner: `apps/web` (APP-012). */
export type { WebSurfaceDescriptor, WebSurfaceId } from "./surfaces.js";
export { WEB_SURFACES, adminSurfaces } from "./surfaces.js";
export { canOpen, usesPublicContracts, PUBLIC_COMMAND_PREFIX, PUBLIC_ERROR_SHAPE } from "./rbac.js";
export type { ServerRole } from "./rbac.js";

