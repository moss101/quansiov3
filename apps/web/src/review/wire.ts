/** Small decoders shared by the page modules: a field is what it claims or nothing. */

export function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

export function number(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

export function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

export function strings(value: unknown): string[] {
  return list(value).filter((item): item is string => typeof item === "string");
}
