/**
 * Deep links (APP-003).
 *
 * `quansio://` URLs open the desktop on a thread, run, approval or artifact. A deep link is
 * data from outside the product (invariant 17): it is parsed into a typed intent with
 * validated identifiers and nothing else, never executed, and every intent is a navigation
 * the renderer performs through the ordinary projection/command surface.
 */

export const DEEP_LINK_SCHEME = "quansio";

export type DeepLinkIntent =
  | { readonly kind: "oauth-callback"; readonly code: string; readonly state: string }
  | { readonly kind: "open-thread"; readonly threadId: string }
  | { readonly kind: "open-run"; readonly runId: string }
  | { readonly kind: "open-approval"; readonly approvalId: string }
  | { readonly kind: "open-artifact"; readonly artifactId: string }
  | { readonly kind: "open-workspace"; readonly workspaceId: string };

const ID_PATTERNS: Record<string, { prefix: string; kind: DeepLinkIntent["kind"]; field: string }> = {
  thread: { prefix: "thr_", kind: "open-thread", field: "threadId" },
  run: { prefix: "run_", kind: "open-run", field: "runId" },
  approval: { prefix: "apr_", kind: "open-approval", field: "approvalId" },
  artifact: { prefix: "art_", kind: "open-artifact", field: "artifactId" },
  workspace: { prefix: "ws_", kind: "open-workspace", field: "workspaceId" },
};

const ULID_BODY = /^[0-9A-HJKMNP-TV-Z]{26}$/;

/** Parse a deep link; returns `null` for anything that is not exactly `quansio://<kind>/<id>`. */
export function parseDeepLink(raw: string): DeepLinkIntent | null {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  if (url.protocol !== `${DEEP_LINK_SCHEME}:` || url.username || url.password || url.hash) {
    return null;
  }
  if (url.hostname === "auth") {
    // quansio://auth/callback?code=…&state=… — the OAuth provider's return trip (APP-002).
    if (url.pathname !== "/callback") {
      return null;
    }
    const code = url.searchParams.get("code") ?? "";
    const state = url.searchParams.get("state") ?? "";
    if (!code || !state || [...url.searchParams.keys()].some((key) => key !== "code" && key !== "state")) {
      return null;
    }
    return { kind: "oauth-callback", code, state };
  }
  if (url.search) {
    return null;
  }
  const kind = url.hostname;
  const rule = ID_PATTERNS[kind];
  if (!rule) {
    return null;
  }
  const segments = url.pathname.split("/").filter(Boolean);
  if (segments.length !== 1) {
    return null;
  }
  const id = segments[0] ?? "";
  if (!id.startsWith(rule.prefix) || !ULID_BODY.test(id.slice(rule.prefix.length))) {
    return null;
  }
  return { kind: rule.kind, [rule.field]: id } as unknown as DeepLinkIntent;
}

/** Pick the deep link out of a second-instance argv (Windows/Linux) — the last quansio:// argument. */
export function deepLinkFromArgv(argv: readonly string[]): string | null {
  for (let index = argv.length - 1; index >= 0; index -= 1) {
    const argument = argv[index] ?? "";
    if (argument.startsWith(`${DEEP_LINK_SCHEME}://`)) {
      return argument;
    }
  }
  return null;
}
