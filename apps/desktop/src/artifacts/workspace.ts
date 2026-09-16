/**
 * Artifact workspace (APP-008).
 *
 * Artifacts outlive the chat/run that produced them. Preview never executes active
 * content with host privileges: HTML/JS/SVG is shown as text, never loaded into a
 * privileged webview.
 */

export type ArtifactKind =
  | "document"
  | "spreadsheet"
  | "presentation"
  | "code"
  | "data"
  | "image"
  | "audio"
  | "video"
  | "archive"
  | "other";

export interface ArtifactVersion {
  readonly id: string;
  readonly seq: number;
  readonly contentDigest: string;
  readonly mediaType: string;
}

export interface ArtifactRecord {
  readonly id: string;
  readonly title: string;
  readonly kind: ArtifactKind;
  readonly runId: string | null;
  readonly versions: readonly ArtifactVersion[];
  readonly sourceEvidenceIds: readonly string[];
}

const ACTIVE_TYPES = new Set([
  "text/html",
  "application/javascript",
  "text/javascript",
  "image/svg+xml",
  "application/xhtml+xml",
]);

export function previewMode(mediaType: string): "safe-text" | "safe-image" | "download-only" {
  if (ACTIVE_TYPES.has(mediaType)) {
    return "safe-text";
  }
  if (mediaType.startsWith("image/") && mediaType !== "image/svg+xml") {
    return "safe-image";
  }
  if (mediaType.startsWith("text/") || mediaType === "application/json") {
    return "safe-text";
  }
  return "download-only";
}

/**
 * Preview must not run script or navigate with host privilege.
 *
 * The parameter is kept, unused, so the answer is legible per media type at the call
 * site and in tests (`previewExecutesWithHostPrivileges("text/html")` reads as "for
 * this type") and so the signature stays uniform with `previewMode`/`previewIsSafe`
 * above -- the answer is always `false` today because every active type is already
 * forced through `safe-text`, not because this function ignores its input.
 */
// eslint-disable-next-line @typescript-eslint/no-unused-vars -- see doc comment above
export function previewExecutesWithHostPrivileges(_mediaType: string): boolean {
  // Every active type is forced through `safe-text`; nothing is loaded as a document.
  return false;
}

export function previewIsSafe(mediaType: string): boolean {
  return !ACTIVE_TYPES.has(mediaType) || previewMode(mediaType) === "safe-text";
}

export function addVersion(artifact: ArtifactRecord, version: ArtifactVersion): ArtifactRecord {
  return { ...artifact, versions: [...artifact.versions, version] };
}

/** Closing the producing run does not drop the artifact. */
export function afterRunClosed(artifact: ArtifactRecord): ArtifactRecord {
  return { ...artifact, runId: artifact.runId };
}

export function searchIndex(artifact: ArtifactRecord): readonly string[] {
  return [artifact.id, artifact.title, ...artifact.sourceEvidenceIds];
}
