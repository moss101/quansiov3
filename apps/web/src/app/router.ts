/**
 * Client-side routes (APP-012): `/app/<surface>[/<id>]`. The server serves `index.html`
 * for any of them (a route is never an asset), and the page reads the path.
 */

import { WEB_SURFACES, type WebSurfaceId } from "../surfaces.js";

export const APP_BASE = "/app";

export interface Route {
  readonly surface: WebSurfaceId;
  readonly id: string | null;
}

/** The route for a pathname; anything unknown is the work review. */
export function routeFor(pathname: string): Route {
  const rest = pathname.startsWith(APP_BASE) ? pathname.slice(APP_BASE.length) : pathname;
  const [surface = "", id = ""] = rest.split("/").filter((part) => part.length > 0);
  const known = WEB_SURFACES.find((item) => item.id === surface);
  return { surface: known ? known.id : "work-review", id: id.length > 0 ? id : null };
}

/** The pathname for a route. */
export function pathFor(route: Route): string {
  return route.id ? `${APP_BASE}/${route.surface}/${encodeURIComponent(route.id)}` : `${APP_BASE}/${route.surface}`;
}
