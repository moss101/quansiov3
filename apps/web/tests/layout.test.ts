/**
 * APP-012 responsive smoke: the layout breakpoints, the stylesheet's media queries, the
 * page's viewport meta and the client routes.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { APP_BASE, BREAKPOINTS, layoutFor, mediaQueries, pathFor, routeFor } from "../src/index.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

describe("responsive layout", () => {
  it("picks a drawer, a rail or a sidebar by width, and the stylesheet carries the same breakpoints", () => {
    expect(layoutFor(375)).toEqual({ mode: "phone", navigation: "drawer", columns: 1 });
    expect(layoutFor(BREAKPOINTS.phone - 1)).toEqual({ mode: "phone", navigation: "drawer", columns: 1 });
    expect(layoutFor(BREAKPOINTS.phone)).toEqual({ mode: "tablet", navigation: "rail", columns: 2 });
    expect(layoutFor(1024)).toEqual({ mode: "tablet", navigation: "rail", columns: 2 });
    expect(layoutFor(BREAKPOINTS.tablet)).toEqual({ mode: "desktop", navigation: "sidebar", columns: 3 });
    expect(layoutFor(1920)).toEqual({ mode: "desktop", navigation: "sidebar", columns: 3 });
    expect(layoutFor(Number.NaN).mode).toBe("phone");
    const css = readFileSync(join(ROOT, "src", "app", "app.css"), "utf8");
    for (const query of mediaQueries()) {
      const [first = ""] = query.split(" and ");
      expect(css.includes(first), `${first} is in app.css`).toBe(true);
    }
    expect(css).toContain(`(max-width: ${String(BREAKPOINTS.phone - 1)}px)`);
    expect(css).toContain(`(min-width: ${String(BREAKPOINTS.phone)}px) and (max-width: ${String(BREAKPOINTS.tablet - 1)}px)`);
    const html = readFileSync(join(ROOT, "src", "app", "index.html"), "utf8");
    expect(html).toContain('name="viewport" content="width=device-width, initial-scale=1"');
    expect(html).not.toMatch(/<script[^>]*src="https?:/);
  });

  it("routes /app/<surface>[/<id>] and falls back to the work review", () => {
    expect(routeFor("/app/audit-and-privacy")).toEqual({ surface: "audit-and-privacy", id: null });
    expect(routeFor("/app/approvals/apr_1")).toEqual({ surface: "approvals", id: "apr_1" });
    expect(routeFor("/app/")).toEqual({ surface: "work-review", id: null });
    expect(routeFor("/app/nope")).toEqual({ surface: "work-review", id: null });
    expect(pathFor({ surface: "users", id: null })).toBe(`${APP_BASE}/users`);
    expect(pathFor({ surface: "approvals", id: "apr 1" })).toBe(`${APP_BASE}/approvals/apr%201`);
  });
});
