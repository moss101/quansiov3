/**
 * Quansio public API v1 client (APP-013).
 *
 * Canonical owner: `sdk/typescript`. Generated request/response types for the whole
 * surface are produced from `schemas/` (GOV-004); this module owns the public API
 * version constant and path builder that every generated call uses, so a client can
 * never address an unversioned or malformed path.
 */
export const PUBLIC_API_VERSION = "v1" as const;

const RESOURCE_PATTERN = /^[a-z0-9][a-z0-9/_-]{0,127}$/;

/** Build a public API path: `/v1/<resource>` with the resource validated. */
export function publicApiPath(resource: string): string {
  if (!RESOURCE_PATTERN.test(resource)) {
    throw new Error(`invalid public API resource: ${JSON.stringify(resource)}`);
  }
  return `/${PUBLIC_API_VERSION}/${resource}`;
}
