/**
 * Responsive layout (APP-012): one app, three layouts, decided by the viewport width.
 *
 * Below {@link BREAKPOINTS.phone} the navigation is a drawer behind a menu button and
 * pages stack; up to {@link BREAKPOINTS.tablet} it is a rail of icons; above, a full
 * sidebar. The same breakpoints are in `app.css` as media queries, and the responsive
 * smoke test pins both.
 */

export const BREAKPOINTS = { phone: 720, tablet: 1080 } as const;

export type LayoutMode = "phone" | "tablet" | "desktop";
export type Navigation = "drawer" | "rail" | "sidebar";

export interface Layout {
  readonly mode: LayoutMode;
  readonly navigation: Navigation;
  /** Columns the review pages lay their cards in. */
  readonly columns: 1 | 2 | 3;
}

/** The layout for a viewport width in CSS pixels. */
export function layoutFor(width: number): Layout {
  if (!Number.isFinite(width) || width < BREAKPOINTS.phone) {
    return { mode: "phone", navigation: "drawer", columns: 1 };
  }
  if (width < BREAKPOINTS.tablet) {
    return { mode: "tablet", navigation: "rail", columns: 2 };
  }
  return { mode: "desktop", navigation: "sidebar", columns: 3 };
}

/** The media queries `app.css` must carry, derived from the same breakpoints. */
export function mediaQueries(): readonly string[] {
  return [`(max-width: ${String(BREAKPOINTS.phone - 1)}px)`, `(min-width: ${String(BREAKPOINTS.phone)}px) and (max-width: ${String(BREAKPOINTS.tablet - 1)}px)`, `(min-width: ${String(BREAKPOINTS.tablet)}px)`];
}
