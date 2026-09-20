/**
 * Artifacts on the web (APP-012): the list and the version history as APP-008 projects
 * them. The web never renders artifact bytes as a document; it links to the content
 * projection, which the server serves as an attachment with `nosniff`.
 */

import { list, number, record, text } from "./wire.js";

export interface ArtifactView {
  readonly id: string;
  readonly kind: string;
  readonly title: string;
  readonly currentVersionId: string | null;
  readonly versionCount: number;
  readonly mediaType: string | null;
  readonly sizeBytes: number | null;
  readonly contentDigest: string | null;
  readonly updatedAt: string;
}

/** Decode `GET /v1/artifacts`. */
export function decodeArtifacts(payload: unknown): ArtifactView[] {
  return list(record(payload)["artifacts"]).map(record).flatMap((row) => {
    const id = text(row["id"]);
    return id
      ? [
          {
            id,
            kind: text(row["kind"]) ?? "",
            title: text(row["title"]) ?? id,
            currentVersionId: text(row["current_version_id"]),
            versionCount: number(row["version_count"]) ?? 0,
            mediaType: text(row["media_type"]),
            sizeBytes: number(row["size_bytes"]),
            contentDigest: text(row["content_digest"]),
            updatedAt: text(row["updated_at"]) ?? "",
          },
        ]
      : [];
  });
}

/** The content projection's path for a version: downloaded, never rendered inline. */
export function contentPath(artifactId: string, versionId: string): string {
  return `/v1/artifacts/${encodeURIComponent(artifactId)}/versions/${encodeURIComponent(versionId)}/content`;
}
