/**
 * Safe preview (APP-008, DOMAIN.md §10.1): preview never executes active content with
 * host privileges.
 *
 * The rule is structural, not a blocklist the renderer could forget: bytes are shown as
 * text (a DOM text node, never `innerHTML`, never a document), as a raster image decoded
 * in-process from a `data:` URL (the CSP allows `img-src data:` and nothing else), or not
 * at all (download only). HTML, SVG, XHTML, XML and JavaScript are text here, whatever
 * their media type says. "Open" hands a saved file to the OS only for media the OS will
 * not execute as a program or a page, and only after the user asked.
 */

export type PreviewMode = "text" | "image" | "download-only";

export interface PreviewPlan {
  readonly mode: PreviewMode;
  /** Why this mode was chosen; shown to the user for download-only. */
  readonly reason: string;
}

/** Largest text preview rendered in full; longer text is cut with a marker. */
export const MAX_TEXT_PREVIEW_CHARS = 200_000;
/** Largest image decoded in-process. */
export const MAX_IMAGE_PREVIEW_BYTES = 8 * 1024 * 1024;
/** Largest text fetched for preview at all. */
export const MAX_TEXT_PREVIEW_BYTES = 4 * 1024 * 1024;

/** Media that a browser or the OS would execute or render as a page/script. */
const ACTIVE_TYPES: ReadonlySet<string> = new Set([
  "text/html",
  "application/xhtml+xml",
  "image/svg+xml",
  "application/javascript",
  "application/x-javascript",
  "text/javascript",
  "application/ecmascript",
  "text/ecmascript",
  "application/xml",
  "text/xml",
  "application/xslt+xml",
  "application/mathml+xml",
  "application/vnd.mozilla.xul+xml",
  "application/pdf",
]);

/** Media the OS would run as a program or a shell script. */
const EXECUTABLE_TYPES: ReadonlySet<string> = new Set([
  "application/x-msdownload",
  "application/x-msdos-program",
  "application/x-executable",
  "application/x-mach-binary",
  "application/x-sh",
  "application/x-shellscript",
  "text/x-shellscript",
  "application/x-bat",
  "application/x-msi",
  "application/vnd.microsoft.portable-executable",
  "application/x-apple-diskimage",
  "application/java-archive",
  "application/x-python-code",
  "application/vnd.apple.installer+xml",
  "application/x-desktop",
]);

/** Raster images the renderer decodes itself; SVG is deliberately absent. */
const RASTER_TYPES: ReadonlySet<string> = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/bmp",
  "image/avif",
]);

const TEXT_TYPES: ReadonlySet<string> = new Set([
  "application/json",
  "application/x-ndjson",
  "application/yaml",
  "application/x-yaml",
  "application/toml",
  "application/sql",
  "application/x-sql",
  "application/csv",
]);

/** Normalize a media type: lower-case, no parameters. */
export function mediaKind(mediaType: string): string {
  return (mediaType.split(";")[0] ?? "").trim().toLowerCase();
}

/** Whether the media is active content (rendered/executed by a browser or the OS as a page). */
export function isActiveContent(mediaType: string): boolean {
  return ACTIVE_TYPES.has(mediaKind(mediaType));
}

/** Whether the OS would execute the file as a program. */
export function isExecutable(mediaType: string): boolean {
  return EXECUTABLE_TYPES.has(mediaKind(mediaType));
}

/** Whether the preview shows the bytes as text. */
export function isTextLike(mediaType: string): boolean {
  const kind = mediaKind(mediaType);
  return kind.startsWith("text/") || TEXT_TYPES.has(kind) || (isActiveContent(kind) && kind !== "application/pdf");
}

/** Decide how a version is previewed. The decision depends on media type and size only. */
export function previewPlan(mediaType: string, sizeBytes: number): PreviewPlan {
  const kind = mediaKind(mediaType);
  if (isExecutable(kind)) {
    return { mode: "download-only", reason: "executable content is never previewed or opened" };
  }
  if (isActiveContent(kind) && kind !== "application/pdf") {
    return sizeBytes <= MAX_TEXT_PREVIEW_BYTES
      ? { mode: "text", reason: "active content is shown as text, never rendered" }
      : { mode: "download-only", reason: "too large to show as text" };
  }
  if (RASTER_TYPES.has(kind)) {
    return sizeBytes <= MAX_IMAGE_PREVIEW_BYTES
      ? { mode: "image", reason: "raster image decoded in-process" }
      : { mode: "download-only", reason: "too large to decode in-process" };
  }
  if (isTextLike(kind)) {
    return sizeBytes <= MAX_TEXT_PREVIEW_BYTES
      ? { mode: "text", reason: "text" }
      : { mode: "download-only", reason: "too large to show as text" };
  }
  return { mode: "download-only", reason: `no in-process preview for ${kind || "unknown media"}` };
}

/** Whether "open with the OS" is offered for the media. Never for pages, scripts or programs. */
export function openAllowed(mediaType: string): boolean {
  const kind = mediaKind(mediaType);
  return kind !== "" && !isActiveContent(kind) && !isExecutable(kind) && kind !== "application/octet-stream";
}

/**
 * Preview must not run script or navigate with host privilege. The answer is structural:
 * every preview mode is a text node, an in-process raster decode or nothing.
 */
export function previewExecutesWithHostPrivileges(mediaType: string): boolean {
  return previewPlan(mediaType, 0).mode !== "text" && previewPlan(mediaType, 0).mode !== "image" && previewPlan(mediaType, 0).mode !== "download-only";
}

/** Legacy name kept for the journey spec: a preview of this media is safe by construction. */
export function previewIsSafe(mediaType: string): boolean {
  return !previewExecutesWithHostPrivileges(mediaType);
}

/** Decode bytes for the text mode: UTF-8 with replacement, bounded, marked when cut. */
export function textPreview(bytes: Uint8Array): { text: string; truncated: boolean } {
  const decoder = new TextDecoder("utf-8", { fatal: false });
  const decoded = decoder.decode(bytes);
  if (decoded.length <= MAX_TEXT_PREVIEW_CHARS) {
    return { text: decoded, truncated: false };
  }
  return { text: `${decoded.slice(0, MAX_TEXT_PREVIEW_CHARS)}\n…[truncated]`, truncated: true };
}

/** A `data:` URL for a raster image; refuses anything that is not a raster type. */
export function imageDataUrl(bytes: Uint8Array, mediaType: string): string {
  const kind = mediaKind(mediaType);
  if (!RASTER_TYPES.has(kind)) {
    throw new Error(`${kind || "unknown media"} is not a raster image`);
  }
  if (bytes.byteLength > MAX_IMAGE_PREVIEW_BYTES) {
    throw new Error("image too large to decode in-process");
  }
  return `data:${kind};base64,${toBase64(bytes)}`;
}

function toBase64(bytes: Uint8Array): string {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(bytes).toString("base64");
  }
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
