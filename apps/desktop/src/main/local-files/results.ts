/**
 * The shapes the local-file channels answer with (ADP-005, DOMAIN.md §13.7 F): a refusal is data, `{ok: false, error}`,
 * the shape the shell's channels already use, and carries a §15 code. None carries a path.
 */

/** The codes of §15 these modules answer with. */
export type LocalFileCode =
  | "FILE_GRANT_INVALID"
  | "FILE_OUTSIDE_ROOTS"
  | "FILE_EDIT_CONFLICT"
  | "FILE_REFUSED"
  | "VALIDATION_SCHEMA"
  | "POLICY_DENIED"
  | "NOT_FOUND"
  | "CONFLICT_STATE"
  | "RATE_LIMITED"
  | "INTERNAL";

export interface Refusal {
  readonly ok: false;
  readonly error: { readonly code: LocalFileCode; readonly message: string; readonly retryable: false; readonly details: Readonly<Record<string, unknown>> | null };
}

export interface Done<T> {
  readonly ok: true;
  readonly result: T;
}

export type Answer<T> = Done<T> | Refusal;

export function refuse(code: LocalFileCode, message: string, details: Readonly<Record<string, unknown>> | null = null): Refusal {
  return { ok: false, error: { code, message, retryable: false, details } };
}

export function done<T>(result: T): Done<T> {
  return { ok: true, result };
}

/** The rules a picked or listed file can break (`FILE_REFUSED`'s `details.reason`). */
export type RefusedReason = "not_regular" | "unreadable" | "symlink" | "too_large" | "too_many" | "total_too_large" | "too_many_entries" | "not_text" | "root_not_allowed" | "limit";

export function refusedFile(reason: RefusedReason, message: string, name?: string): Refusal {
  return refuse("FILE_REFUSED", message, name === undefined ? { reason } : { reason, name });
}

/** A name that is safe to show: the last element of a path, never a directory. */
export function displayName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] ?? "";
}

const MEDIA_TYPES: Readonly<Record<string, string>> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  avif: "image/avif",
  bmp: "image/bmp",
  ico: "image/x-icon",
  pdf: "application/pdf",
  txt: "text/plain",
  md: "text/markdown",
  csv: "text/csv",
  tsv: "text/tab-separated-values",
  json: "application/json",
  mp3: "audio/mpeg",
  wav: "audio/wav",
  ogg: "audio/ogg",
  m4a: "audio/mp4",
  flac: "audio/flac",
  mp4: "video/mp4",
  m4v: "video/mp4",
  webm: "video/webm",
  mov: "video/quicktime",
};

/** The media type a file name implies, or `application/octet-stream`. */
export function mediaTypeOf(name: string): string {
  const dot = name.lastIndexOf(".");
  const extension = dot === -1 ? "" : name.slice(dot + 1).toLowerCase();
  return MEDIA_TYPES[extension] ?? "application/octet-stream";
}

/**
 * The types a media URL serves as themselves: images (not SVG), audio, video, PDF and plain text. Every other type,
 * HTML and SVG included, is served as `application/octet-stream` (DOMAIN.md §13.7 C).
 */
export function servedMediaType(name: string): string {
  const type = mediaTypeOf(name);
  const served = /^(image\/(png|jpeg|gif|webp|avif|bmp|x-icon)|audio\/|video\/|application\/pdf$|text\/(plain|markdown|csv|tab-separated-values)$)/;
  return served.test(type) ? type : "application/octet-stream";
}
