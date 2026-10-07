/**
 * Origins (ADP-009, DOMAIN.md §13.9 A).
 *
 * A saved login belongs to an origin: `scheme://host[:port]`, lowercase, no path, no credentials. A suggestion is offered for the
 * page's origin only; a fill into another origin needs the person's explicit confirmation. Matching is exact on scheme, host and
 * port: `https://example.com` and `https://example.com:8443` are different origins, `http` and `https` are different, and a
 * subdomain is not its parent — the browser's own rule, not a looser one.
 */

/** The canonical origin of a web URL, or `null` for anything that is not an `http` or `https` URL with a host. */
export function originOf(url: string): string | null {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
    return null;
  }
  if (parsed.hostname === "" || parsed.username !== "" || parsed.password !== "") {
    return null;
  }
  // `URL.origin` is already lowercase, punycoded and omits a default port.
  return parsed.origin;
}

/** Whether two URLs or origins are the same origin. A value that is not a web origin matches nothing, not even itself. */
export function sameOrigin(a: string, b: string): boolean {
  const left = originOf(a);
  const right = originOf(b);
  return left !== null && left === right;
}
